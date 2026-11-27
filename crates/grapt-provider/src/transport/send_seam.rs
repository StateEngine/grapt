//! Deterministic hold immediately before an attempt's request bytes leave.
//!
//! Testkit builds only. A Step cancel that lands between context preparation
//! and the outbound send is otherwise caught only by luck on a slow runner, so
//! a fixture cannot prove the product never sends after it. This seam makes the
//! ordering forcible: the attempt holds here, the witness publishes the cancel,
//! and the released attempt must not reach the socket.
//!
//! Selection is by environment so it can reach a product child process.
//! `GRAPT_TESTKIT_HOLD_SESSION` names the Session whose attempt to hold — the
//! stable identity a fixture actually knows, never an attempt-id guess.
//! `GRAPT_TESTKIT_HOLD_MARKER` names the file created once the attempt is
//! genuinely inside the window, and `GRAPT_TESTKIT_HOLD_RELEASE` names the file
//! whose appearance releases it.
//!
//! An enabled hold that cannot establish its ordering — no marker configured,
//! an unwritable marker, no release path, or the bound elapsing — fails the
//! attempt rather than silently proceeding, because a witness that quietly
//! becomes a no-op would report a guarantee it never tested.

use std::path::{Path, PathBuf};
use std::time::Duration;

use grapt_types::{PhysicalAttemptId, SessionId};

/// How often the marker and release files are re-checked while held.
const POLL: Duration = Duration::from_millis(5);

/// Hold this attempt when the environment selects it, then return once the
/// fixture releases it. A no-op unless a Session is named, which no ordinary
/// build and no ordinary test does.
/// `bound` is the attempt's own response-head budget: a hold may never outlive
/// the deadline the caller already set for the send, so a released hold and an
/// expired send are never confused.
pub(crate) async fn hold(
    attempt: Option<&PhysicalAttemptId>,
    session: Option<&SessionId>,
    bound: Duration,
) -> Result<(), String> {
    let Some(wanted) = std::env::var_os("GRAPT_TESTKIT_HOLD_SESSION") else {
        return Ok(());
    };
    let wanted = wanted.to_string_lossy();
    if wanted.is_empty() {
        return Ok(());
    }
    let matches = session.is_some_and(|session| session.as_str() == wanted);
    if !matches {
        return Ok(());
    }
    let attempt = attempt.ok_or_else(|| {
        "hold selected this Session but the attempt identity is unavailable".to_owned()
    })?;
    let marker = std::env::var_os("GRAPT_TESTKIT_HOLD_MARKER")
        .map(PathBuf::from)
        .ok_or_else(|| "hold selected this Session but no marker path was configured".to_owned())?;
    let release = std::env::var_os("GRAPT_TESTKIT_HOLD_RELEASE")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "hold selected this Session but no release path was configured".to_owned()
        })?;
    // The marker carries the real attempt identity, so the witness can assert
    // WHICH attempt was held rather than inferring it.
    std::fs::write(&marker, attempt.as_str())
        .map_err(|error| format!("hold marker is unwritable: {error}"))?;
    let released = tokio::time::timeout(bound, wait_for(&release)).await;
    let _ = std::fs::remove_file(&marker);
    released.map_err(|_| {
        format!(
            "held attempt {} was never released within its response-head budget of {}ms",
            attempt.as_str(),
            bound.as_millis()
        )
    })
}

async fn wait_for(release: &Path) {
    while !release.exists() {
        tokio::time::sleep(POLL).await;
    }
}
