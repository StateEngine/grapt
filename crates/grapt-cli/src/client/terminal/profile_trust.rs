//! SC-SEC-01: a repository profile needs a separate, content-bound user decision.
//! Canonical-path trust lookup, extended with a
//! configuration fingerprint. Profile selection never grants process consent.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/.
//! Modified by Grapt.
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

use grapt_types::profile_names::{choose, Spelling};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::command::terminal::{ProfileSource, ProfileTrustDecision, TerminalCommand};

const DOCUMENT_BYTES: u64 = 256 * 1024;
const CONFIGURATION: &[&str] = &[
    "grapt.json",
    "grapt.jsonc",
    "service.conf",
    "connections.json",
];
/// A profile from before the product rename keeps its trust fingerprint: the
/// same framing over its own document names, absent names included.
const LEGACY_CONFIGURATION: &[&str] = &[
    "secode.json",  // grapt-rename: keep
    "secode.jsonc", // grapt-rename: keep
    "service.conf",
    "connections.json",
];

/// The document names a profile is fingerprinted under: the legacy names only
/// when neither current configuration file is present and a legacy one is.
fn configuration(profile: &Path) -> Result<&'static [&'static str], String> {
    let present = |spelling: Spelling| -> Result<bool, String> {
        for name in [spelling.config_json(), spelling.config_jsonc()] {
            match std::fs::symlink_metadata(profile.join(name)) {
                Ok(_) => return Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "profile configuration is unreadable: {:?}",
                        error.kind()
                    ))
                }
            }
        }
        Ok(false)
    };
    Ok(
        match choose(present(Spelling::Current)?, present(Spelling::Legacy)?).spelling {
            Spelling::Current => CONFIGURATION,
            Spelling::Legacy => LEGACY_CONFIGURATION,
        },
    )
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileSnapshot {
    canonical: PathBuf,
    fingerprint: [u8; 32],
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    read_input(
        path,
        path.file_name() == Some(std::ffi::OsStr::new("connections.json")),
    )
}

fn read_input(path: &Path, private: bool) -> Result<Option<Vec<u8>>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "profile trust input is unreadable: {:?}",
                error.kind()
            ))
        }
    };
    if !metadata.is_file() || linked(&metadata) || metadata.len() > DOCUMENT_BYTES {
        return Err("profile trust requires bounded, regular, unlinked configuration files".into());
    }
    #[cfg(windows)]
    let before_identity = same_file::Handle::from_path(path).map_err(|error| error.to_string())?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOFOLLOW,
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000).share_mode(1);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("profile trust input cannot open: {:?}", error.kind()))?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if !opened.is_file() || linked(&opened) {
        return Err("profile trust input changed type".into());
    }
    #[cfg(windows)]
    if before_identity
        != same_file::Handle::from_file(file.try_clone().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?
    {
        return Err("profile trust input changed identity".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.dev() != opened.dev() || metadata.ino() != opened.ino() {
            return Err("profile trust input changed identity".into());
        }
    }
    if private {
        let verified = if path.file_name() == Some(std::ffi::OsStr::new("connections.json")) {
            grapt_login::profile_security::verify_owner_only_file(path, &file)
        } else {
            grapt_login::profile_security::verify_private_file(path, &file)
        };
        verified.map_err(|error| {
            format!(
                "profile trust record authority is invalid: {:?}",
                error.kind()
            )
        })?;
    }
    let mut bytes = Vec::new();
    file.take(DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("profile trust input cannot read: {:?}", error.kind()))?;
    if bytes.len() as u64 > DOCUMENT_BYTES {
        return Err("profile trust input exceeds its byte bound".into());
    }
    Ok(Some(bytes))
}

impl ProfileSnapshot {
    fn location(profile: &Path) -> Result<PathBuf, String> {
        std::fs::canonicalize(profile)
            .map_err(|error| format!("profile trust path cannot be resolved: {:?}", error.kind()))
    }

    fn capture(profile: &Path) -> Result<Self, String> {
        let metadata = std::fs::symlink_metadata(profile).map_err(|error| {
            format!("profile trust directory is unreadable: {:?}", error.kind())
        })?;
        if !metadata.is_dir() || linked(&metadata) {
            return Err("profile trust requires a real profile directory".into());
        }
        let canonical = Self::location(profile)?;
        let mut hash = Sha256::new();
        hash.update(b"secode.repository-profile-trust.v1\0");
        for name in configuration(&canonical)? {
            hash.update(name.as_bytes());
            match read(&canonical.join(name))? {
                Some(bytes) => {
                    hash.update([1]);
                    hash.update((bytes.len() as u64).to_le_bytes());
                    hash.update(bytes);
                }
                None => hash.update([0]),
            }
        }
        Ok(Self {
            canonical,
            fingerprint: hash.finalize().into(),
        })
    }

    fn record(&self, root: &Path) -> Result<PathBuf, String> {
        Self::record_path(&self.canonical, root)
    }

    fn record_path(canonical: &Path, root: &Path) -> Result<PathBuf, String> {
        let path = canonical
            .to_str()
            .ok_or("profile trust requires a UTF-8 path")?;
        Ok(root.join(format!("{:x}.json", Sha256::digest(path.as_bytes()))))
    }

    fn trusted(&self, root: &Path) -> Result<bool, String> {
        let Some(bytes) = read_input(&self.record(root)?, true)? else {
            return Ok(false);
        };
        let previous: Self = serde_json::from_slice(&bytes)
            .map_err(|_| "profile trust record is malformed; revoke it explicitly".to_owned())?;
        Ok(previous == *self)
    }

    fn remember(&self, root: &Path) -> Result<(), String> {
        std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
        let metadata = std::fs::symlink_metadata(root).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || linked(&metadata) {
            return Err("profile trust storage must be a real user-owned directory".into());
        }
        grapt_login::profile_security::protect_directory(root).map_err(|error| {
            format!(
                "profile trust storage protection failed: {:?}",
                error.kind()
            )
        })?;
        let target = self.record(root)?;
        let temporary = target.with_extension(format!("{}.new", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        let result = (|| {
            grapt_login::profile_security::protect_new_private_file(&temporary, &file).map_err(
                |error| format!("new trust record protection failed: {:?}", error.kind()),
            )?;
            let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|error| error.to_string())?;
            drop(file);
            if Self::capture(&self.canonical)? != *self {
                return Err("profile configuration changed while trust was being recorded".into());
            }
            std::fs::rename(&temporary, &target).map_err(|error| error.to_string())?;
            crate::installation::sync_directory(root)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
}

fn storage(command: &TerminalCommand, repository: &Path) -> Result<PathBuf, String> {
    let root = command.user_profile.as_deref().and_then(Path::parent)
        .ok_or("repository profile trust needs a user configuration directory; use --profile for this launch only")?;
    // Existing parents are resolved before appending missing components. Trust
    // must never be read from a repository-supplied registry or an alias of it.
    let mut ancestor = root;
    let mut tail = Vec::new();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tail.push(ancestor.file_name().ok_or("invalid trust storage path")?);
                ancestor = ancestor.parent().ok_or("invalid trust storage path")?;
                if tail.len() > 64 {
                    return Err("trust storage path exceeds ancestor bound".into());
                }
            }
            Err(error) => return Err(format!("trust storage is unreadable: {:?}", error.kind())),
        }
    }
    let mut resolved = std::fs::canonicalize(ancestor).map_err(|error| error.to_string())?;
    for part in tail.into_iter().rev() {
        resolved.push(part);
    }
    if resolved.starts_with(repository) {
        return Err("profile trust must be stored outside the opened repository".into());
    }
    let root = resolved.join("profile-trust");
    match std::fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() && !linked(&metadata) => {
            grapt_login::profile_security::verify_directory(&root).map_err(|error| {
                format!(
                    "profile trust storage protection is invalid: {:?}",
                    error.kind()
                )
            })?;
        }
        Ok(_) => return Err("profile trust storage must be a real user-owned directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("trust storage is unreadable: {:?}", error.kind())),
    }
    Ok(root)
}

fn revoke_record(
    root: &Path,
    record: &Path,
    publish: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    match std::fs::remove_file(record) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "profile trust revocation failed: {:?}",
                error.kind()
            ))
        }
    }
    match std::fs::symlink_metadata(root) {
        Ok(_) => publish(root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "profile trust publication failed: {:?}",
            error.kind()
        )),
    }
}

#[derive(Debug)]
pub(super) enum Authorization {
    Revoked,
    Launch(Option<String>),
}

impl ProfileSnapshot {
    fn launch(&self) -> Result<Authorization, String> {
        serde_json::to_string(self)
            .map(|encoded| Authorization::Launch(Some(encoded)))
            .map_err(|_| "profile approval cannot be encoded".into())
    }
}

pub(super) fn authorize(
    command: &TerminalCommand,
    repository: &Path,
) -> Result<Authorization, String> {
    authorize_inner(command, repository, super::first_run::interactive())
}

fn authorize_inner(
    command: &TerminalCommand,
    repository: &Path,
    interactive: bool,
) -> Result<Authorization, String> {
    let decision = command.profile_trust;
    if decision == ProfileTrustDecision::Check
        && command.profile_source != ProfileSource::RepositoryDefault
    {
        return Ok(Authorization::Launch(None));
    }
    let root = storage(command, repository)?;
    if decision == ProfileTrustDecision::Revoke {
        // Revocation removes the decision for a location, not for its current
        // contents. A malformed or newly linked configuration must not prevent
        // the user from withdrawing an earlier grant.
        let canonical = ProfileSnapshot::location(&command.profile)?;
        let record = ProfileSnapshot::record_path(&canonical, &root)?;
        revoke_record(&root, &record, crate::installation::sync_directory)?;
        return Ok(Authorization::Revoked);
    }
    let snapshot = ProfileSnapshot::capture(&command.profile)?;
    if decision == ProfileTrustDecision::Remember {
        snapshot.remember(&root)?;
        return snapshot.launch();
    }
    if snapshot.trusted(&root)? {
        return snapshot.launch();
    }
    if !interactive {
        return Err("repository profile is not trusted for its current path and configuration; select it explicitly with --profile, or review it in an interactive terminal".into());
    }
    let mut out = std::io::stderr().lock();
    writeln!(out, "Repository profile {} can select providers and executable configuration. Trust this exact configuration? [y/N]", snapshot.canonical.display()).map_err(|error| error.to_string())?;
    out.flush().map_err(|error| error.to_string())?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .take(16)
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        return Err("repository profile trust declined; nothing was started".into());
    }
    snapshot.remember(&root)?;
    snapshot.launch()
}

#[cfg(test)]
#[path = "profile_trust_tests.rs"]
mod tests;
