//! How long a client waits for a request, shared by journeys on every
//! platform (`prompt_admission` itself runs only on Linux).
use std::time::{Duration, Instant};

/// The CLI's whole-request allowance for the prompt POST (`grapt_cli`'s
/// `REQUEST_ALLOWANCE`); the crates do not depend on each other. Also the
/// bound a journey waits for its Session to settle: as long as a client
/// waits for a prompt.
pub(super) const CLI_REQUEST_ALLOWANCE: Duration = Duration::from_secs(30);

#[allow(clippy::disallowed_methods)]
pub(super) fn now() -> Instant {
    Instant::now()
}
