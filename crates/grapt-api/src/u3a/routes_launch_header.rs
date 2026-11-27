//! The launch mode a creating client states on `POST /api/session`.
//!
//! The create body is a frozen object, so the mode does not go in it. It rides
//! a header instead: a header that is absent means the client did not state a
//! mode, which is exactly the "old client" case the reader must be able to tell
//! apart from a stated `interactive`.

use axum::http::HeaderMap;
use grapt_types::SessionLaunch;

use super::super::query::{QueryError, MAX_QUERY_BYTES};

/// The one header that states a Session's launch mode.
pub(super) const LAUNCH_HEADER: &str = "x-grapt-launch";
/// The pre-rename spelling, still accepted from older launchers.
pub(super) const LEGACY_LAUNCH_HEADER: &str = "x-secode-launch"; // grapt-rename: keep

/// Read the launch mode, if the client stated one.
///
/// An absent header is `Ok(None)` — a real answer, not a missing one. A header
/// that is present but unreadable or unrecognized is REFUSED: silently treating
/// it as `interactive` would arm the close grace against a client that asked to
/// run headless, which is the failure this whole fact exists to prevent.
pub(super) fn launch(headers: &HeaderMap) -> Result<Option<SessionLaunch>, QueryError> {
    // Both spellings may be sent only when they agree.
    if headers.contains_key(LAUNCH_HEADER)
        && headers.contains_key(LEGACY_LAUNCH_HEADER)
        && headers.get(LAUNCH_HEADER) != headers.get(LEGACY_LAUNCH_HEADER)
    {
        return Err(QueryError::DuplicateField);
    }
    let name = if headers.contains_key(LAUNCH_HEADER) {
        LAUNCH_HEADER
    } else {
        LEGACY_LAUNCH_HEADER
    };
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(QueryError::DuplicateField);
    }
    if value.len() > MAX_QUERY_BYTES {
        return Err(QueryError::TooLarge);
    }
    let raw = value.to_str().map_err(|_| QueryError::InvalidEncoding)?;
    SessionLaunch::parse(raw)
        .map(Some)
        .ok_or(QueryError::InvalidValue { field: "launch" })
}
