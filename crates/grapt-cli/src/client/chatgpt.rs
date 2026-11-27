//! CLI adapter for the shared ChatGPT sign-in owner.
pub use grapt_login::chatgpt::*;
use grapt_types::{ClockAuthority, Timestamp};
use std::path::Path;

/// The operating system's wall clock: this module's one ambient time read.
/// It decides whether a stored token is near expiry and, when the issuer
/// states no expiry, when a fresh token lapses. It also stamps model discovery.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl ClockAuthority for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "named CLI wall-clock adapter documented in clippy.toml; grapt-cli has no grapt-store dependency"
    )]
    fn now(&self) -> Timestamp {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0);
        Timestamp::from_millis(i64::try_from(millis).unwrap_or(i64::MAX))
    }
}

/// Every path that starts the product (`grapt code`, `launch`, `service
/// start`, embedded `run`/`attach`/`acp`) calls this first. The running service
/// continues renewal when preparing model requests. A renewal that fails
/// while the token still works is reported on stderr and the start goes on.
pub async fn renew_before_start(profile: &Path) -> Result<(), String> {
    renew_before_start_with_clock(profile, &SystemClock).await
}

/// [`renew_before_start`] with an injected [`ClockAuthority`].
pub async fn renew_before_start_with_clock(
    profile: &Path,
    clock: &(dyn ClockAuthority + Sync),
) -> Result<(), String> {
    if !requires_renewal(profile).map_err(|error| error.to_string())? {
        return Ok(());
    }
    let client = production_client().map_err(|error| error.to_string())?;
    match refresh_if_due(profile, &client, ISSUER, clock).await {
        Ok(Renewal::Deferred(error)) => {
            eprintln!(
                "grapt: the ChatGPT sign-in could not be renewed yet ({error}); continuing with the current one"
            );
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
