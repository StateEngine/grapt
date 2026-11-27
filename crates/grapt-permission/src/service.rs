//! Write-side Permission blocker service. Shares the blocker LAWS in
//! `grapt_types::blocker` with the Form service, not an implementation.
//!
//! Every transition decides legality AND appends its durable event + projects
//! the `permission_request` row inside ONE immediate transaction via
//! `EventStore::append_within` (a read that informs a write lives in the
//! write's transaction).
//!
//! Admission is TWO paths: `create` issues a durable request with NO live
//! waiter; `ask` issues it and structurally attaches the live
//! `RuntimeGeneration`. Settlement is THREE distinct authority paths, never a
//! branch in one generic reply: `reply` (ordinary Once/Reject, Reject
//! cascading over suspended siblings), `grant_session` (process-local grant),
//! and `grant_always` (a durable, project-scoped rule that REFUSES with zero
//! writes when no project resolves — never degrading to Once).

use crate::policy::{UnresolvedAction, UnresolvedApproval};
use grapt_store::{
    AggregateId, AppendInput, EventStore, GovernedTransaction, HeldWriters, ProjectionId,
    Projector, WriteError,
};
use grapt_types::PermissionReply;
use grapt_types::{
    AuthoritySource, CasOutcome, EventId, EventVersion, PermissionId, PermissionSuspensionReason,
    ProjectId, RuntimeGeneration, SessionId, Waiter,
};
use rusqlite::{OptionalExtension, Transaction};
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error(transparent)]
    Store(#[from] WriteError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("permission {id}: durable record corrupt: {reason}")]
    Corruption { id: PermissionId, reason: String },
    #[error("permission {id}: no live pending request to attach a waiter to")]
    NotLiveWaiter { id: PermissionId },
    #[error("permission action {action:?} is not a valid capability identity")]
    InvalidAction { action: String },
    #[error("event version 1 must exist")]
    Version,
}

/// Durable status of a permission request row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pending,
    Replied,
    Suspended(PermissionSuspensionReason),
    Cancelled,
}

impl Status {
    fn is_terminal(self) -> bool {
        matches!(self, Status::Replied | Status::Cancelled)
    }
}

/// The ordinary (non-authority) replies. `Session` and `Always` have their own
/// methods, so they cannot travel through `reply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrdinaryReply {
    Once,
    Reject,
}

impl OrdinaryReply {
    fn as_permission_reply(self) -> PermissionReply {
        match self {
            OrdinaryReply::Once => PermissionReply::Once,
            OrdinaryReply::Reject => PermissionReply::Reject,
        }
    }
}

/// The durable payload of a permission admission: the requested capability
/// action and the resources/authority carried verbatim into `permission.asked`.
/// Bundling these keeps `create`/`ask` to a handful of arguments and makes the
/// admission payload one value at every call site.
#[derive(Debug, Clone, Copy)]
pub struct PermissionRequest<'a> {
    pub action: &'a str,
    pub resources: &'a [String],
    pub authority: &'a AuthoritySource,
}

/// One cascade sibling swept terminal, for the receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CascadeSettled {
    pub request: PermissionId,
    pub had_suspension: PermissionSuspensionReason,
}

/// Outcome of an ordinary reply: the primary CAS result plus any suspended
/// siblings a Reject cascade settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyOutcome {
    pub primary: CasOutcome,
    pub cascaded: Vec<CascadeSettled>,
}

fn reason_str(r: PermissionSuspensionReason) -> &'static str {
    match r {
        PermissionSuspensionReason::Shutdown => "shutdown",
        PermissionSuspensionReason::ProcessLoss => "process-loss",
    }
}

fn reason_from_str(raw: &str) -> Option<PermissionSuspensionReason> {
    match raw {
        "shutdown" => Some(PermissionSuspensionReason::Shutdown),
        "process-loss" => Some(PermissionSuspensionReason::ProcessLoss),
        _ => None,
    }
}

/// The `metadata.authority.layer` string the oracle writes on an asked event.
fn authority_layer(source: &AuthoritySource) -> String {
    // Adapted to the corrected `AuthoritySource` vocabulary. The oracle writes
    // one string here, so each new variant maps onto the string the oracle
    // already produces for that decision; the exact policy version the new
    // variants carry is recorded by the settlement receipt, not by this field.
    match source {
        AuthoritySource::DefaultPolicy { .. } => "fallback".to_owned(),
        AuthoritySource::DirectReply => "direct-reply".to_owned(),
        AuthoritySource::SavedRule { .. } => "saved-rule".to_owned(),
        AuthoritySource::Hook { hook } => hook.as_str().to_owned(),
        AuthoritySource::PolicyLayer { layer } => layer.as_str().to_owned(),
    }
}

/// Run a transition UPDATE and require it to affect EXACTLY one row (review G):
/// zero rows means a cross-session or unknown target (a silent no-op); more
/// than one is impossible on a PK but is rejected defensively.
fn affect_one(
    tx: &Transaction<'_>,
    sql: &str,
    params: impl rusqlite::Params,
    request: &PermissionId,
) -> Result<(), String> {
    let n = tx.execute(sql, params).map_err(|e| e.to_string())?;
    if n != 1 {
        return Err(format!(
            "permission {request}: transition affected {n} rows (expected exactly 1; wrong session or unknown request)"
        ));
    }
    Ok(())
}

fn encode_projection_value(value: Option<&Value>) -> Result<Option<String>, String> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| error.to_string())
}

fn project_http_asked(
    tx: &Transaction<'_>,
    id: &PermissionId,
    coordinates: &crate::event::PermissionAskedCoordinates,
) -> Result<(), String> {
    let resources = serde_json::to_string(&coordinates.resources).map_err(|e| e.to_string())?;
    let save = coordinates
        .save
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| e.to_string())?;
    let metadata = encode_projection_value(coordinates.metadata.as_ref())?;
    let source = encode_projection_value(coordinates.source.as_ref())?;
    tx.execute(
        "INSERT INTO permission_http_request
         (id, agent_id, project_id, resources_json, save_json, metadata_json, source_json,
          owner_subject, owner_client)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            id.as_str(),
            coordinates.agent.as_str(),
            coordinates.project.as_str(),
            resources,
            save,
            metadata,
            source,
            coordinates.owner_subject,
            coordinates.owner_client.as_str()
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn project_always_rule(
    tx: &Transaction<'_>,
    request: &PermissionId,
    created_ms: i64,
) -> Result<(), String> {
    let row = tx
        .query_row(
            "SELECT h.project_id, p.action, h.save_json
             FROM permission_http_request h
             JOIN permission_request p ON p.id = h.id
             WHERE h.id = ?1",
            [request.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((project, action, Some(raw_patterns))) = row else {
        return Ok(());
    };
    let patterns: Vec<String> = serde_json::from_str(&raw_patterns).map_err(|e| e.to_string())?;
    if patterns.len() > crate::service::http::MAX_HTTP_PERMISSION_RESOURCES {
        return Err(format!(
            "permission {request}: saved resources exceed the replay bound"
        ));
    }
    for pattern in patterns {
        tx.execute(
            "INSERT INTO permission_http_rule_origin(project_id,action,resource,permission_id)
             SELECT ?1,?2,?3,?4 WHERE NOT EXISTS (
                 SELECT 1 FROM permission_http_rule_revocation
                 WHERE permission_id=?4 AND project_id=?1 AND action=?2 AND resource=?3)
             ON CONFLICT DO NOTHING",
            rusqlite::params![project, action, pattern, request.as_str()],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO permission_http_rule (project_id, action, resource, time_created)
             SELECT ?1, ?2, ?3, ?4 WHERE NOT EXISTS (
                 SELECT 1 FROM permission_http_rule_revocation
                 WHERE permission_id=?5 AND project_id=?1 AND action=?2 AND resource=?3)
             ON CONFLICT(project_id, action, resource) DO NOTHING",
            rusqlite::params![project, action, pattern, created_ms, request.as_str()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Projects permission decisions and saved-rule withdrawals. The service
/// decides legality BEFORE the append; the projector applies the already-legal
/// transition and enforces session-scoped, exactly-one-row writes.
struct PermissionProjector;

impl Projector for PermissionProjector {
    fn projection(&self) -> ProjectionId {
        ProjectionId::new("permission")
    }

    fn project(
        &mut self,
        tx: &Transaction<'_>,
        event: &grapt_store::DecodedEvent,
    ) -> Result<(), grapt_store::ProjectorError> {
        let decoded = crate::PermissionEvent::decode(
            event.kind.name(),
            event.kind.version().value(),
            &event.data,
        )
        .map_err(|e| e.to_string())?;
        match decoded {
            Some(crate::PermissionEvent::RuleRevoked(revocation)) => {
                if event.aggregate_id != revocation.session {
                    return Err("permission withdrawal names another session".into());
                }
                http::revocation::project(tx, &revocation)?;
            }
            Some(crate::PermissionEvent::Asked {
                id,
                session,
                action,
                coordinates,
            }) => {
                tx.execute(
                    "INSERT INTO permission_request
                     (id, session_id, action, status, suspended_reason, reply)
                     VALUES (?1, ?2, ?3, 'pending', NULL, NULL)",
                    rusqlite::params![id.as_str(), session.as_str(), action],
                )
                .map_err(|e| e.to_string())?;
                if let Some(coordinates) = coordinates {
                    project_http_asked(tx, &id, &coordinates)?;
                }
            }
            Some(crate::PermissionEvent::Replied {
                request,
                session,
                reply,
                ..
            }) => {
                affect_one(
                    tx,
                    "UPDATE permission_request SET status = 'replied', reply = ?3
                     WHERE id = ?1 AND session_id = ?2",
                    rusqlite::params![
                        request.as_str(),
                        session.as_str(),
                        crate::event::reply_as_str(reply)
                    ],
                    &request,
                )?;
                if reply == PermissionReply::Always {
                    project_always_rule(tx, &request, event.created_ms)?;
                }
            }
            Some(crate::PermissionEvent::Suspended {
                request,
                session,
                reason,
            }) => {
                affect_one(
                    tx,
                    "UPDATE permission_request SET status = 'suspended', suspended_reason = ?3
                     WHERE id = ?1 AND session_id = ?2",
                    rusqlite::params![request.as_str(), session.as_str(), reason_str(reason)],
                    &request,
                )?;
            }
            Some(crate::PermissionEvent::Cancelled {
                request, session, ..
            }) => {
                affect_one(
                    tx,
                    "UPDATE permission_request SET status = 'cancelled'
                     WHERE id = ?1 AND session_id = ?2",
                    rusqlite::params![request.as_str(), session.as_str()],
                    &request,
                )?;
            }
            None => {}
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct PermissionService {
    store: EventStore,
    /// `Session` approvals live ONLY here — process-local, never persisted,
    /// never serialized, and never queryable from outside this type. They are a
    /// NON-AUTHORITY record that a Session reply covered an action; see
    /// `crate::policy` for why this crate cannot turn one into a grant.
    session_approvals: Vec<(SessionId, UnresolvedApproval)>,
    /// Live waiters are process-local projection state, independent of durable
    /// blocker state. `create` leaves none; `ask` attaches one.
    waiters: Vec<(PermissionId, RuntimeGeneration)>,
}

struct Row {
    session: String,
    action: String,
    status: Status,
}

include!("service/operations.rs");

mod http;
pub use http::{
    DecisionIds, PermissionConsequence, PermissionCreateEffect, PermissionHttpError,
    PermissionHttpLifecycle, PermissionHttpPage, PermissionHttpReply, PermissionHttpRequest,
    PermissionHttpSnapshot, PermissionHttpSource, PermissionHttpState, PermissionOwner,
    PermissionReplyCommand, PermissionReplyEffect, PermissionRevokeRuleCommand,
    PermissionRuleWithdrawal, SavedPermissionRule, SessionPermissionRule,
};

#[cfg(test)]
mod projector_guard {
    use super::*;
    use grapt_store::{AppendInput, EventStore};
    use serde_json::json;

    // Review G negative witness: a promoted event whose session does not match
    // the stored row is rejected by the projector's exactly-one-row guard.
    #[test]
    fn cross_session_reply_event_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let owner: SessionId = "ses_owner".parse()?;
        // The owning session is elected BEFORE the service is built. Election is
        // explicit by contract — no append path elects on a caller's behalf — so
        // the admission below would be refused by the fence rather than reaching
        // the projector law this test exists to prove.
        let mut store = EventStore::open_in_memory()?;
        store.elect_writer(&AggregateId::Session(owner.clone()))?;
        let mut svc = PermissionService::new(store)?;
        let id: PermissionId = "per_guard".parse()?;
        let evt: EventId = "evt_ask".parse()?;
        let request = PermissionRequest {
            action: "read",
            resources: &[],
            authority: &AuthoritySource::DefaultPolicy {
                policy: match grapt_types::PolicyVersion::new("default") {
                    Some(policy) => policy,
                    None => unreachable!("non-empty policy version"),
                },
            },
        };
        svc.create(&evt, &id, &owner, &request, 0)?;

        let intruder: SessionId = "ses_intruder".parse()?;
        let data = json!({
            "sessionID": intruder.as_str(),
            "requestID": id.as_str(),
            "reply": "once",
        });
        let evt2: EventId = "evt_forge".parse()?;
        let version = EventVersion::new(1).ok_or("v1")?;
        // The intruder aggregate is ELECTED on purpose. Without this the append
        // would be refused by the writer fence, and the test would pass without
        // ever reaching the projector law it exists to prove.
        let aggregate = AggregateId::Session(intruder.clone());
        svc.store.elect_writer(&aggregate)?;
        let held = svc.store.held_writers();
        let tx = svc.store.governed_transaction()?;
        let result = EventStore::append_within(
            &tx,
            AppendInput {
                id: &evt2,
                aggregate: aggregate.clone(),
                writer: held.token(&aggregate)?,
                kind_name: "permission.replied",
                version,
                created_ms: 0,
                data: &data,
            },
            &mut [&mut PermissionProjector],
        );
        assert!(
            matches!(result, Err(WriteError::Projector { .. })),
            "the PROJECTOR must refuse a cross-session settlement, got {result:?}"
        );
        drop(tx);
        Ok(())
    }
}
