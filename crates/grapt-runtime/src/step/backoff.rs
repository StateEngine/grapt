//! One cancellation-aware backoff for provider and pre-admission retries.
use super::*;

pub(super) fn salt_from_str(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Deterministic bounded positive jitter (`[0%, +25%]`) capped at `ceiling`.
///
/// Using a pure hash of the step, retry, and per-attempt/session salt avoids
/// ambient randomness while spreading concurrent sessions across the backoff
/// window so rate-limited or capacity-contended turns do not stampede in
/// lockstep.
pub(super) fn jittered(
    base: Duration,
    step: StepNumber,
    attempt: u32,
    salt: u64,
    ceiling: Duration,
) -> Duration {
    let base_ms = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
    if base_ms == 0 {
        return base.min(ceiling);
    }
    let mut state = u64::from(step.value())
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(u64::from(attempt).wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add(salt.wrapping_mul(0x94D0_49BB_1331_11EB))
        .wrapping_add(base_ms);
    state ^= state >> 30;
    state = state.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    state ^= state >> 27;
    state = state.wrapping_mul(0x94D0_49BB_1331_11EB);
    state ^= state >> 31;
    let extra_quarter = base_ms / 4;
    let jitter_ms = if extra_quarter == 0 {
        0
    } else {
        state % (extra_quarter + 1)
    };
    Duration::from_millis(base_ms.saturating_add(jitter_ms)).min(ceiling)
}

pub(super) async fn wait<S: AttemptSource>(
    source: &mut S,
    cancel: &mut watch::Receiver<Option<InterruptionSource>>,
    interrupt_available: &mut bool,
    notice: RetryNotice,
) -> Result<(), StepFailure> {
    source.retrying(Some(notice));
    // Pin once: losing the cancellation sender must not restart the delay.
    let mut backoff = std::pin::pin!(tokio::time::sleep(notice.after));
    let interrupted = loop {
        if let Some(by) = *cancel.borrow() {
            break Some(by);
        }
        tokio::select! {
            biased;
            changed = cancel.changed(), if *interrupt_available => {
                if changed.is_err() { *interrupt_available = false; }
            }
            _ = &mut backoff => break *cancel.borrow(),
        }
    };
    source.retrying(None);
    match interrupted {
        Some(by) => Err(StepFailure::new(
            StepTerminalReason::Interrupted { by },
            "interrupted during retry backoff",
        )),
        None => Ok(()),
    }
}
