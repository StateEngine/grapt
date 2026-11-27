//! Read-only, volatile comparisons beside the existing Session accounting owner.
use super::{SessionCalibrations, SessionId};
use grapt_provider::{DispatchSample, RequestDelta, RequestFingerprint};
use grapt_types::PhysicalAttemptId;
use std::collections::VecDeque;
const MAX_OBSERVATIONS: usize = 256;
mod call_usage;

#[derive(Default)]
pub(super) struct WireHistory {
    calls: call_usage::Totals,
    previous: Option<(PhysicalAttemptId, RequestFingerprint)>,
    observations: VecDeque<(PhysicalAttemptId, Observation)>,
    loss: u64,
}
#[derive(Clone)]
struct Observation {
    change: Option<RequestDelta>,
    previous: Option<PhysicalAttemptId>,
    body_bytes: Option<usize>,
    tools_bytes: Option<usize>,
    instructions_bytes: Option<usize>,
    usage_warning: Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
    cache_read_share: Option<f64>,
}
impl Observation {
    /// Whether this request repeated the previous one's whole prefix, so a
    /// provider cache could serve it; absent when there was nothing to compare.
    fn local_prefix_unchanged(&self) -> Option<bool> {
        match self.change? {
            RequestDelta::AppendOnly | RequestDelta::Identical => Some(true),
            RequestDelta::FirstObservation | RequestDelta::ComparisonBound => None,
            _ => Some(false),
        }
    }

    fn metadata(&self) -> serde_json::Value {
        let (change, index) = match self.change {
            None => ("unreported", None),
            Some(RequestDelta::FirstObservation) => ("first_observation", None),
            Some(RequestDelta::TargetChanged) => ("target_changed", None),
            Some(RequestDelta::CacheKeyChanged) => ("cache_key_changed", None),
            Some(RequestDelta::RoutingChanged) => ("routing_changed", None),
            Some(RequestDelta::ToolsChanged) => ("tools_changed", None),
            Some(RequestDelta::InstructionsChanged) => ("instructions_changed", None),
            Some(RequestDelta::OptionsChanged) => ("options_changed", None),
            Some(RequestDelta::InputChanged { index }) => ("input_changed", Some(index)),
            Some(RequestDelta::InputShortened { index }) => ("input_shortened", Some(index)),
            Some(RequestDelta::AppendOnly) => ("append_only", None),
            Some(RequestDelta::Identical) => ("identical", None),
            Some(RequestDelta::ComparisonBound) => ("comparison_bound", None),
        };
        serde_json::json!({
            "usageWarning": self.usage_warning.map(|warning| serde_json::json!({"field":warning.field_word(),"issue":warning.issue.word()})),
            "cacheRequestChange": change,
            "cacheRequestChangedItem": index.map(|index| index.to_string()),
            "cacheRequestPreviousAttempt": self.previous.as_ref().map(PhysicalAttemptId::as_str),
            "cacheRequestBodyBytes": self.body_bytes,
            "cacheRequestToolsBytes": self.tools_bytes,
            "cacheRequestInstructionsBytes": self.instructions_bytes,
            "cacheRequestComparison": "previous-observed-request; volatile; provider cause unreported",
            "cacheReadShare": self.cache_read_share,
            "localPrefixUnchanged": self.local_prefix_unchanged(),
        })
    }
}
impl SessionCalibrations {
    pub(in crate::dogfood) fn note_dispatch_loss(&self, loss: u64) {
        if loss == 0 {
            return;
        }
        // Even when the queue is now empty, previously lost samples invalidate
        // complete coverage. We cannot guess which Session lost the record.
        for accounting in super::super::lock(&self.0).values_mut() {
            if accounting.wire.loss != loss {
                accounting.wire.previous = None;
                accounting.wire.loss = loss;
            }
        }
    }

    pub(in crate::dogfood) fn observe_request(&self, sample: &DispatchSample, loss: u64) {
        let Some(session) = &sample.session else {
            return;
        };
        self.update(session, |accounting| {
            let history = &mut accounting.wire;
            // A dropped sample must never manufacture an immediate predecessor.
            if history.loss != loss {
                history.previous = None;
                history.loss = loss;
            }
            if history
                .observations
                .iter()
                .any(|(attempt, _)| *attempt == sample.attempt)
            {
                return;
            }
            history.calls.observe(sample);
            let observation = Observation {
                usage_warning: sample.usage_warning,
                cache_read_share: call_usage::cache_read_share(sample),
                change: sample.request.as_ref().map(|request| {
                    history
                        .previous
                        .as_ref()
                        .map_or(RequestDelta::FirstObservation, |(_, previous)| {
                            request.compare(previous)
                        })
                }),
                previous: history
                    .previous
                    .as_ref()
                    .map(|(attempt, _)| attempt.clone()),
                body_bytes: sample.request.as_ref().map(|request| request.body_bytes),
                tools_bytes: sample.request.as_ref().map(|request| request.tools_bytes),
                instructions_bytes: sample
                    .request
                    .as_ref()
                    .map(|request| request.instructions_bytes),
            };
            if history.observations.len() == MAX_OBSERVATIONS {
                let _oldest = history.observations.pop_front();
            }
            history
                .observations
                .push_back((sample.attempt.clone(), observation));
            history.previous = sample
                .request
                .as_ref()
                .map(|request| (sample.attempt.clone(), request.clone()));
        });
    }
    pub(in crate::dogfood) fn request_observation(
        &self,
        session: &SessionId,
        attempt: &PhysicalAttemptId,
    ) -> Option<serde_json::Value> {
        let sessions = super::super::lock(&self.0);
        let history = &sessions.get(session)?.wire;
        let mut metadata = history
            .observations
            .iter()
            .find(|(observed, _)| observed == attempt)?
            .1
            .metadata();
        metadata.as_object_mut()?.insert(
            "cacheObservedCalls".to_owned(),
            history.calls.metadata(history.loss),
        );
        Some(metadata)
    }
}
