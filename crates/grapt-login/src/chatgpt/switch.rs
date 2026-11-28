//! One recoverable switch of a profile's sign-in together with the files that
//! describe its account (`grapt login chatgpt`).
//!
//! Every replacement is validated by the caller, then staged durably in a
//! private journal directory. The commit marker is published through its own
//! synced temporary and one rename, so it is either absent or complete. Only
//! then are the staged files renamed into the profile. A failure or crash
//! before the marker leaves the profile untouched, and the journal is
//! discarded; after it, the next owner of the profile's sign-in lock finishes
//! the renames. The lock is held throughout and every owner recovers on
//! acquiring it, so a running service never reads a half-switched profile.
//!
//! The journal is a closed namespace: recovery and cleanup accept only the
//! names below as bounded regular files, remove only those, and refuse,
//! preserving everything, when anything else is there.

use std::io::{self, Write};
use std::path::Path;

use grapt_types::profile_names::Spelling;

#[path = "switch/connections.rs"]
mod connections;
const JOURNAL: &str = ".chatgpt-switch";
const COMMITTED: &str = "committed";
const COMMITTING: &str = "committed.new";
const MARKER: &[u8] = b"secode.chatgpt-switch.v1\n";

/// The largest staged file: a model listing.
const STAGED_CAP: u64 = 16 * 1024 * 1024 + 64;

/// A profile file a switch replaces besides the sign-in itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchFile {
    /// `grapt.jsonc`, already validated by the caller.
    Config(Vec<u8>),
    /// The validated secret-free managed connection generation.
    Connections(Vec<u8>),
    /// `account-models.json`, already validated by the caller.
    Listing(Vec<u8>),
}

impl SwitchFile {
    /// The published name; the configuration keeps the profile's spelling.
    fn name(&self, spelling: Spelling) -> &'static str {
        match self {
            Self::Config(_) => spelling.config_jsonc(),
            Self::Connections(_) => "connections.json",
            Self::Listing(_) => LISTING_FILE,
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::Config(bytes) | Self::Connections(bytes) | Self::Listing(bytes) => bytes,
        }
    }
}

const LISTING_FILE: &str = "account-models.json";

/// Every name a journal may publish, in publication order. The sign-in comes
/// after the files describing its account. A journal stages the configuration
/// under its profile's spelling, so recovery accepts either.
const NAMES: [&str; 6] = [
    "connections.json",
    Spelling::Current.config_jsonc(),
    Spelling::Legacy.config_jsonc(),
    LISTING_FILE,
    super::STATE_FILE,
    super::CREDENTIAL_FILE,
];

pub(super) fn pending(profile: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(profile.join(JOURNAL)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn invalid(detail: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("sign-in switch journal {detail}; it was left in place for inspection"),
    )
}

/// Switch `profile` to `tokens` and `files` as one unit. The caller holds the
/// profile's sign-in lock and has recovered any earlier journal.
pub(super) fn switch(
    profile: &Path,
    tokens: &super::Tokens,
    files: &[SwitchFile],
) -> Result<(), super::SignInError> {
    let journal = profile.join(JOURNAL);
    if std::fs::symlink_metadata(&journal).is_ok() {
        // Recovery refused it on taking the lock; never stage over it.
        return Err(super::SignInError::Store(
            "an earlier sign-in switch journal is still present; inspect .chatgpt-switch in the profile".into(),
        ));
    }
    if let Err(error) = stage(&journal, tokens, files, true) {
        // Nothing was published. Discard what this call staged; a journal
        // holding anything else stays for inspection.
        let _ = discard(&journal);
        return Err(super::write_failure("sign-in switch", "staged", &error));
    }
    roll_forward(profile, &journal).map_err(|error| {
        super::SignInError::Store(format!(
            "switch was committed but not finished ({:?}); the next grapt start finishes it",
            error.kind()
        ))
    })
}

/// Durably stage every replacement, then (when `commit`) publish the marker
/// that makes the switch happen.
pub(super) fn stage(
    journal: &Path,
    tokens: &super::Tokens,
    files: &[SwitchFile],
    commit: bool,
) -> io::Result<()> {
    // The journal lives in its profile; stage under that profile's spelling.
    let spelling = match journal.parent() {
        Some(profile) => crate::profile_files::profile_spelling(profile)?.spelling,
        None => Spelling::Current,
    };
    #[cfg(unix)]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(journal)?;
    #[cfg(windows)]
    crate::profile_security::protect_directory(journal)?;
    for file in files {
        if file.name(spelling) == "connections.json" && file.bytes().len() > 64 * 1024 {
            return Err(invalid("connection generation exceeds its bound"));
        }
        write_new(&journal.join(file.name(spelling)), file.bytes())?;
    }
    // Staged files are published over the profile's own, so each is sealed
    // as a replacement for the file it will become.
    let seal = |bytes: &[u8], name: &str| match journal.parent() {
        Some(profile) => crate::profile_security::seal_for(bytes, &profile.join(name)),
        None => crate::profile_security::seal(bytes),
    };
    let state = seal(&super::encode(tokens), super::STATE_FILE)?;
    write_new(&journal.join(super::STATE_FILE), state.as_slice())?;
    let credential = seal(tokens.access.as_bytes(), super::CREDENTIAL_FILE)?;
    write_new(&journal.join(super::CREDENTIAL_FILE), credential.as_slice())?;
    super::sync_directory(journal);
    if commit {
        write_new(&journal.join(COMMITTING), MARKER)?;
        std::fs::rename(journal.join(COMMITTING), journal.join(COMMITTED))?;
        super::sync_directory(journal);
    }
    Ok(())
}

/// Finish or discard an interrupted switch. Every owner of the sign-in lock
/// calls this on acquiring it.
pub(super) fn recover(profile: &Path) -> io::Result<()> {
    let journal = profile.join(JOURNAL);
    match std::fs::symlink_metadata(&journal) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(invalid("is not a directory"))
        }
        Ok(_) => {}
    }
    // A managed switch owns both writers. Keep this synchronous recovery
    // lease only through publication, never through sign-in network I/O.
    let _connections = connections::recover_lock(profile, &journal)?;
    if committed(&journal)? {
        roll_forward(profile, &journal)
    } else {
        discard(&journal)
    }
}

/// Whether the journal holds its complete marker. Validates the whole
/// namespace first, so nothing is published or removed from a journal this
/// code did not write.
fn committed(journal: &Path) -> io::Result<bool> {
    validate(journal)?;
    let path = journal.join(COMMITTED);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
        Ok(_) => {
            // The marker is published by rename, so it is complete or absent.
            if std::fs::read(&path)? == MARKER {
                Ok(true)
            } else {
                Err(invalid("holds a foreign commit marker"))
            }
        }
    }
}

/// Every entry must be one of this module's names and a bounded, unlinked
/// regular file.
fn validate(journal: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(journal)? {
        let entry = entry?;
        let name = entry.file_name();
        let known = name
            .to_str()
            .is_some_and(|name| NAMES.contains(&name) || name == COMMITTED || name == COMMITTING);
        if !known {
            return Err(invalid("holds an unexpected entry"));
        }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > STAGED_CAP {
            return Err(invalid("holds an entry that is not a bounded regular file"));
        }
    }
    Ok(())
}

/// Publish whatever staged files remain, then clean up; idempotent across
/// crashes.
fn roll_forward(profile: &Path, journal: &Path) -> io::Result<()> {
    validate(journal)?;
    for name in NAMES {
        let staged = journal.join(name);
        match std::fs::symlink_metadata(&staged) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
            Ok(_) => std::fs::rename(&staged, profile.join(name))?,
        }
    }
    super::sync_directory(profile);
    remove_known(journal)
}

/// Discard an uncommitted journal: only its recognized entries.
fn discard(journal: &Path) -> io::Result<()> {
    validate(journal)?;
    remove_known(journal)
}

/// Remove the recognized entries, the marker last, then the now-empty
/// directory. A directory that is not empty is refused, never removed.
fn remove_known(journal: &Path) -> io::Result<()> {
    for name in NAMES.iter().copied().chain([COMMITTING, COMMITTED]) {
        match std::fs::remove_file(journal.join(name)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    std::fs::remove_dir(journal)
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    crate::profile_security::protect_new_private_file(path, &file)?;
    file.write_all(bytes)?;
    file.sync_all()
}
