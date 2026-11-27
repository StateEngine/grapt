//! Lifecycle fold: permission events → final blocker states, with transition
//! legality from the frozen contract:
//!
//! ```text
//! Issued    -- reply --> Settled     -- cancel --> Cancelled
//!           -- suspend --> Suspended
//! Suspended -- reply/cascade --> Settled   (observed: the S1 reconnect sweep)
//!           -- cancel --> Cancelled
//! Settled / Cancelled: terminal, no outgoing transition.
//! ```
//!
//! A request settled AFTER suspension keeps its suspension as HISTORICAL
//! metadata (D1 empiric: 10 real `replied` rows carry
//! `suspended_reason='process-loss'` — the reply does not clear the column,
//! and decode must never resurrect those as `Suspended`).

use crate::event::PermissionEvent;
use grapt_types::{
    BlockerState, PermissionBlockerState, PermissionId, PermissionSuspensionReason, SessionId,
};
use std::collections::HashMap;

/// Final record for one permission request after folding its events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRecord {
    pub session: SessionId,
    pub action: String,
    pub state: PermissionBlockerState,
    /// Suspension that happened on the way to a terminal state — historical
    /// metadata, never part of the terminal state itself.
    pub historical_suspension: Option<PermissionSuspensionReason>,
}

#[derive(Debug, Default)]
pub struct PermissionFold {
    pub requests: HashMap<PermissionId, RequestRecord>,
    /// Contract-violating transitions, with what was attempted.
    pub violations: Vec<String>,
    /// Events referencing a request never asked (broken causality).
    pub unmatched_references: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FoldOutcome {
    pub asked: u64,
    pub settled: u64,
    pub cancelled: u64,
    pub still_suspended: u64,
    pub still_issued: u64,
    pub settled_after_suspension: u64,
}

impl PermissionFold {
    pub fn apply(&mut self, event: PermissionEvent) {
        match event {
            // Withdrawing a saved rule does not reopen or cancel its settled
            // permission request. The HTTP projection owns that rule state.
            PermissionEvent::RuleRevoked(_) => {}
            PermissionEvent::Asked {
                id,
                session,
                action,
                ..
            } => {
                if self.requests.contains_key(&id) {
                    self.violations.push(format!("{id}: asked twice"));
                    return;
                }
                self.requests.insert(
                    id,
                    RequestRecord {
                        session,
                        action,
                        state: BlockerState::Issued,
                        historical_suspension: None,
                    },
                );
            }
            PermissionEvent::Replied { request, reply, .. } => {
                self.transition(request, "reply", |record| match record.state {
                    BlockerState::Issued => {
                        record.state = BlockerState::Settled(reply);
                        Ok(())
                    }
                    BlockerState::Suspended(reason) => {
                        // Legal settle-after-suspension (reject cascade /
                        // reconnect sweep); the reason becomes history.
                        record.historical_suspension = Some(reason);
                        record.state = BlockerState::Settled(reply);
                        Ok(())
                    }
                    _ => Err("reply to a terminal request"),
                });
            }
            PermissionEvent::Suspended {
                request, reason, ..
            } => {
                self.transition(request, "suspend", |record| match record.state {
                    BlockerState::Issued => {
                        record.state = BlockerState::Suspended(reason);
                        Ok(())
                    }
                    _ => Err("suspend from a non-issued state"),
                });
            }
            PermissionEvent::Cancelled { request, .. } => {
                self.transition(request, "cancel", |record| match record.state {
                    BlockerState::Issued | BlockerState::Suspended(_) => {
                        if let BlockerState::Suspended(reason) = record.state {
                            record.historical_suspension = Some(reason);
                        }
                        record.state = BlockerState::Cancelled(grapt_types::CancelledBy::User);
                        Ok(())
                    }
                    _ => Err("cancel of a terminal request"),
                });
            }
        }
    }

    fn transition(
        &mut self,
        id: PermissionId,
        verb: &str,
        apply: impl FnOnce(&mut RequestRecord) -> Result<(), &'static str>,
    ) {
        match self.requests.get_mut(&id) {
            None => self
                .unmatched_references
                .push(format!("{id}: {verb} before asked")),
            Some(record) => {
                if let Err(law) = apply(record) {
                    self.violations.push(format!("{id}: {law}"));
                }
            }
        }
    }

    pub fn outcome(&self) -> FoldOutcome {
        let mut out = FoldOutcome {
            asked: self.requests.len() as u64,
            settled: 0,
            cancelled: 0,
            still_suspended: 0,
            still_issued: 0,
            settled_after_suspension: 0,
        };
        for record in self.requests.values() {
            match &record.state {
                BlockerState::Settled(_) => {
                    out.settled += 1;
                    if record.historical_suspension.is_some() {
                        out.settled_after_suspension += 1;
                    }
                }
                BlockerState::Cancelled(_) => out.cancelled += 1,
                BlockerState::Suspended(_) => out.still_suspended += 1,
                BlockerState::Issued => out.still_issued += 1,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::PermissionReply;
    use serde_json::json;

    fn ev(kind: &str, data: serde_json::Value) -> PermissionEvent {
        match PermissionEvent::decode(kind, 1, &data) {
            Ok(Some(e)) => e,
            other => panic!("decode {kind}: {other:?}"),
        }
    }

    #[test]
    fn settle_after_suspension_keeps_history_not_state() -> Result<(), grapt_types::IdError> {
        let mut fold = PermissionFold::default();
        fold.apply(ev(
            "permission.asked",
            json!({"id":"per_a","sessionID":"ses_s","action":"x"}),
        ));
        fold.apply(ev(
            "permission.suspended",
            json!({"requestID":"per_a","sessionID":"ses_s","reason":"process-loss"}),
        ));
        fold.apply(ev(
            "permission.replied",
            json!({"requestID":"per_a","sessionID":"ses_s","reply":"reject"}),
        ));
        let request: PermissionId = "per_a".parse()?;
        let record = &fold.requests[&request];
        assert_eq!(record.state, BlockerState::Settled(PermissionReply::Reject));
        assert_eq!(
            record.historical_suspension,
            Some(PermissionSuspensionReason::ProcessLoss)
        );
        assert!(fold.violations.is_empty());
        Ok(())
    }

    #[test]
    fn terminal_states_refuse_further_transitions() {
        let mut fold = PermissionFold::default();
        fold.apply(ev(
            "permission.asked",
            json!({"id":"per_a","sessionID":"ses_s","action":"x"}),
        ));
        fold.apply(ev(
            "permission.replied",
            json!({"requestID":"per_a","sessionID":"ses_s","reply":"once"}),
        ));
        fold.apply(ev(
            "permission.replied",
            json!({"requestID":"per_a","sessionID":"ses_s","reply":"always"}),
        ));
        assert_eq!(fold.violations, vec!["per_a: reply to a terminal request"]);
    }

    #[test]
    fn references_before_ask_are_broken_causality() {
        let mut fold = PermissionFold::default();
        fold.apply(ev(
            "permission.replied",
            json!({"requestID":"per_ghost","sessionID":"ses_s","reply":"once"}),
        ));
        assert_eq!(
            fold.unmatched_references,
            vec!["per_ghost: reply before asked"]
        );
    }
}
