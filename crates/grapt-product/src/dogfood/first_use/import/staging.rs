//! One claimed staging directory and the existing kernel exclusion owner.
//! The reserved sibling context is stable: never unlink its lock or enumerate
//! the parent. Under the per-user boundary, its `profile` is this operation's
//! sole unfinished generation. No input/provenance record grants store authority.
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};

use grapt_service::installation::PrefixGuard;

// Keep the initializer allowance plus state, owner, lock and listing per account.
const STAGED_FILE_CAP: usize = 32 + 4 * grapt_connections::store::CONNECTION_CAP;

pub(super) struct StagedProfile {
    exclusion: PrefixGuard,
    root: PathBuf,
    target: PathBuf,
}

impl StagedProfile {
    pub(super) fn begin(requested: &Path) -> Result<Self, String> {
        let parent = requested
            .parent()
            .ok_or("profile needs a parent directory")?;
        let name = requested
            .file_name()
            .ok_or("profile needs a final directory name")?;
        let parent = fs::canonicalize(parent).map_err(|_| "profile parent cannot be resolved")?;
        let target = parent.join(name);
        let digest = ring::digest::digest(&ring::digest::SHA256, name.as_encoded_bytes());
        let key: String = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let context = parent.join(format!(".grapt-init-{key}"));
        private_directory(&context, true)?;
        let exclusion = PrefixGuard::exclusive(&context).map_err(|error| match error {
            grapt_service::installation::PrefixError::Busy => {
                "profile is being initialized; wait for setup to finish, then retry".to_owned()
            }
            other => format!("profile initialization exclusion refused: {other}"),
        })?;
        absent_or_published(&target)?;
        let root = context.join("profile");
        clean_claimed(&root)?;
        private_directory(&root, false)?;
        Ok(Self {
            exclusion,
            root,
            target,
        })
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn publish(self) -> Result<PathBuf, String> {
        let Self {
            exclusion,
            root,
            target,
        } = self;
        let _exclusion = exclusion;
        #[cfg(test)]
        super::tests::fresh_crash::cut(&target, "publish")?;
        absent_or_published(&target)?;
        // Unix rename can replace only an EMPTY destination directory. The exclusion
        // stays held through publish, so another SE initializer cannot race.
        // Empty-directory creation by the same trusted user is accepted by.
        fs::rename(&root, &target).map_err(|error| {
            format!("profile publication refused: {:?}; preserve {} and rerun setup with an unused profile directory", error.kind(), target.display())
        })?;
        #[cfg(test)]
        super::tests::fresh_crash::cut(&target, "receipt")?;
        Ok(target)
    }
}

fn absent_or_published(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) if published_own_profile(path) => Err(format!(
            "profile {} is already published; use it",
            path.display()
        )),
        Ok(_) => Err(format!("profile {} already exists; choose an unused profile directory; if setup was interrupted, move it aside, then rerun setup", path.display())),
        Err(error) => Err(format!("profile {} is unreadable: {:?}; repair access before setup", path.display(), error.kind())),
    }
}

/// This install's own complete published profile: an open-ready configuration
/// stamped by this build over a store that is openable (or mid-replay). The
/// configuration is parsed with the account's own JSONC decoder: a published
/// profile keeps its source document's comments. The check is conservative
/// and read-only; anything it cannot verify keeps the generic refusal above.
fn published_own_profile(path: &Path) -> bool {
    // Bounded regular-file read: a non-file or oversized configuration is
    // conservatively "not published", and the read never blocks or
    // over-acquires.
    let spelling = super::super::super::legacy_names::config_resolution(path).spelling;
    let Ok(config_bytes) = super::super::read_bounded_regular_file(
        &path.join(spelling.config_jsonc()),
        "the published profile configuration",
        64 * 1_024,
    ) else {
        return false;
    };
    let Ok(parsed) = super::decode(&config_bytes) else {
        return false;
    };
    let open_ready = parsed
        .value()
        .get("store")
        .and_then(|store| store.value().get("authority"))
        .and_then(|value| value.value().as_str())
        == Some("open-ready");
    let Ok(own_build) = super::super::executable_build() else {
        return false;
    };
    let stamped = parsed
        .value()
        .get("product")
        .and_then(|product| product.value().get("build"))
        .and_then(|value| value.value().as_str())
        .is_some_and(|build| build == own_build);
    if !open_ready || !stamped {
        return false;
    }
    // Discovery only: this asks whether the profile can be opened at all, and
    // it runs while a user waits, so it must not walk every page.
    matches!(
        grapt_store::preflight_metadata_read_only(&path.join("database.sqlite3")),
        grapt_store::StoreReadiness::Ready | grapt_store::StoreReadiness::HotJournal
    )
}

fn linked(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x0000_0400 != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

fn private_directory(path: &Path, existing: bool) -> Result<(), String> {
    #[cfg(unix)]
    let mut directory = fs::DirBuilder::new();
    #[cfg(not(unix))]
    let directory = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut directory, 0o700);
    match directory.create(path) {
        Ok(()) => grapt_login::profile_security::protect_directory(path).map_err(|error| {
            format!(
                "profile directory protection failed: {}",
                grapt_login::profile_security::failure_diagnostic(&error)
            )
        }),
        Err(error) if existing && error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata =
                fs::symlink_metadata(path).map_err(|_| "initialization directory is unreadable")?;
            if !metadata.is_dir() || linked(&metadata) {
                return Err("initialization directory must be a real private directory".into());
            }
            grapt_login::profile_security::protect_directory(path).map_err(|error| {
                format!(
                    "profile directory protection failed: {}",
                    grapt_login::profile_security::failure_diagnostic(&error)
                )
            })
        }
        Err(error) => Err(format!(
            "initialization directory cannot be created: {:?}",
            error.kind()
        )),
    }
}

/// Only the exact reserved generation while its kernel exclusion is held.
/// Validate the complete bounded set before removing anything. No recursive
/// traversal, source-directory cleanup or existing-profile deletion occurs.
fn clean_claimed(root: &Path) -> Result<(), String> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(metadata) if metadata.is_dir() && !linked(&metadata) => {}
        Ok(_) => {
            return Err(
                "unfinished initialization must be a real staging directory; preserve it".into(),
            )
        }
        Err(error) => {
            return Err(format!(
                "unfinished initialization is unreadable: {:?}",
                error.kind()
            ))
        }
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(root)
        .map_err(|_| "unfinished initialization cannot be listed")?
        .take(STAGED_FILE_CAP + 1)
    {
        let entry = entry.map_err(|_| "unfinished initialization entry is unreadable")?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| "unfinished initialization file is unreadable")?;
        if files.len() == STAGED_FILE_CAP || !metadata.is_file() || linked(&metadata) {
            return Err(format!("unfinished initialization exceeds its flat {STAGED_FILE_CAP}-file bound or contains an unsupported entry; preserve it"));
        }
        files.push(path);
    }
    for path in files {
        fs::remove_file(path)
            .map_err(|_| "unfinished initialization file cannot be removed; preserve it")?;
    }
    fs::remove_dir(root).map_err(|_| "unfinished initialization directory cannot be removed".into())
}

#[cfg(test)]
mod tests;
