//! One text summary: a single call that must return text.
use super::super::summary_owner::{Rejection, SummaryOwner};
use super::*;

/// What the no-progress reason of a refused summary starts with: the
/// execution's stop names it, and the Session's notice is keyed on it.
pub(in crate::dogfood) const REFUSED_SUMMARY: &str = "the summary was refused";

/// Whether a summary call's answer can be committed: text within the size
/// bound. A tool call in the reply was refused before any effect and is
/// ignored, so the text it left stands; an answer cut at the bound, or one
/// with no text, is refused with nothing committed.
pub(super) fn usable(rejection: Option<Rejection>, text: &str) -> Result<(), CompactionFailure> {
    let why = match rejection {
        Some(Rejection::TextBound | Rejection::TooManyItems) => "it exceeded its size bound",
        Some(Rejection::ToolEffect) | None if text.trim().is_empty() => "it was empty",
        Some(Rejection::ToolEffect) | None => return Ok(()),
    };
    Err(CompactionFailure::NoProgress {
        reason: format!("{REFUSED_SUMMARY}: {why}; nothing was committed"),
    })
}

/// Why a summary response that did not finish is a retryable shape failure.
///
/// The transport detail is the richer source; a watchdog stall leaves it absent
/// and its class travels in the reason alone. A response that simply did not
/// finish (truncated, stopped for tool calls, or no stated completion) is a
/// response-shape failure, not a permanent one: the next input can try again,
/// so it must not pause automatic compaction as `Other`. An owner rejection is a different fact — the call was
/// refused before an effect — and keeps `Other`.
pub(super) fn unfinished_cause(report: &grapt_runtime::supervisor::AttemptReport) -> Cause {
    match report.provider_failure.as_ref() {
        Some(detail) => Cause::of(Some(detail)),
        None => match &report.outcome {
            StepOutcome::Failed { failure } => Cause::of_reason(failure.reason()),
            StepOutcome::Ended { .. } => Cause::Transient,
            StepOutcome::OwnerRejected { .. } => Cause::Other,
        },
    }
}

impl ProductCompactor {
    /// The one summary call. A tool call in the reply is refused before any
    /// effect and ignored: only the text counts, and an empty text fails the
    /// compaction visibly with nothing committed.
    pub(super) async fn run_summary(
        &mut self,
        context: &CompactionContext,
        id: &CompactionId,
        planned: PlannedAttempt,
    ) -> Result<SummaryOwner, CompactionFailure> {
        let owner = SummaryOwner {
            inbox: self.continuation.inbox.clone(),
            artifacts: self.continuation.artifacts.clone(),
            session: self.continuation.session.clone(),
            ids: self.ids.clone(),
            clock: self.clock.clone(),
            compaction: id.clone(),
            step: context.step,
            text: String::new(),
            coordinates: None,
            compaction_states: Vec::new(),
            tokens: None,
            other_items: 0,
            rejection: None,
            rejected_fragments: 0,
        };
        let (owner, report) = match self.call.run(context.clone(), planned, owner).await {
            Ok(ran) => ran,
            Err(error) => {
                self.failure_cause = Cause::of_refusal(&error);
                return Err(failed(error));
            }
        };
        let answered = owner.rejection.is_some()
            || matches!(
                report.outcome,
                StepOutcome::Ended {
                    finish: grapt_provider::FinishReason::Stop
                }
            );
        if !answered {
            // The transport detail is the richer source; a watchdog stall
            // leaves it absent and its class travels in the reason alone. A
            // response that simply did not finish is a response-shape
            // failure, not a permanent one: the next input can try again.
            let cause = unfinished_cause(&report);
            self.failure_cause = Some(cause);
            return Err(failed(format!(
                "summary provider operation failed: {:?}; {}",
                report.outcome,
                cause.message()
            )));
        }
        usable(owner.rejection, &owner.text)?;
        Ok(owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(result: Result<(), CompactionFailure>) -> Option<String> {
        match result {
            Err(CompactionFailure::NoProgress { reason }) => Some(reason),
            _ => None,
        }
    }

    /// Text stands; an empty answer, and one cut at the size bound, are refused
    /// by name with nothing committed; a tool call is ignored and leaves the
    /// text it did not replace.
    #[test]
    fn only_text_within_the_bound_is_usable() {
        assert!(usable(None, "## Objective\n- continue").is_ok());
        assert!(usable(Some(Rejection::ToolEffect), "the text beside a call").is_ok());
        for empty in ["", "  \n\t "] {
            for rejection in [None, Some(Rejection::ToolEffect)] {
                let reason = reason(usable(rejection, empty)).unwrap_or_default();
                assert!(reason.starts_with(REFUSED_SUMMARY) && reason.contains("empty"));
            }
        }
        for rejection in [Rejection::TextBound, Rejection::TooManyItems] {
            let reason = reason(usable(Some(rejection), "a summary cut at the bound"));
            assert!(
                reason
                    .is_some_and(|reason| reason.starts_with(REFUSED_SUMMARY)
                        && reason.contains("size bound")),
                "a truncated summary was usable"
            );
        }
    }
}
