//! Product lifetime exclusion and the store owner's read-only rollback check.

use std::path::Path;

use grapt_service::installation::{prefix_of_binary, PrefixGuard};
use grapt_store::migration::preflight::{preflight_metadata_read_only, StoreReadiness};

#[cfg(windows)]
#[path = "installation_windows.rs"]
mod windows;

pub(super) const STORE_RECOVERY: &str = "this build cannot read the profile store; start or revert refused. Keep the current installation. Inspect recovery with `grapt-migrate disposition --run-root <existing-run> --served <current-database>`. Restore into a NEW file with `grapt-migrate restore --source <original-database> --backup <matching-backup> --restored <new-database>`. These commands need the actual recorded migration run and matching source/backup; this check creates none.";

pub(super) fn platform_refusal(
    error: grapt_platform::PlatformRefusal,
) -> crate::compose::ProductRefusal {
    match error {
        grapt_platform::PlatformRefusal::Config(
            cause @ grapt_platform::ConfigPublicationError::LocalModeRemoved,
        ) => crate::compose::ProductRefusal::TerminalSetup(cause.to_string()),
        grapt_platform::PlatformRefusal::Store(
            cause @ grapt_platform::PlatformStoreRefusal::CorruptOrQuarantined(_),
        ) => crate::compose::ProductRefusal::StoreCompatibility(cause),
        other => crate::compose::ProductRefusal::Platform(other.to_string()),
    }
}

pub(super) fn guard() -> Result<Option<PrefixGuard>, String> {
    let binary = std::env::current_exe()
        .map_err(|error| format!("product binary cannot be located: {:?}", error.kind()))?;
    let Some(prefix) = prefix_of_binary(&binary).map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let guard = PrefixGuard::shared(&prefix).map_err(|error| error.to_string())?;
    #[cfg(target_os = "linux")]
    {
        let target = std::fs::read_link(prefix.join("current")).map_err(|error| {
            format!("active installation cannot be selected: {:?}", error.kind())
        })?;
        let version = binary
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .ok_or("product has no immutable version")?;
        if target != Path::new("versions").join(version) {
            return Err(format!(
                "this product is no longer active; restart with {} code",
                prefix.join("current/bin/grapt").display()
            ));
        }
    }
    #[cfg(windows)]
    windows::selected(&prefix, &binary)?;
    Ok(Some(guard))
}

// This command is exempt from the prefix shared lock: revert already holds
// it exclusively. It only delegates to the store's read-only preflight and
// never starts election, services, config, providers or a writable database.
pub(super) fn check(arguments: &[String]) -> Result<(), String> {
    if arguments.len() != 3
        || arguments[1] != "--service"
        || arguments[2].is_empty()
        || arguments[2].len() > 4096
        || arguments[2].chars().any(char::is_control)
    {
        return Err("usage: grapt-server check-profile --service <profile>".into());
    }
    let profile = Path::new(&arguments[2]);
    match std::fs::symlink_metadata(profile) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err("profile check requires a real directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("profile cannot be inspected: {:?}", error.kind())),
    }
    // This is revert's "can the profile be opened" probe, run as a child under a
    // hard ten-second timeout. It asks the same question a ready open asks, and
    // must answer in milliseconds on any store size: a full page walk here would
    // time out every revert on a large store and refuse it.
    match preflight_metadata_read_only(&profile.join("database.sqlite3")) {
        StoreReadiness::Absent
        | StoreReadiness::Ready
        | StoreReadiness::Slow
        | StoreReadiness::MigrationNeeded(_) => Ok(()),
        StoreReadiness::Unreadable(_) => Err("profile store is unreadable; repair access before reverting".into()),
        StoreReadiness::HotJournal => Err("profile store needs recovery by its current build; start it and quit cleanly before reverting".into()),
        StoreReadiness::QuarantinedOrCorrupt(cause) => Err(format!(
            "profile store at {}: {cause:?}. {STORE_RECOVERY}", profile.join("database.sqlite3").display())),
    }
}
