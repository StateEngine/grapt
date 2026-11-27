//! Live display observations for listed sessions, independent of transcript cursors.
use super::*;
use crate::contract::EventMember;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionActivity {
    Idle,
    Running,
    Compacting,
    Retrying,
    WaitingForProviderSlot { position: Option<u64> },
    Succeeded,
    Failed,
    Interrupted,
}

#[cfg(test)]
mod tests;

impl SessionActivity {
    /// Whether the session is doing work in this state.
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            Self::Running | Self::Compacting | Self::Retrying | Self::WaitingForProviderSlot { .. }
        )
    }

    pub fn label(self) -> std::borrow::Cow<'static, str> {
        let label = match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Compacting => "compacting",
            Self::Retrying => "retrying",
            Self::WaitingForProviderSlot {
                position: Some(position),
            } => return format!("waiting for provider slot (position {position})").into(),
            Self::WaitingForProviderSlot { position: None } => "waiting for provider slot",
            Self::Succeeded => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        };
        label.into()
    }
}

#[derive(Debug)]
pub(super) struct Observation {
    pub(super) state: SessionActivity,
    pub(super) created: u64,
    /// The frame's order, or the position of the presented pair.
    pub(super) stamp: crate::projection::FactStamp,
}

impl<C: Connection> Client<C> {
    /// A live fact, cleared when its subscription closes; absence is unreported.
    pub fn observed_activity(&self, session: &SessionWireId) -> Option<SessionActivity> {
        self.activity.get(session).map(|row| row.state)
    }

    /// The same observation with the server's order of the fact behind it.
    pub fn observed_activity_stamped(
        &self,
        session: &SessionWireId,
    ) -> Option<(SessionActivity, crate::projection::FactStamp)> {
        self.activity.get(session).map(|row| (row.state, row.stamp))
    }

    pub(super) fn observe_activity(&mut self, envelope: &EventEnvelope) -> bool {
        let (Some(session), Some(created)) = (&envelope.session_id, envelope.created) else {
            return false;
        };
        if !self.sessions.iter().any(|row| &row.id == session) {
            return false;
        }
        if matches!(
            envelope.member,
            Some(
                EventMember::ExecutionStarted
                    | EventMember::ExecutionSucceeded
                    | EventMember::ExecutionFailed
                    | EventMember::ExecutionInterrupted
            )
        ) && envelope.execution_id.is_none()
        {
            return false;
        }
        let state = match envelope.member {
            Some(EventMember::SessionIdle) => SessionActivity::Idle,
            Some(EventMember::SessionStatus) => match envelope
                .data
                .get("status")
                .and_then(|s| s.get("type"))
                .and_then(serde_json::Value::as_str)
            {
                Some("idle") => SessionActivity::Idle,
                Some("busy" | "retry") if envelope.data["phase"].as_str() == Some("compacting") => {
                    SessionActivity::Compacting
                }
                Some("busy") => SessionActivity::Running,
                Some("retry")
                    if envelope.data["status"]["action"]["reason"].as_str()
                        == Some("provider_slot") =>
                {
                    SessionActivity::WaitingForProviderSlot {
                        position: envelope.data["status"]["action"]["queuePosition"]
                            .as_u64()
                            .filter(|position| *position > 0),
                    }
                }
                Some("retry") => SessionActivity::Retrying,
                _ => return false,
            },
            Some(EventMember::ExecutionStarted) => SessionActivity::Running,
            Some(EventMember::ExecutionSucceeded) => SessionActivity::Succeeded,
            Some(EventMember::ExecutionFailed) => SessionActivity::Failed,
            Some(EventMember::ExecutionInterrupted) => SessionActivity::Interrupted,
            _ => return false,
        };
        let stamp = crate::projection::FactStamp::of(envelope);
        let fact = (state.is_busy(), stamp);
        if self.activity.get(session).is_some_and(|row| {
            crate::projection::supersedes(fact, (row.state.is_busy(), row.stamp))
                .map_or(row.created > created, |replaces| !replaces)
        }) {
            return false;
        }
        // Only the bounded picker can introduce keys, and refresh prunes removals.
        let changed = self
            .activity
            .get(session)
            .is_none_or(|row| row.state != state);
        self.activity.insert(
            session.clone(),
            Observation {
                state,
                created,
                stamp,
            },
        );
        changed
    }
}
