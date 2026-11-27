//! Saved-rule withdrawal through the existing synchronous permission owner.
use super::*;
use crate::event::{PermissionRuleRevocation, MAX_RULE_ORIGINS};

#[derive(Debug)]
pub struct PermissionRevokeRuleCommand<'a> {
    pub owner: PermissionOwner<'a>,
    pub session: &'a SessionId,
    pub rule: &'a SavedPermissionRule,
    pub event: &'a EventId,
    pub created_ms: i64,
}

/// One bounded withdrawal step. Pending leaves consent active: the caller
/// must continue with a fresh event identity and must not acknowledge complete
/// revocation until Complete (or Absent). Each event retires at most 128 origins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionRuleWithdrawal {
    Absent,
    Pending,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPermissionRule {
    pub action: String,
    pub resource: String,
}

impl PermissionHttpState {
    /// Filter before applying the per-action bound: unrelated remembered
    /// command permissions must not hide sandbox grants from their operator.
    pub fn session_rule_resources(
        &self,
        session: &SessionId,
        action: &str,
    ) -> Result<Vec<String>, PermissionHttpError> {
        let mut result = Vec::new();
        for (_, _, resources) in self
            .session_approvals
            .iter()
            .filter(|(held, held_action, _)| held == session && held_action == action)
        {
            for resource in resources {
                if result.len() == MAX_HTTP_PERMISSION_RESOURCES {
                    return Err(PermissionHttpError::ResourceBound {
                        max: MAX_HTTP_PERMISSION_RESOURCES,
                    });
                }
                result.push(resource.clone());
            }
        }
        Ok(result)
    }

    pub fn saved_rule_resources(
        store: &EventStore,
        project: &ProjectId,
        action: &str,
    ) -> Result<Vec<String>, PermissionHttpError> {
        saved_rule_patterns(store, project, action)
    }

    pub fn session_rules(
        &self,
        session: &SessionId,
    ) -> Result<Vec<SessionPermissionRule>, PermissionHttpError> {
        let mut result = Vec::new();
        for (_, action, resources) in self
            .session_approvals
            .iter()
            .filter(|(held, _, _)| held == session)
        {
            for resource in resources {
                if result.len() == MAX_HTTP_PERMISSION_ROWS {
                    return Err(PermissionHttpError::ResourceBound {
                        max: MAX_HTTP_PERMISSION_ROWS,
                    });
                }
                result.push(SessionPermissionRule {
                    action: action.clone(),
                    resource: resource.clone(),
                });
            }
        }
        Ok(result)
    }

    /// One bounded page of old expansion asks to cancel during withdrawal.
    /// Match exact stored resource bytes; never reinterpret a path as a pattern.
    pub fn pending_sandbox_grants(
        &self,
        store: &EventStore,
        owner: &PermissionOwner<'_>,
        project: &ProjectId,
        session: Option<&SessionId>,
        resource: &str,
    ) -> Result<Vec<(PermissionId, SessionId)>, PermissionHttpError> {
        let mut query = store.connection().prepare(
            "SELECT CASE WHEN length(CAST(p.id AS BLOB)) <= 512 THEN p.id END,
                    CASE WHEN length(CAST(p.session_id AS BLOB)) <= 512 THEN p.session_id END
             FROM permission_request p
             JOIN permission_http_request h ON h.id=p.id
             WHERE h.owner_subject=?1 AND h.owner_client=?2 AND h.project_id=?3
               AND (?4 IS NULL OR p.session_id=?4)
               AND p.action='sandbox.write' AND p.status IN ('pending','suspended')
               AND EXISTS(SELECT 1 FROM json_each(h.resources_json) WHERE value=?5)
             ORDER BY p.rowid LIMIT 128",
        )?;
        let rows = query.query_map(
            rusqlite::params![
                owner.subject,
                owner.client.as_str(),
                project.as_str(),
                session.map(SessionId::as_str),
                resource
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        rows.map(|row| {
            let (permission, session) = row?;
            Ok((
                permission.parse().map_err(|_| {
                    PermissionHttpError::Replay("Pending grant permission is invalid".into())
                })?,
                session.parse().map_err(|_| {
                    PermissionHttpError::Replay("Pending grant session is invalid".into())
                })?,
            ))
        })
        .collect()
    }

    /// Withdraw remembered consent only. The caller's process owner must stop
    /// affected commands and fence prepared retries before acknowledging it.
    pub fn revoke_session_rule(&mut self, session: &SessionId, action: &str, resource: &str) {
        self.session_approvals
            .retain_mut(|(held, held_action, resources)| {
                if held == session && held_action == action {
                    resources.retain(|held| held != resource);
                }
                !resources.is_empty()
            });
    }

    /// Retire the current origins atomically with a canonical withdrawal fact.
    /// Authentication/project resolution and command cleanup remain with the
    /// existing application owner, as for permission creation and settlement.
    pub fn revoke_saved_rule(
        &mut self,
        store: &mut EventStore,
        command: PermissionRevokeRuleCommand<'_>,
    ) -> Result<PermissionRuleWithdrawal, PermissionHttpError> {
        let PermissionRevokeRuleCommand {
            owner,
            session,
            rule,
            event,
            created_ms,
        } = command;
        if rule.action.is_empty() || rule.action.len() > 1024 || rule.resource.len() > 8192 {
            return Err(PermissionHttpError::Replay(
                "Saved rule coordinates exceed their bounds".into(),
            ));
        }
        let held = store.held_writers();
        let transaction = store.governed_transaction()?;
        let present: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM permission_http_rule WHERE project_id=?1 AND action=?2 AND resource=?3)",
            rusqlite::params![rule.project.as_str(), rule.action, rule.resource], |row| row.get(0),
        )?;
        if !present {
            return Ok(PermissionRuleWithdrawal::Absent);
        }
        let origins = active_origins(&transaction, &rule.project, &rule.action, &rule.resource)?;
        if origins.is_empty() {
            return Err(PermissionHttpError::Replay(
                "Saved rule has no replayable approval origin".into(),
            ));
        }
        let data = json!({"sessionID":session.as_str(), "projectID":rule.project.as_str(),
            "action":rule.action, "resource":rule.resource,
            "originRequests":origins.iter().map(PermissionId::as_str).collect::<Vec<_>>(),
            "ownerSubject":owner.subject, "ownerClient":owner.client.as_str()});
        PermissionRuleRevocation::decode(&data)
            .map_err(|error| PermissionHttpError::Replay(error.to_string()))?;
        let aggregate = AggregateId::Session(session.clone());
        let writer = held.token(&aggregate)?;
        EventStore::append_within(
            &transaction,
            AppendInput {
                id: event,
                aggregate,
                writer,
                kind_name: "permission.rule.revoked",
                version: EventVersion::new(1).ok_or(PermissionError::Version)?,
                created_ms,
                data: &data,
            },
            &mut [&mut PermissionProjector],
        )?;
        let remaining: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM permission_http_rule_origin
             WHERE project_id=?1 AND action=?2 AND resource=?3)",
            rusqlite::params![rule.project.as_str(), rule.action, rule.resource],
            |row| row.get(0),
        )?;
        transaction.commit()?.discard();
        Ok(if remaining {
            PermissionRuleWithdrawal::Pending
        } else {
            PermissionRuleWithdrawal::Complete
        })
    }
}

pub(super) fn active_origins(
    transaction: &grapt_store::GovernedTransaction<'_>,
    project: &ProjectId,
    action: &str,
    resource: &str,
) -> Result<Vec<PermissionId>, PermissionHttpError> {
    let mut query = transaction.prepare(
        "SELECT CASE WHEN length(CAST(permission_id AS BLOB)) <= 512 THEN permission_id END
         FROM permission_http_rule_origin WHERE project_id=?1 AND action=?2 AND resource=?3
         ORDER BY permission_id LIMIT ?4",
    )?;
    let mut rows = query.query(rusqlite::params![
        project.as_str(),
        action,
        resource,
        MAX_RULE_ORIGINS as u32
    ])?;
    let mut origins = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        origins.push(
            id.parse().map_err(|_| {
                PermissionHttpError::Replay("Saved approval origin is invalid".into())
            })?,
        );
    }
    Ok(origins)
}

pub(in crate::service) fn project(
    transaction: &rusqlite::Transaction<'_>,
    revocation: &PermissionRuleRevocation,
) -> Result<(), String> {
    for origin in &revocation.origins {
        let coordinates = rusqlite::params![
            origin.as_str(),
            revocation.project.as_str(),
            revocation.action,
            revocation.resource
        ];
        transaction.execute(
            "INSERT INTO permission_http_rule_revocation(permission_id,project_id,action,resource)
             VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING", coordinates,
        ).map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM permission_http_rule_origin
             WHERE permission_id=?1 AND project_id=?2 AND action=?3 AND resource=?4",
                coordinates,
            )
            .map_err(|error| error.to_string())?;
    }
    // Another aggregate's later approval may have replayed already. Only the
    // retired IDs are withdrawn; an unrevoked origin keeps the resource live.
    transaction
        .execute(
            "DELETE FROM permission_http_rule WHERE project_id=?1 AND action=?2 AND resource=?3
         AND NOT EXISTS(SELECT 1 FROM permission_http_rule_origin
             WHERE project_id=?1 AND action=?2 AND resource=?3)",
            rusqlite::params![
                revocation.project.as_str(),
                revocation.action,
                revocation.resource
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}
