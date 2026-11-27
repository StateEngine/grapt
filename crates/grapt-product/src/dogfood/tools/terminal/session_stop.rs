//! Session-scoped cancellation with a supervisor cleanup receipt.
use super::*;

// Allow the supervisor its process-stop and I/O-drain bounds, plus a drain
// grace for queued startup and the final durable receipt.
const SESSION_STOP_BOUND: Duration = STOP_SETTLE_BOUND
    .saturating_add(DRAIN_GRACE)
    .saturating_add(DRAIN_GRACE);

pub(super) async fn stop_session(
    registry: &TerminalRegistry,
    session: &SessionId,
) -> Result<(), String> {
    let affected: Vec<_> = {
        let entries = lock(&registry.entries);
        entries
            .by_id
            .values()
            .chain(&entries.quarantine)
            .filter(|entry| &entry.session == session)
            .cloned()
            .collect()
    };
    let deadline = tokio::time::Instant::now() + SESSION_STOP_BOUND;
    // Signal every owned terminal before waiting for any one of them.
    for entry in &affected {
        entry.cancel();
    }
    let mut failures = Vec::new();
    for entry in affected {
        if let Err(reason) = await_cleanup(&entry, deadline).await {
            failures.push(format!("terminal {}: {reason}", entry.operation.as_str()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

async fn await_cleanup(entry: &Entry, deadline: tokio::time::Instant) -> Result<(), String> {
    loop {
        let changed = entry.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        {
            let data = lock(&entry.observation);
            // An exit value can describe requested cancellation rather than
            // observed exit. Only the supervisor's complete cleanup is proof.
            // Startup must also return: it can still install I/O workers.
            if data.startup_settled && data.supervised {
                if let Some(reason) = data.observed_cleanup_error() {
                    return Err(reason);
                }
                if data.exit.is_some()
                    && data.pipe.is_settled()
                    && data.cleanup() == CleanupState::Complete
                    && data.end_recorded
                {
                    return Ok(());
                }
            }
        }
        if tokio::time::timeout_at(deadline, changed).await.is_err() {
            let data = lock(&entry.observation);
            let reason = format!(
                "session stop could not confirm terminal startup, process, I/O and durable cleanup within {} ms (startup settled: {}, supervised: {}, exit recorded: {}, cleanup complete: {}, output readers: {}, input writer open: {}, durable end recorded: {}); cleanup remains owned by the terminal registry",
                SESSION_STOP_BOUND.as_millis(),
                data.startup_settled,
                data.supervised,
                data.exit.is_some(),
                data.cleanup() == CleanupState::Complete,
                data.readers,
                data.writer_open,
                data.end_recorded,
            );
            return Err(match data.observed_cleanup_error() {
                Some(error) => format!("{reason}; {error}"),
                None => reason,
            });
        }
    }
}
