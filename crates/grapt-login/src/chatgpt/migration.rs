//! Two-file format migration under the existing renewal lock. The token
//! generation does not change here: a protected rollback slot holds both
//! originals until the complete pair is durable.
use std::io::{self, Write};
use std::path::Path;

const JOURNAL: &str = ".chatgpt-protection-upgrade";
const MARKER: &[u8] = b"secode.chatgpt-protection-upgrade.v1\n";
const NAMES: [&str; 2] = [super::CREDENTIAL_FILE, super::STATE_FILE];
const ENTRIES: [&str; 12] = [
    "0.original",
    "0.absent",
    "0.new",
    "0.restore",
    "1.original",
    "1.absent",
    "1.new",
    "1.restore",
    "ready.new",
    "ready",
    "commit.new",
    "committed",
];

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("profile migration journal is invalid")]
    InvalidJournal,
    #[error("profile migration rollback pending; restart this build before downgrading")]
    RollbackPending,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, Failure::InvalidJournal)
}

fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(18 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 18 * 1024 {
        return Err(invalid());
    }
    Ok(Some(bytes))
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn marker(path: &Path) -> io::Result<bool> {
    match read(path)? {
        None => Ok(false),
        Some(bytes) if bytes == MARKER => Ok(true),
        Some(_) => Err(invalid()),
    }
}

fn commit(journal: &Path) -> io::Result<()> {
    let temporary = journal.join("commit.new");
    if read(&temporary)?.is_some() {
        std::fs::remove_file(&temporary)?;
    }
    write_new(&temporary, MARKER)?;
    std::fs::rename(temporary, journal.join("committed"))?;
    super::sync_directory(journal);
    Ok(())
}

fn prepare(journal: &Path) -> io::Result<()> {
    write_new(&journal.join("ready.new"), MARKER)?;
    std::fs::rename(journal.join("ready.new"), journal.join("ready"))?;
    super::sync_directory(journal);
    Ok(())
}

fn validate(journal: &Path) -> io::Result<()> {
    crate::profile_security::verify_directory(journal)?;
    // Bound and validate the namespace before deleting any entry. Never walk
    // recursively or remove an unexpected file from a recovery directory.
    for (index, entry) in std::fs::read_dir(journal)?.enumerate() {
        let entry = entry?;
        if index >= ENTRIES.len() || !ENTRIES.iter().any(|name| entry.file_name() == *name) {
            return Err(invalid());
        }
        read(&entry.path())?.ok_or_else(invalid)?;
    }
    Ok(())
}

fn cleanup(profile: &Path, journal: &Path) -> io::Result<()> {
    validate(journal)?;
    // Remove the commit marker last: partial cleanup of a committed migration
    // can never turn its remaining originals into a rollback instruction.
    for name in ENTRIES.iter().filter(|name| **name != "committed") {
        match std::fs::remove_file(journal.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    std::fs::remove_file(journal.join("committed")).or_else(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error)
        }
    })?;
    std::fs::remove_dir(journal)?;
    super::sync_directory(profile);
    Ok(())
}

fn restore(profile: &Path, journal: &Path, count: usize) -> io::Result<()> {
    // Validate every original before replacing either target.
    let originals: Vec<_> = (0..NAMES.len())
        .map(|index| {
            let original = read(&journal.join(format!("{index}.original")))?;
            let absent = marker(&journal.join(format!("{index}.absent")))?;
            if original.is_some() == absent {
                return Err(invalid());
            }
            Ok(original)
        })
        .collect::<io::Result<_>>()?;
    for (index, name) in NAMES.iter().enumerate().take(count) {
        let target = profile.join(name);
        if let Some(bytes) = &originals[index] {
            let temporary = journal.join(format!("{index}.restore"));
            if read(&temporary)?.is_some() {
                std::fs::remove_file(&temporary)?;
            }
            write_new(&temporary, bytes)?;
            std::fs::rename(temporary, target)?;
        } else {
            match std::fs::remove_file(target) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    super::sync_directory(profile);
    Ok(())
}

/// Call under the renewal lock, before reading or renewing the generation.
/// An interrupted migration restores its exact originals; a committed one
/// only removes its protected rollback slot.
pub(super) fn recover(profile: &Path) -> io::Result<()> {
    let journal = profile.join(JOURNAL);
    match std::fs::symlink_metadata(&journal) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(_) => validate(&journal)?,
    }
    if !marker(&journal.join("committed"))? && marker(&journal.join("ready"))? {
        restore(profile, &journal, NAMES.len())?;
        commit(&journal)?;
    }
    cleanup(profile, &journal)
}

pub(super) fn migrate(profile: &Path, tokens: &super::Tokens) -> io::Result<()> {
    recover(profile)?;
    let mut protected = true;
    for name in NAMES {
        protected &= read(&profile.join(name))?
            .is_some_and(|bytes| crate::profile_security::is_protected(&bytes));
    }
    if protected {
        return Ok(());
    }
    // Both seals and directory protection must succeed before any credential
    // content changes. Current-user DPAPI receives bytes over the fixed pipe.
    crate::profile_security::protect_directory(profile)?;
    let state = crate::profile_security::seal(&super::encode(tokens))?;
    let credential = crate::profile_security::seal(tokens.access.as_bytes())?;
    publish(profile, [&credential, &state], |_| Ok(()))
}

fn publish(
    profile: &Path,
    pair: [&[u8]; 2],
    mut after_publish: impl FnMut(usize) -> io::Result<()>,
) -> io::Result<()> {
    if pair.iter().any(|bytes| bytes.len() > 18 * 1024) {
        return Err(invalid());
    }
    let journal = profile.join(JOURNAL);
    std::fs::create_dir(&journal)?;
    if let Err(error) = crate::profile_security::protect_directory(&journal) {
        let _ = std::fs::remove_dir(&journal);
        return Err(error);
    }
    let mut published = 0;
    let result = (|| {
        for (index, name) in NAMES.iter().enumerate() {
            match read(&profile.join(name))? {
                Some(bytes) => write_new(&journal.join(format!("{index}.original")), &bytes)?,
                None => write_new(&journal.join(format!("{index}.absent")), MARKER)?,
            }
            write_new(&journal.join(format!("{index}.new")), pair[index])?;
        }
        prepare(&journal)?;
        super::sync_directory(profile);
        after_publish(0)?;
        for (index, name) in NAMES.iter().enumerate() {
            std::fs::rename(journal.join(format!("{index}.new")), profile.join(name))?;
            published += 1;
            after_publish(published)?;
        }
        super::sync_directory(profile);
        commit(&journal)?;
        Ok(())
    })();
    if let Err(error) = result {
        if published != 0 && restore(profile, &journal, published).is_err() {
            return Err(io::Error::other(Failure::RollbackPending));
        }
        if marker(&journal.join("ready"))? {
            commit(&journal)?;
        }
        // No successful publication can be reported as a failed upgrade.
        // If cleanup is interrupted, the ready slot safely repeats rollback.
        let _ = cleanup(profile, &journal);
        return Err(error);
    }
    // A committed slot is safe to retain if cleanup cannot complete now.
    let _ = cleanup(profile, &journal);
    Ok(())
}

#[cfg(windows)]
pub(super) fn failure_code(error: &io::Error) -> &'static str {
    match error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<Failure>())
    {
        Some(Failure::InvalidJournal) => "invalid-journal",
        Some(Failure::RollbackPending) => "rollback-pending; restart this build before downgrading",
        None => crate::profile_security::failure_code(error),
    }
}

#[cfg(windows)]
pub(super) fn projection_is_protected(profile: &Path) -> bool {
    // Only controls whether the authoritative generation may skip replacement.
    // An unreadable or oversized projection still takes the existing repair path.
    read(&profile.join(super::CREDENTIAL_FILE))
        .is_ok_and(|bytes| bytes.is_some_and(|bytes| crate::profile_security::is_protected(&bytes)))
}

#[cfg(test)]
#[path = "migration_tests.rs"]
mod tests;
