//! Whether a quiet subscription is still held by its door.
//!
//! A door holds a quiet long poll past the client's ten-second budget, so a
//! poll deadline looks the same from a quiet door and a hung one. A live door
//! answers a one-row page of the held snapshot at once.
use super::super::recovery::{advisory_action, Action, Recovery};
use super::*;

/// How often a quiet subscription asks the door whether it is still held. The
/// worst case to notice a hung door is about this interval plus the poll
/// budget plus PROBE_BOUND.
const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
/// A live door answers the probe's one-row page at once; well under the poll.
const PROBE_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

/// What a quiet poll deadline means for the owned subscription.
pub(super) enum Liveness {
    /// Renew the same poll: probed recently, or the door answered.
    Held,
    /// The door did not answer. The driver closes and reopens the
    /// subscription, or parks it when the refusal is not retryable.
    Lost { retryable: bool },
}

/// After a quiet poll deadline, at most once a PROBE_INTERVAL ask the door,
/// briefly, for a page of the held snapshot.
pub(super) async fn after_deadline(
    context: &mut NativeMailContext,
    recovery: &Mutex<Recovery>,
) -> Liveness {
    if context
        .probed
        .is_some_and(|probed| probed.elapsed() < PROBE_INTERVAL)
    {
        return Liveness::Held;
    }
    let probed = match context.runtime.as_mut() {
        Some(runtime) => runtime.probe(PROBE_BOUND).await,
        None => Ok(()),
    };
    // Only a completed probe spends the interval.
    context.probed = Some(tokio::time::Instant::now());
    let Err(dead) = probed else {
        return Liveness::Held;
    };
    // The door's typed refusals keep their usual meaning; only the probe's own
    // deadline means the door stopped answering.
    let retryable = match &dead {
        RemoteMailboxError::Port(failure) => {
            context
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.change_failure_is_retryable(failure))
                || advisory_action(context, failure).is_some()
        }
        _ => true,
    };
    // A reopen starts the exact retry count afresh.
    if let Ok(mut recovery) = recovery.lock() {
        recovery.escalate(Action::Reconnect, false);
    }
    let _refusal = context.transport_refusal("observation probe", dead);
    Liveness::Lost { retryable }
}
