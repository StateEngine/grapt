//! Transient grant fencing and cancellation stay with the terminal owner.
use super::*;
use grapt_types::ProjectId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dogfood::tools) struct GrantLease {
    project: ProjectId,
    generation: u64,
}

#[derive(Debug)]
pub(in crate::dogfood) struct RunningWriteGrant {
    pub operation: OperationId,
    pub resource: String,
}

impl TerminalRegistry {
    /// Capture before asking permission. Withdrawal and reservation serialize
    /// under this same owner, including while native spawn is queued.
    pub(in crate::dogfood::tools) fn prepare_grants(
        &self,
        project: &ProjectId,
        spec: &mut ExecSpec,
    ) -> Result<(), String> {
        if spec.environment.write_targets().is_empty() {
            return Ok(());
        }
        let entries = lock(&self.entries);
        if entries.grant_generation == u64::MAX {
            return Err("Sandbox consent generation is exhausted; restart the service".into());
        }
        spec.grant = Some(GrantLease {
            project: project.clone(),
            generation: entries.grant_generation,
        });
        Ok(())
    }

    pub(in crate::dogfood) fn running_write_grants(
        &self,
        session: &SessionId,
    ) -> Result<Vec<RunningWriteGrant>, String> {
        let entries = lock(&self.entries);
        let mut grants = Vec::new();
        for entry in entries
            .by_id
            .values()
            .filter(|entry| &entry.session == session)
        {
            let data = lock(&entry.observation);
            if data.exit.is_some() && data.cleanup() == CleanupState::Complete {
                continue;
            }
            for target in &entry.write_resources {
                if grants.len() == 128 {
                    return Err("Running sandbox grant list exceeds its bound".into());
                }
                grants.push(RunningWriteGrant {
                    operation: entry.operation.clone(),
                    resource: path_text(target),
                });
            }
        }
        Ok(grants)
    }

    /// Withdrawn consent cannot cross reservation after this fence. Already
    /// reserved commands, including startup races and closed-session quarantine,
    /// must settle physical cleanup before the caller acknowledges withdrawal.
    pub(in crate::dogfood) async fn revoke_write_grant(
        &self,
        project: &ProjectId,
        session: Option<&SessionId>,
        resource: &str,
        operation: Option<&OperationId>,
    ) -> Result<(), String> {
        self.persist_completed();
        let affected = {
            let mut entries = lock(&self.entries);
            // Exhaustion permanently closes expansion admission until restart;
            // cancellation still proceeds, so a saturated counter cannot revive it.
            entries.grant_generation = entries.grant_generation.saturating_add(1);
            entries
                .by_id
                .values()
                .chain(&entries.quarantine)
                .filter(|entry| {
                    entry
                        .spec
                        .grant
                        .as_ref()
                        .is_some_and(|lease| &lease.project == project)
                        && session.is_none_or(|session| &entry.session == session)
                        && operation.is_none_or(|operation| &entry.operation == operation)
                        && entry
                            .write_resources
                            .iter()
                            .any(|target| path_text(target) == resource)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        for entry in &affected {
            entry.cancel();
        }
        let deadline = tokio::time::Instant::now() + STOP_SETTLE_BOUND;
        for entry in affected {
            loop {
                let changed = entry.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                {
                    let data = lock(&entry.observation);
                    if data.exit.is_some() && data.cleanup() == CleanupState::Complete {
                        if let Some(error) = data.observed_cleanup_error() {
                            return Err(error);
                        }
                        break;
                    }
                }
                tokio::time::timeout_at(deadline, changed).await
                    .map_err(|_| "Consent withdrawn; command cleanup remains unconfirmed. Retry revocation before relying on it.".to_owned())?;
            }
        }
        Ok(())
    }
}

pub(super) fn validate(entries: &Entries, spec: &ExecSpec) -> Result<(), String> {
    if !spec.environment.write_targets().is_empty()
        && !spec.grant.as_ref().is_some_and(|lease| {
            lease.generation == entries.grant_generation && lease.generation != u64::MAX
        })
    {
        return Err(
            "Sandbox consent changed before command startup; request approval again".into(),
        );
    }
    Ok(())
}
