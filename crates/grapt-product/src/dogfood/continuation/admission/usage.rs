//! Terminal provider usage for admission; estimated counts stay separate.
use grapt_types::ContextFailure;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::dogfood) struct ConfirmedUsage {
    pub input: Option<u64>,
    pub context: Option<u64>,
}

// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Compact at 90% of the window.
// Configurable threshold.
// at bb64b894fd67158be0f874872e6d7bfe717e7368. The universal 90% ceiling remains.
//
// The ceiling is judged on the best available size, in this order: the
// provider's own terminal total when it reported one, otherwise the size this
// request was locally estimated at. A provider that omits usage must still
// compact before it overflows; waiting for a count that never arrives leaves
// the ceiling unreachable. `None` stays `None`: no evidence is not zero.
pub(in crate::dogfood) fn auto_compact_reached(
    context: Option<u64>,
    window: u64,
    limit: Option<u64>,
) -> bool {
    context.is_some_and(|used| {
        u128::from(used) * 10 >= u128::from(window) * 9 || limit.is_some_and(|limit| used >= limit)
    })
}

/// The size the universal ceiling is judged on: the provider's reported total
/// when there is one, else the local estimate for the request about to be sent.
pub(in crate::dogfood) fn ceiling_size(
    reported: Option<u64>,
    estimated: Option<u64>,
) -> Option<u64> {
    reported.or(estimated)
}

impl super::super::DurableContinuation {
    pub(in crate::dogfood) fn confirmed_usage(
        &self,
        attempt: &grapt_types::PhysicalAttemptId,
    ) -> Result<ConfirmedUsage, ContextFailure> {
        self.read_projection(|reader| reader.confirmed_usage_at(attempt))
            .map_err(|_: String| ContextFailure::CompactionFailed)
    }

    fn confirmed_usage_at(
        &self,
        attempt: &grapt_types::PhysicalAttemptId,
    ) -> Result<ConfirmedUsage, String> {
        confirmed_usage_in(&super::super::lock(&self.inbox), &self.session, attempt)
    }
}

/// One attempt's provider-confirmed input and context, read from the Session's
/// durable usage rows; harness estimates never count as confirmed.
pub(in crate::dogfood) fn confirmed_usage_in(
    inbox: &grapt_inbox::Inbox,
    session: &grapt_types::SessionId,
    attempt: &grapt_types::PhysicalAttemptId,
) -> Result<ConfirmedUsage, String> {
    // The gaps are logged once at their own durable coordinates: one
    // unreadable usage row costs that observation, so the caller that
    // reports evidence says exactly which rows it could not read instead of
    // silently returning a shorter list. The attempt's other measurements
    // still answer.
    let observations = grapt_session::SessionHistoryStore::over(inbox.store())
        .usage_for_attempt(session, attempt)
        .map_err(|error| error.to_string())
        .map(|(observations, omissions)| {
            crate::dogfood::history_notices::log_omissions(session, &omissions);
            observations
        })?;
    let usage = grapt_types::usage::reconcile_recorded_usage(&observations)
        .map_err(|error| error.to_string())?;
    let observed = |field: Option<&grapt_types::usage::ReconciledUsageField>| {
        field
            .filter(|field| {
                field.phase() == grapt_types::UsageObservationPhase::Terminal
                    && !matches!(
                        field.provenance(),
                        grapt_types::UsageProvenance::HarnessEstimated { .. }
                    )
            })
            .map(|field| field.count().value())
    };
    let input = observed(usage.input_total());
    // Output includes reasoning on the provider's reported coordinate;
    // never add the reasoning leaf again. Input alone is a lower bound
    // when a provider omits its output count, not an invented zero.
    let context = observed(usage.total())
        .or_else(|| {
            input
                .zip(observed(usage.output()))
                .and_then(|(a, b)| a.checked_add(b))
        })
        .or(input);
    Ok(ConfirmedUsage { input, context })
}
