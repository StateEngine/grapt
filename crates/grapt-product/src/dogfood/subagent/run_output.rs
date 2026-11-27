//! Child output belongs to an exact closed execution, including fast runs.
use super::*;
use grapt_types::ExecutionId;

pub(in crate::dogfood) fn child_output_for_execution(
    application: &DogfoodApplication,
    child: &SessionId,
    execution: &ExecutionId,
) -> Result<String, ChildOutcome> {
    let observations = {
        let held = application.lock_inbox();
        grapt_session::execution::read(held.store().connection(), child, execution)
            .map_err(ChildOutcome::Failed)?
    };
    let [started, terminal] = observations.as_slice() else {
        return Err(ChildOutcome::Failed(
            "the child has no closed execution observation".into(),
        ));
    };
    if terminal.outcome == Some(grapt_session::execution::Outcome::Interrupted) {
        return Err(ChildOutcome::Cancelled(
            "the child execution was interrupted".into(),
        ));
    }
    let candidate = application
        .message_history(child)
        .map_err(ChildOutcome::Failed)?
        .into_iter()
        .rfind(|message| {
            message.opened_at_seq > started.position.seq
                && message.opened_at_seq < terminal.position.seq
        });
    if let Some(grapt_session::StepTerminal::Failed { reason }) = candidate
        .as_ref()
        .and_then(|message| message.terminal.as_ref())
    {
        let detail = format!("the child did not succeed: {reason:?}");
        return Err(
            if matches!(reason, grapt_types::StepTerminalReason::Interrupted { .. }) {
                ChildOutcome::Cancelled(detail)
            } else {
                ChildOutcome::Failed(detail)
            },
        );
    }
    match terminal.outcome {
        Some(grapt_session::execution::Outcome::Succeeded) => {}
        Some(grapt_session::execution::Outcome::Interrupted) => {
            return Err(ChildOutcome::Cancelled(
                "the child execution was interrupted".into(),
            ))
        }
        Some(grapt_session::execution::Outcome::Failed) => {
            return Err(ChildOutcome::Failed(
                terminal
                    .failure
                    .as_ref()
                    .map_or("the child execution failed", |failure| {
                        failure.message.as_str()
                    })
                    .to_owned(),
            ))
        }
        None => {
            return Err(ChildOutcome::Failed(
                "the child execution never settled".into(),
            ))
        }
    }
    let candidate = candidate.ok_or_else(|| {
        ChildOutcome::Failed("the child opened no Step in its own execution".into())
    })?;
    if !matches!(
        &candidate.terminal,
        Some(grapt_session::StepTerminal::Ended { .. })
    ) {
        return Err(ChildOutcome::Failed(
            "the child's final Step did not end successfully".into(),
        ));
    }
    let rendered = application
        .assistant_json(child, &candidate)
        .map_err(ChildOutcome::Failed)?;
    let value = serde_json::from_str(&rendered)
        .map_err(|error| ChildOutcome::Failed(format!("child answer: {error}")))?;
    Ok(assistant_text(&value))
}

/// Initial child admission can fail before it opens an assistant Step. Preserve
/// the closed, public capacity diagnosis so the parent can resume that child.
/// An initial child accepts one prompt; the admission floor excludes old rows.
pub(super) fn capacity_failure_after_admission(
    application: &DogfoodApplication,
    child: &SessionId,
    admitted_seq: u64,
) -> Result<Option<String>, ChildOutcome> {
    let held = application.lock_inbox();
    let failures = grapt_session::execution::failures(held.store().connection(), child)
        .map_err(ChildOutcome::Failed)?;
    Ok(failures
        .into_iter()
        .rev()
        .filter(|row| row.position.seq > admitted_seq)
        .filter_map(|row| row.failure)
        .find(|failure| failure.name == "ProviderCapacity")
        .map(|failure| failure.message))
}
