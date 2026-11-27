//! Borrowed-store product boundary for the versioned permission HTTP surface.
//!
//! The shipped product already owns the sole `EventStore` inside its inbox.
//! This boundary therefore borrows that store for one synchronous operation;
//! it never opens a second connection and never holds a database transaction
//! across an await.

use super::{PermissionError, PermissionProjector, Status};
use crate::event::{reply_as_str, PermissionReplySource};
use grapt_store::{AggregateId, AppendInput, EventLog, EventStore, Projector};
use grapt_types::{
    AgentId, AuthenticatedClientId, CapabilityId, CasOutcome, DecisionNonce, EventId, EventVersion,
    PermissionId, PermissionReply, PermissionSuspensionReason, ProjectId, RequestId, SessionId,
};
use serde_json::{json, Value};

mod consequences;
mod listing;
mod msys;
pub(super) mod revocation;
mod snapshot;
pub use consequences::PermissionConsequence;
pub use revocation::{
    PermissionRevokeRuleCommand, PermissionRuleWithdrawal, SessionPermissionRule,
};
pub use snapshot::PermissionHttpSnapshot;

pub const MAX_HTTP_PERMISSION_ROWS: usize = 128;
pub const MAX_HTTP_SESSION_APPROVALS: usize = 128;
pub const MAX_HTTP_PERMISSION_RESOURCES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionHttpSource {
    Tool { message_id: String, call_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionHttpLifecycle {
    Pending,
    Suspended(PermissionSuspensionReason),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionHttpRequest {
    pub id: PermissionId,
    pub session: SessionId,
    pub agent: AgentId,
    pub action: String,
    pub resources: Vec<String>,
    pub save: Option<Vec<String>>,
    pub metadata: Option<Value>,
    pub source: Option<PermissionHttpSource>,
    pub lifecycle: PermissionHttpLifecycle,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionHttpPage {
    pub requests: Vec<PermissionHttpRequest>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionCreateEffect {
    Ask,
    Allow,
    /// A narrowing authority (the Session's preset, the owner floor, or
    /// configuration) refuses the action; no approval can lift it.
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionHttpReply {
    Once,
    Session,
    Always,
    Reject,
}

impl PermissionHttpReply {
    fn canonical(self) -> PermissionReply {
        match self {
            Self::Once => PermissionReply::Once,
            Self::Session => PermissionReply::Session,
            Self::Always => PermissionReply::Always,
            Self::Reject => PermissionReply::Reject,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PermissionOwner<'a> {
    pub subject: &'a str,
    pub client: &'a AuthenticatedClientId,
}

#[derive(Debug, Clone)]
pub struct DecisionIds {
    pub event: EventId,
    pub request: RequestId,
    pub nonce: DecisionNonce,
}

#[derive(Debug, Clone)]
pub struct PermissionReplyCommand<'a> {
    pub owner: PermissionOwner<'a>,
    pub session: &'a SessionId,
    pub id: &'a PermissionId,
    pub reply: PermissionHttpReply,
    pub project: Option<&'a ProjectId>,
    pub created_ms: i64,
    pub decision: DecisionIds,
}

#[derive(Debug)]
pub struct PermissionReplyEffect {
    outcome: CasOutcome,
    committed: Option<crate::tool_authority::CommittedPermissionReply>,
}

impl PermissionReplyEffect {
    pub fn outcome(&self) -> CasOutcome {
        self.outcome
    }

    pub fn take_committed(&mut self) -> Option<crate::tool_authority::CommittedPermissionReply> {
        self.committed.take()
    }
}

impl PartialEq<CasOutcome> for PermissionReplyEffect {
    fn eq(&self, other: &CasOutcome) -> bool {
        self.outcome == *other
    }
}

impl PartialEq<PermissionReplyEffect> for CasOutcome {
    fn eq(&self, other: &PermissionReplyEffect) -> bool {
        *self == other.outcome
    }
}

#[derive(Debug, Default)]
pub struct PermissionHttpState {
    session_approvals: Vec<(SessionId, String, Vec<String>)>,
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionHttpError {
    #[error("approval unavailable (headless): no matching approver is attached; no effect ran")]
    ApprovalUnavailable,
    #[error(transparent)]
    Domain(#[from] PermissionError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Store(#[from] grapt_store::WriteError),
    #[error("permission {0} was not found")]
    NotFound(PermissionId),
    #[error("permission {0} belongs to another authority principal")]
    WrongPrincipal(PermissionId),
    #[error("permission {0} belongs to another authenticated client")]
    WrongClient(PermissionId),
    #[error("permission {id} is not live ({reason:?})")]
    NotLive {
        id: PermissionId,
        reason: PermissionSuspensionReason,
    },
    #[error("permission session approval bound {max} is exhausted")]
    ApprovalBound { max: usize },
    #[error("permission resource bound {max} is exceeded")]
    ResourceBound { max: usize },
    #[error("permission decision identity could not be minted: {0}")]
    Identity(String),
    #[error("an always permission reply requires a project")]
    ProjectRequired,
    #[error("permission {id}: adjunct record corrupt: {reason}")]
    Corruption { id: PermissionId, reason: String },
    #[error("permission event replay refused: {0}")]
    Replay(String),
    #[error(transparent)]
    ToolAuthority(#[from] crate::tool_authority::ToolAuthorizationError),
    #[error("{action} is denied by the {layer} permission policy")]
    Denied { action: String, layer: &'static str },
}

impl PermissionHttpState {
    /// Cancel only this authenticated Session's pending decision through the
    /// same canonical transition as PermissionService::cancel.
    #[allow(
        clippy::too_many_arguments,
        reason = "borrows the canonical store with exact owner and event coordinates"
    )]
    pub fn cancel_tool(
        &mut self,
        store: &mut EventStore,
        owner: &PermissionOwner<'_>,
        session: &SessionId,
        id: &PermissionId,
        event: &EventId,
        created_ms: i64,
    ) -> Result<Option<crate::CommittedPermissionCancellation>, PermissionHttpError> {
        self.cancel_tool_because(
            store,
            owner,
            session,
            id,
            event,
            created_ms,
            crate::CancelReason::Interrupted,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn cancel_tool_because(
        &mut self,
        store: &mut EventStore,
        owner: &PermissionOwner<'_>,
        session: &SessionId,
        id: &PermissionId,
        event: &EventId,
        created_ms: i64,
        reason: crate::CancelReason,
    ) -> Result<Option<crate::CommittedPermissionCancellation>, PermissionHttpError> {
        self.verify_owner(store, owner, session, id)?;
        let outcome = super::PermissionService::cancel_in_store_because(
            store, event, id, created_ms, reason,
        )?;
        Ok((outcome == CasOutcome::Applied).then(|| {
            crate::CommittedPermissionCancellation::new(id.clone(), session.clone(), reason)
        }))
    }

    /// Bounded saved-policy inventory from the same projection used for admission.
    pub fn saved_rules(
        store: &EventStore,
        project: Option<&ProjectId>,
    ) -> Result<Vec<SavedPermissionRule>, PermissionHttpError> {
        let mut query = store.connection().prepare(
            "SELECT CASE WHEN length(CAST(project_id AS BLOB)) <= 512 THEN project_id END,
                    CASE WHEN length(CAST(action AS BLOB)) BETWEEN 1 AND 1024 THEN action END,
                    CASE WHEN length(CAST(resource AS BLOB)) <= 8192 THEN resource END
             FROM permission_http_rule WHERE (?1 IS NULL OR project_id = ?1)
             ORDER BY time_created, project_id, action, resource LIMIT 129",
        )?;
        let mut rows = query.query([project.map(ProjectId::as_str)])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            if result.len() == 128 {
                return Err(PermissionHttpError::ResourceBound { max: 128 });
            }
            let project: String = row.get(0)?;
            let project = ProjectId::new(project).ok_or_else(|| {
                PermissionHttpError::Replay("saved rule project id is invalid".to_owned())
            })?;
            result.push(SavedPermissionRule {
                project,
                action: row.get(1)?,
                resource: row.get(2)?,
            });
        }
        Ok(result)
    }

    pub fn initialize(store: &mut EventStore) -> Result<(), PermissionHttpError> {
        store.connection().prepare(
            "SELECT id, session_id, action, status, suspended_reason, reply
             FROM permission_request LIMIT 0",
        )?;
        store.connection().prepare(
            "SELECT project_id, action, resource, time_created
             FROM permission_http_rule LIMIT 0",
        )?;
        store.connection().prepare(
            "SELECT project_id,action,resource,permission_id FROM permission_http_rule_origin LIMIT 0",
        )?;
        store.connection().prepare(
            "SELECT permission_id,project_id,action,resource FROM permission_http_rule_revocation LIMIT 0",
        )?;
        store.connection().prepare(
            "SELECT id, agent_id, project_id, resources_json, save_json, metadata_json, source_json,
                    owner_subject, owner_client
             FROM permission_http_request LIMIT 0",
        )?;
        Ok(())
    }

    /// Rebuild every permission projection in `target` from canonical event
    /// facts in `source`. The destination may be a brand-new database: no row
    /// is read from the source projections, and all permission projection tables are
    /// cleared inside one transaction before the strict replay fold begins.
    pub fn rebuild_from_event_log(
        target: &mut EventStore,
        source: &EventLog,
    ) -> Result<u64, PermissionHttpError> {
        Self::initialize(target)?;
        let tx = target.governed_transaction()?;
        tx.execute("DELETE FROM permission_http_request", [])?;
        tx.execute("DELETE FROM permission_http_rule", [])?;
        tx.execute("DELETE FROM permission_http_rule_revocation", [])?;
        tx.execute("DELETE FROM permission_http_rule_origin", [])?;
        tx.execute("DELETE FROM permission_request", [])?;
        let mut projector = PermissionProjector;
        let mut projection_failure = None;
        let events = source
            .for_each_in_replay_order(|event| {
                if projection_failure.is_none() {
                    if let Err(error) = projector.project(&tx, event) {
                        projection_failure = Some(error);
                    }
                }
            })
            .map_err(|error| PermissionHttpError::Replay(error.to_string()))?;
        if let Some(error) = projection_failure {
            return Err(PermissionHttpError::Replay(error.to_string()));
        }
        tx.commit()?.discard();
        Ok(events)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &mut self,
        store: &mut EventStore,
        owner: &PermissionOwner<'_>,
        event: &EventId,
        id: &PermissionId,
        session: &SessionId,
        agent: &AgentId,
        layers: &[crate::preset::Layer],
        project: &ProjectId,
        action: &str,
        resources: &[String],
        save: Option<&[String]>,
        metadata: Option<&Value>,
        source: Option<&PermissionHttpSource>,
        created_ms: i64,
    ) -> Result<PermissionCreateEffect, PermissionHttpError> {
        let effect =
            self.evaluate_create(store, session, layers, project, action, resources, save)?;
        if effect == PermissionCreateEffect::Ask {
            Self::commit_ask(
                store, owner, event, id, session, agent, project, action, resources, save,
                metadata, source, created_ms,
            )?;
        }
        Ok(effect)
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_create(
        &self,
        store: &EventStore,
        session: &SessionId,
        layers: &[crate::preset::Layer],
        project: &ProjectId,
        action: &str,
        resources: &[String],
        save: Option<&[String]>,
    ) -> Result<PermissionCreateEffect, PermissionHttpError> {
        if action.is_empty() {
            return Err(PermissionError::InvalidAction {
                action: action.to_owned(),
            }
            .into());
        }
        if resources.len() > MAX_HTTP_PERMISSION_RESOURCES
            || save.is_some_and(|patterns| patterns.len() > MAX_HTTP_PERMISSION_RESOURCES)
        {
            return Err(PermissionHttpError::ResourceBound {
                max: MAX_HTTP_PERMISSION_RESOURCES,
            });
        }
        // The narrowing authorities decide first, so a deny never reads
        // the approvals store and a broken store cannot break a denial.
        let refs: Vec<&str> = resources.iter().map(String::as_str).collect();
        // Only an explicit recorded grant for this action may remove OS
        // containment. Bypass and ordinary static command allows are not grants.
        let msys_layers = (action == crate::SANDBOX_MSYS_ACTION).then(|| {
            layers
                .iter()
                .filter(|layer| layer.name != crate::preset::LayerName::Bypass)
                .cloned()
                .collect::<Vec<_>>()
        });
        let narrowed =
            crate::preset::decide(action, &refs, msys_layers.as_deref().unwrap_or(layers));
        if narrowed.effect == crate::preset::Effect::Deny {
            return Ok(PermissionCreateEffect::Deny);
        }
        let mut approved: Vec<String> = self
            .session_approvals
            .iter()
            .filter(|(held_session, held_action, _)| {
                held_session == session && held_action == action
            })
            .flat_map(|(_, _, patterns)| patterns.iter().cloned())
            .collect();
        if action != crate::SANDBOX_MSYS_ACTION
            || !resources
                .iter()
                .any(|value| value == crate::SANDBOX_MSYS_SESSION)
        {
            approved.extend(saved_rule_patterns(store, project, action)?);
        }
        if action == crate::SANDBOX_MSYS_ACTION {
            return Ok(
                if !resources.is_empty()
                    && resources.iter().all(|resource| approved.contains(resource))
                {
                    PermissionCreateEffect::Allow
                } else {
                    PermissionCreateEffect::Ask
                },
            );
        }
        let mut stack = layers.to_vec();
        stack.push(crate::preset::Layer::approvals(action, &approved));
        Ok(match crate::preset::decide(action, &refs, &stack).effect {
            crate::preset::Effect::Allow => PermissionCreateEffect::Allow,
            crate::preset::Effect::Ask => PermissionCreateEffect::Ask,
            crate::preset::Effect::Deny => PermissionCreateEffect::Deny,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_ask(
        store: &mut EventStore,
        owner: &PermissionOwner<'_>,
        event: &EventId,
        id: &PermissionId,
        session: &SessionId,
        agent: &AgentId,
        project: &ProjectId,
        action: &str,
        resources: &[String],
        save: Option<&[String]>,
        metadata: Option<&Value>,
        source: Option<&PermissionHttpSource>,
        created_ms: i64,
    ) -> Result<(), PermissionHttpError> {
        let held = store.held_writers();
        let tx = store.governed_transaction()?;
        let aggregate = AggregateId::Session(session.clone());
        let writer = held.token(&aggregate)?;
        let version = EventVersion::new(2).ok_or(PermissionError::Version)?;
        let data = json!({
            "id": id.as_str(),
            "sessionID": session.as_str(),
            "agentID": agent.as_str(),
            "projectID": project.as_str(),
            "action": action,
            "resources": resources,
            "save": save,
            "metadata": metadata,
            "source": source.map(source_json),
            "ownerSubject": owner.subject,
            "ownerClient": owner.client.as_str(),
        });
        EventStore::append_within(
            &tx,
            AppendInput {
                id: event,
                aggregate,
                writer,
                kind_name: "permission.asked",
                version,
                created_ms,
                data: &data,
            },
            &mut [&mut PermissionProjector],
        )?;
        tx.commit()?.discard();
        Ok(())
    }

    /// Evaluate the same durable permission policy as [`Self::create`] and
    /// bind its sealed decision directly to one pending tool effect.
    ///
    /// Callers receive only the consume-and-run boundary. They never receive
    /// a lease and cannot turn the public display disposition into authority.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_tool(
        &mut self,
        store: &mut EventStore,
        authority: &crate::tool_authority::ToolPermissionAuthority,
        capability: CapabilityId,
        owner: &PermissionOwner<'_>,
        event: &EventId,
        id: &PermissionId,
        session: &SessionId,
        agent: &AgentId,
        layers: &[crate::preset::Layer],
        project: &ProjectId,
        action: &str,
        resources: &[String],
        save: Option<&[String]>,
        metadata: Option<&Value>,
        source: Option<&PermissionHttpSource>,
        created_ms: i64,
        consequences: Option<&[PermissionConsequence]>,
        approver_attached: bool,
    ) -> Result<crate::tool_authority::PendingToolEffect, PermissionHttpError> {
        let (effect, policy_metadata) = self.evaluate_tool_consequences(
            store,
            session,
            layers,
            project,
            action,
            resources,
            save,
            metadata,
            consequences,
        )?;
        if effect == PermissionCreateEffect::Deny {
            let refs: Vec<&str> = resources.iter().map(String::as_str).collect();
            return Err(PermissionHttpError::Denied {
                action: action.to_owned(),
                layer: crate::preset::decide(action, &refs, layers)
                    .layer
                    .map_or("permission", crate::preset::LayerName::label),
            });
        }
        if effect == PermissionCreateEffect::Allow {
            return authority
                .begin(effect, id.clone(), session.clone(), capability)
                .map_err(PermissionHttpError::from);
        }
        // Copyright 2025 OpenAI. Apache-2.0.
        // Headless no-wait behavior.
        // Modified by Grapt: retain resolved Allow/Deny instead of changing presets.
        if !approver_attached {
            return Err(PermissionHttpError::ApprovalUnavailable);
        }
        let reservation = authority.reserve(id.clone(), session.clone(), capability)?;
        Self::commit_ask(
            store,
            owner,
            event,
            id,
            session,
            agent,
            project,
            action,
            resources,
            save,
            Some(&policy_metadata),
            source,
            created_ms,
        )?;
        reservation.commit().map_err(PermissionHttpError::from)
    }

    pub fn list(
        &self,
        store: &EventStore,
        owner: &PermissionOwner<'_>,
        session: Option<&SessionId>,
    ) -> Result<PermissionHttpPage, PermissionHttpError> {
        Self::list_from(store, owner, session)
    }

    pub fn get(
        &self,
        store: &EventStore,
        owner: &PermissionOwner<'_>,
        session: &SessionId,
        id: &PermissionId,
    ) -> Result<PermissionHttpRequest, PermissionHttpError> {
        self.verify_owner(store, owner, session, id)?;
        let raw = store.connection().query_row(
            "SELECT p.id, p.session_id, p.action, p.status, p.suspended_reason,
                    h.agent_id, h.resources_json, h.save_json, h.metadata_json, h.source_json
             FROM permission_request p JOIN permission_http_request h ON h.id = p.id
             WHERE p.id = ?1 AND p.session_id = ?2
               AND h.owner_subject = ?3 AND h.owner_client = ?4
               AND p.status IN ('pending', 'suspended')",
            rusqlite::params![
                id.as_str(),
                session.as_str(),
                owner.subject,
                owner.client.as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        );
        match raw {
            Ok(row) => decode_row(row),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                Err(PermissionHttpError::NotFound(id.clone()))
            }
            Err(error) => Err(PermissionHttpError::Sqlite(error)),
        }
    }

    pub fn reply(
        &mut self,
        store: &mut EventStore,
        command: PermissionReplyCommand<'_>,
        mint_sibling: impl FnMut() -> Result<DecisionIds, PermissionHttpError>,
    ) -> Result<PermissionReplyEffect, PermissionHttpError> {
        self.reply_inner(store, command, mint_sibling, &[])
            .map(|(effect, _)| effect)
    }

    /// Notify exactly the live tool waiters whose replies committed together.
    /// The snapshot and receipt are bounded by the authority's 64 reservations;
    /// durable HTTP-only siblings still cascade through the paged store path.
    pub fn reply_tools(
        &mut self,
        store: &mut EventStore,
        authority: &crate::tool_authority::ToolPermissionAuthority,
        command: PermissionReplyCommand<'_>,
        mint_sibling: impl FnMut() -> Result<DecisionIds, PermissionHttpError>,
    ) -> Result<PermissionReplyEffect, PermissionHttpError> {
        let pending = authority.pending_in(command.session);
        let (mut effect, cascaded) = self.reply_inner(store, command, mint_sibling, &pending)?;
        for committed in effect.take_committed().into_iter().chain(cascaded) {
            match authority.settle(committed) {
                Ok(()) | Err(crate::tool_authority::ToolAuthorizationError::Unknown(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(effect)
    }

    fn reply_inner(
        &mut self,
        store: &mut EventStore,
        command: PermissionReplyCommand<'_>,
        mut mint_sibling: impl FnMut() -> Result<DecisionIds, PermissionHttpError>,
        pending: &[PermissionId],
    ) -> Result<
        (
            PermissionReplyEffect,
            Vec<crate::tool_authority::CommittedPermissionReply>,
        ),
        PermissionHttpError,
    > {
        self.verify_owner(store, &command.owner, command.session, command.id)?;
        let held = store.held_writers();
        let tx = store.governed_transaction()?;
        let row = super::PermissionService::read_row(&tx, command.id)?
            .ok_or_else(|| PermissionHttpError::NotFound(command.id.clone()))?;
        if row.session != command.session.as_str() {
            return Err(PermissionHttpError::NotFound(command.id.clone()));
        }
        match row.status {
            Status::Pending => {}
            Status::Suspended(reason) => {
                return Err(PermissionHttpError::NotLive {
                    id: command.id.clone(),
                    reason,
                });
            }
            Status::Replied => {
                return if exact_retry(&tx, &command)? {
                    Ok((
                        PermissionReplyEffect {
                            outcome: CasOutcome::Applied,
                            committed: None,
                        },
                        Vec::new(),
                    ))
                } else {
                    Ok((
                        PermissionReplyEffect {
                            outcome: CasOutcome::AlreadyTerminal,
                            committed: None,
                        },
                        Vec::new(),
                    ))
                };
            }
            Status::Cancelled => {
                return Ok((
                    PermissionReplyEffect {
                        outcome: CasOutcome::AlreadyTerminal,
                        committed: None,
                    },
                    Vec::new(),
                ))
            }
        }
        let saved_patterns = saved_patterns(&tx, command.id)?;
        msys::check_reply(&row.action, &saved_patterns, command.reply)?;
        if command.reply == PermissionHttpReply::Session && !saved_patterns.is_empty() {
            self.admit_session_patterns(command.session, &row.action, &saved_patterns)?;
        }
        if command.reply == PermissionHttpReply::Always && command.project.is_none() {
            return Err(PermissionHttpError::ProjectRequired);
        }
        if let (PermissionHttpReply::Always, Some(project)) = (command.reply, command.project) {
            admit_rule_patterns(&tx, project, &row.action, &saved_patterns)?;
        }
        append_v2_reply(&tx, &held, &command, &command.decision)?;
        let cascaded = if command.reply == PermissionHttpReply::Reject {
            cascade_reject(&tx, &held, &command, &mut mint_sibling, pending)?
        } else {
            Vec::new()
        };
        tx.commit()?.discard();
        if command.reply == PermissionHttpReply::Session && !saved_patterns.is_empty() {
            self.remember_session(command.session, row.action, saved_patterns)?;
        }
        Ok((
            PermissionReplyEffect {
                outcome: CasOutcome::Applied,
                committed: Some(crate::tool_authority::CommittedPermissionReply::new(
                    command.id.clone(),
                    command.session.clone(),
                    command.reply.canonical(),
                )),
            },
            cascaded,
        ))
    }

    fn remember_session(
        &mut self,
        session: &SessionId,
        action: String,
        patterns: Vec<String>,
    ) -> Result<(), PermissionHttpError> {
        if let Some((_, _, held)) =
            self.session_approvals
                .iter_mut()
                .find(|(held_session, held_action, _)| {
                    held_session == session && held_action == &action
                })
        {
            for pattern in patterns {
                if !held.contains(&pattern) {
                    if held.len() >= MAX_HTTP_PERMISSION_RESOURCES {
                        return Err(PermissionHttpError::ResourceBound {
                            max: MAX_HTTP_PERMISSION_RESOURCES,
                        });
                    }
                    held.push(pattern);
                }
            }
            return Ok(());
        }
        if self.session_approvals.len() >= MAX_HTTP_SESSION_APPROVALS {
            return Err(PermissionHttpError::ApprovalBound {
                max: MAX_HTTP_SESSION_APPROVALS,
            });
        }
        self.session_approvals
            .push((session.clone(), action, patterns));
        Ok(())
    }

    fn admit_session_patterns(
        &self,
        session: &SessionId,
        action: &str,
        patterns: &[String],
    ) -> Result<(), PermissionHttpError> {
        if let Some((_, _, held)) = self
            .session_approvals
            .iter()
            .find(|(held_session, held_action, _)| held_session == session && held_action == action)
        {
            let additions = patterns
                .iter()
                .filter(|pattern| !held.contains(pattern))
                .count();
            if held.len().saturating_add(additions) > MAX_HTTP_PERMISSION_RESOURCES {
                return Err(PermissionHttpError::ResourceBound {
                    max: MAX_HTTP_PERMISSION_RESOURCES,
                });
            }
            return Ok(());
        }
        if self.session_approvals.len() >= MAX_HTTP_SESSION_APPROVALS {
            return Err(PermissionHttpError::ApprovalBound {
                max: MAX_HTTP_SESSION_APPROVALS,
            });
        }
        Ok(())
    }

    fn verify_owner(
        &self,
        store: &EventStore,
        owner: &PermissionOwner<'_>,
        session: &SessionId,
        id: &PermissionId,
    ) -> Result<(), PermissionHttpError> {
        let found = store.connection().query_row(
            "SELECT p.session_id, h.owner_subject, h.owner_client
             FROM permission_request p JOIN permission_http_request h ON h.id = p.id WHERE p.id = ?1",
            [id.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
        );
        let (held_session, held_subject, held_client) = match found {
            Ok(value) => value,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(PermissionHttpError::NotFound(id.clone()))
            }
            Err(error) => return Err(PermissionError::Sqlite(error).into()),
        };
        if held_session != session.as_str() {
            return Err(PermissionHttpError::NotFound(id.clone()));
        }
        if held_subject != owner.subject {
            return Err(PermissionHttpError::WrongPrincipal(id.clone()));
        }
        if held_client != owner.client.as_str() {
            return Err(PermissionHttpError::WrongClient(id.clone()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedPermissionRule {
    pub project: ProjectId,
    pub action: String,
    pub resource: String,
}

include!("http/helpers.rs");

#[cfg(test)]
mod reliability;
