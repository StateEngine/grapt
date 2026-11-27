use super::*;

pub(super) async fn compact_and_reload<H: HistorySource>(
    history: &mut H,
    context: CompactionContext,
) -> Result<HistoryGeneration, StepFailure> {
    let generation = context.generation;
    match history.compact_with(context).await {
        Err(by) => {
            return Err(StepFailure::new(
                StepTerminalReason::Interrupted { by },
                "interrupted during compaction",
            ))
        }
        Ok(Err(error)) => {
            let (failure, reason) = error.into_terminal();
            return Err(StepFailure::new(
                StepTerminalReason::ContextFailure { failure },
                reason,
            ));
        }
        Ok(Ok(())) => {}
    }
    let next = history.reload().map_err(|error| {
        StepFailure::new(
            StepTerminalReason::ContextFailure {
                failure: ContextFailure::CompactionFailed,
            },
            error.reason,
        )
    })?;
    if next <= generation {
        return Err(StepFailure::new(
            StepTerminalReason::ContextFailure {
                failure: ContextFailure::CompactionNoProgress,
            },
            format!(
                "reload returned generation {} which does not advance past {}",
                next.value(),
                generation.value()
            ),
        ));
    }
    Ok(next)
}
