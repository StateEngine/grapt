//! Best-effort diagnostics never delay service lifecycle or custody release.
use std::sync::{atomic::AtomicU64, Arc, OnceLock};

mod output;
pub use output::{LossyOutput, Offer};

/// Queue one message without its trailing newline; acceptance is not delivery.
/// The loss snapshot includes failed offers and sink writes across all callers.
pub fn report(message: String) -> (Offer, u64) {
    static STDERR: OnceLock<LossyOutput> = OnceLock::new();
    let output = STDERR.get_or_init(|| {
        LossyOutput::stderr(
            Arc::new(AtomicU64::new(0)),
            "GRAPT_DIAGNOSTIC_LOSS count_at_least",
        )
    });
    let offered = output.send(format!("{message}\n"));
    (offered, output.lost_at_least())
}
