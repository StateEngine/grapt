//! The summary provider call is a separate Physical Attempt, even on the same Step.
use super::*;
use grapt_event::compaction_call::{self, CallCoordinates, CallEnd};
use grapt_types::AttemptUsage;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Calls(BTreeMap<PhysicalAttemptId, Call>);

#[derive(Default)]
struct Call {
    coordinates: Option<CallCoordinates>,
    opening: Option<CommittedPosition>,
    ended: Option<CallEnd>,
    usage: Vec<AttemptUsage>,
}

const KINDS: [&str; 3] = [
    "session.compaction.call.started",
    "session.compaction.call.usage",
    "session.compaction.call.ended",
];

pub(super) fn owns(kind: &str) -> bool {
    KINDS.iter().any(|name| {
        kind == *name
            || kind
                .strip_prefix(*name)
                .is_some_and(|suffix| suffix.starts_with('.'))
    })
}

impl Calls {
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    pub(super) fn apply(
        &mut self,
        kind: &str,
        raw: &str,
        position: CommittedPosition,
    ) -> Result<PhysicalAttemptId, String> {
        let (name, version) = kind
            .rsplit_once('.')
            .ok_or("compaction call version absent")?;
        if version != "1" || !KINDS.contains(&name) {
            return Err("unsupported compaction call version".into());
        }
        let value: Value =
            serde_json::from_str(raw).map_err(|_| "compaction call payload is not JSON")?;
        let (attempt, coordinates, outcome) = match name {
            "session.compaction.call.started" => {
                let coordinates = compaction_call::decode_started(&value)?;
                (coordinates.attempt.clone(), Some(coordinates), None)
            }
            "session.compaction.call.ended" => {
                let (coordinates, outcome) = compaction_call::decode_ended(&value)?;
                (
                    coordinates.attempt.clone(),
                    Some(coordinates),
                    Some(outcome),
                )
            }
            _ => {
                let fact = session_message::decode_observed(&value).map_err(|e| e.to_string())?;
                let call = self.entry(fact.attempt().clone())?;
                if call.usage.len() == 64 {
                    return Err("a compaction call exceeds 64 usage observations".into());
                }
                call.usage.push(fact.usage().clone());
                return Ok(fact.attempt().clone());
            }
        };
        let call = self.entry(attempt.clone())?;
        if let Some(existing) = &call.coordinates {
            if Some(existing) != coordinates.as_ref() {
                return Err("compaction call coordinates conflict".into());
            }
        }
        if let Some(outcome) = outcome {
            if call.ended.is_some() {
                return Err("compaction call ended twice".into());
            }
            call.ended = Some(outcome);
        } else {
            if call.opening.is_some() || call.ended.is_some() {
                return Err("compaction call opening is repeated or follows its end".into());
            }
            call.opening = Some(position);
        }
        call.coordinates = coordinates;
        Ok(attempt)
    }

    fn entry(&mut self, attempt: PhysicalAttemptId) -> Result<&mut Call, String> {
        if !self.0.contains_key(&attempt) && self.0.len() == MAX_ACCOUNTING_ATTEMPTS {
            return Err(format!(
                "accounting exceeds {MAX_ACCOUNTING_ATTEMPTS} attempts"
            ));
        }
        Ok(self.0.entry(attempt).or_default())
    }

    pub(super) fn append_to(self, snapshot: &mut AccountingSnapshot) -> Result<(), String> {
        for (attempt, call) in self.0 {
            if snapshot.attempts.iter().any(|a| a.attempt == attempt) {
                return Err(
                    "Physical Attempt appears in both assistant and compaction accounting".into(),
                );
            }
            snapshot.attempts.push(AccountingAttempt {
                attempt,
                terminal_observed: call.ended.is_some(),
                operation: AccountingOperation::Compaction,
                send_permitted: false,
                request_cache: None,
                step: call.coordinates.map(|c| c.step),
                // The coding Step's model does not prove the summary's target.
                selection: None,
                usage: grapt_types::usage::reconcile_usage(&call.usage)
                    .map_err(|e| e.to_string())?,
                opening: call.opening,
                first_block: None,
                step_terminal: None,
                completed_at: None,
            });
        }
        Ok(())
    }
}
