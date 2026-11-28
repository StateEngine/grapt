//! Per-command private scratch temp directory for Linux and Windows sandboxes
//! when the shared host temp directory is an ancestor of the workspace.

use super::Plan;
#[cfg(any(test, not(any(unix, windows))))]
use std::path::Path;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix::PinnedScratch;
#[cfg(unix)]
pub(super) use unix::{
    ensure_unix_scratch_anchor, unix_scratch_anchor_name, unix_scratch_anchor_path,
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::PinnedScratch;

#[cfg(not(any(unix, windows)))]
struct PinnedScratch {
    path: std::path::PathBuf,
}

#[cfg(not(any(unix, windows)))]
impl PinnedScratch {
    fn create(parent: &Path) -> Result<Self, String> {
        let path = tempfile::Builder::new()
            .prefix("grapt-sandbox-tmp-")
            .tempdir_in(parent)
            .map_err(|error| format!("sandbox private temp directory unavailable: {error}"))?
            .keep();
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup(self) -> Result<(), String> {
        std::fs::remove_dir(&self.path)
            .map_err(|error| format!("sandbox private temp root rmdir failed: {error}"))
    }

    fn verify_canonical_path(&self, _canonical: &Path) -> Result<(), String> {
        Ok(())
    }
}

/// Owned private scratch temp directory attached to one sandboxed child.
///
/// `TempDir`'s recursive drop fallback is disarmed immediately at allocation;
/// an OS directory handle is pinned at creation and held until explicit
/// confined cleanup after child termination is confirmed.
pub(super) struct PrivateScratch {
    pinned: Option<PinnedScratch>,
}

impl PrivateScratch {
    pub(super) fn attach(
        plan: &Plan,
        policy: &mut grapt_sandbox::Policy,
        environment: &mut Vec<String>,
    ) -> Result<Option<Self>, String> {
        let Some(parent) = plan.private_temp_parent() else {
            return Ok(None);
        };
        let pinned = PinnedScratch::create(parent)?;
        let canonical_private = match std::fs::canonicalize(pinned.path()) {
            Ok(path) => path,
            Err(error) => {
                let cleanup_err = pinned.cleanup().err();
                return Err(combine_errors(
                    format!("sandbox private temp directory unavailable: {error}"),
                    [cleanup_err],
                ));
            }
        };
        if let Err(reason) = pinned.verify_canonical_path(&canonical_private) {
            let cleanup_err = pinned.cleanup().err();
            return Err(combine_errors(reason, [cleanup_err]));
        }
        let Some(text) = canonical_private.to_str() else {
            let cleanup_err = pinned.cleanup().err();
            return Err(combine_errors(
                "sandbox private temp directory is not Unicode".to_owned(),
                [cleanup_err],
            ));
        };
        environment.retain(|entry| {
            !entry.split_once('=').is_some_and(|(name, _)| {
                name.eq_ignore_ascii_case("TMPDIR")
                    || name.eq_ignore_ascii_case("TEMP")
                    || name.eq_ignore_ascii_case("TMP")
            })
        });
        environment.push(format!("TMPDIR={text}"));
        environment.push(format!("TEMP={text}"));
        environment.push(format!("TMP={text}"));
        policy.writable_roots.push(canonical_private);
        Ok(Some(Self {
            pinned: Some(pinned),
        }))
    }

    #[cfg(test)]
    pub(super) fn path(&self) -> Option<&Path> {
        self.pinned.as_ref().map(PinnedScratch::path)
    }

    pub(super) fn cleanup(mut self) -> Result<(), String> {
        let Some(pinned) = self.pinned.take() else {
            return Ok(());
        };
        pinned.cleanup()
    }

    /// Release the pinned handles without walking or removing scratch when
    /// child termination could not be confirmed.
    pub(super) fn abandon_unreaped(mut self) {
        let _ = self.pinned.take();
    }
}

/// Run confined scratch cleanup only when the child is confirmed stopped;
/// otherwise release handles without walking scratch and return an explicit
/// error so callers preserve it alongside the primary failure.
pub(super) fn cleanup_after_stop(
    scratch: Option<PrivateScratch>,
    child_stopped: bool,
) -> Option<String> {
    let scratch = scratch?;
    if child_stopped {
        scratch.cleanup().err()
    } else {
        scratch.abandon_unreaped();
        Some(
            "sandbox private temp cleanup skipped because child termination was not confirmed"
                .to_owned(),
        )
    }
}

/// Finalize a completed child's exit code alongside scratch cleanup without
/// overwriting a child's nonzero exit code with generic status 1.
pub(super) fn finish_with_exit_code(
    code: i32,
    cleanup: Result<(), String>,
) -> (i32, Option<String>) {
    match cleanup {
        Ok(()) => (code, None),
        Err(error) if code == 0 => (1, Some(error)),
        Err(error) => (code, Some(error)),
    }
}

/// Preserve secondary kill/reap/receipt/cleanup errors alongside the primary
/// launch or observation error instead of discarding them.
pub(super) fn combine_errors<const N: usize>(
    primary: String,
    extras: [Option<String>; N],
) -> String {
    let mut combined = primary;
    for extra in extras.into_iter().flatten() {
        if !extra.is_empty() {
            combined.push_str("; ");
            combined.push_str(&extra);
        }
    }
    combined
}

#[cfg(test)]
mod tests;
