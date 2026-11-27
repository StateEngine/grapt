//! One activation record and recovery protocol, under the exclusive prefix lock.

#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{io, package, progress};

const RECORD_BYTES: u64 = 16 * 1024;
const HISTORY_BYTES: u64 = 2 * 1024 * 1024;
const STATE_SCHEMA: &str = "secode.install.state.v1";
const INTENT_SCHEMA: &str = "secode.install.transition.v1";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
    schema: String,
    pub(super) current: String,
    pub(super) previous: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema: String,
    from: Option<String>,
    to: String,
}

fn optional(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => io::read(path, RECORD_BYTES).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "installation record is unreadable: {:?}",
            error.kind()
        )),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn pointer(prefix: &Path) -> Result<Option<String>, String> {
    io::directory(prefix)?;
    let path = prefix.join("current");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {}
        Ok(_) => return Err("installation current must be a version symlink".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "installation current is unreadable: {:?}",
                error.kind()
            ))
        }
    }
    let target = std::fs::read_link(path)
        .map_err(|error| format!("installation current cannot be read: {:?}", error.kind()))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| package::commit(name))
        .ok_or("installation current names an invalid version")?;
    if target != Path::new("versions").join(name) {
        return Err("installation current is outside its version directory".into());
    }
    for path in [
        prefix.join("versions"),
        prefix.join(&target),
        prefix.join(&target).join("bin"),
    ] {
        io::directory(&path)?;
    }
    Ok(Some(name.to_owned()))
}

#[cfg(windows)]
pub(super) fn pointer(prefix: &Path) -> Result<Option<String>, String> {
    let identity = super::windows_pointer::read(prefix)?;
    if let Some(identity) = &identity {
        let version = prefix.join("versions").join(identity);
        for path in [
            prefix.join("versions"),
            version.clone(),
            version.join("bin"),
        ] {
            io::directory(&path)?;
        }
    }
    Ok(identity)
}

fn recorded_state(prefix: &Path) -> Result<Option<State>, String> {
    let Some(bytes) = optional(&prefix.join("state.json"))? else {
        return Ok(None);
    };
    let state: State =
        serde_json::from_slice(&bytes).map_err(|_| "installation state is malformed")?;
    if state.schema != STATE_SCHEMA
        || !package::commit(&state.current)
        || state
            .previous
            .as_ref()
            .is_some_and(|previous| !package::commit(previous) || previous == &state.current)
    {
        return Err("installation state names an invalid version".into());
    }
    Ok(Some(state))
}

pub(super) fn state(prefix: &Path) -> Result<Option<State>, String> {
    let state = recorded_state(prefix)?;
    if pointer(prefix)?.as_ref() != state.as_ref().map(|state| &state.current) {
        return Err("installation state disagrees with its current version".into());
    }
    Ok(state)
}

fn verify_predecessor(prefix: &Path, intent: &Intent) -> Result<(), String> {
    let agrees = match recorded_state(prefix)? {
        None => intent.from.is_none(),
        Some(state) => {
            Some(&state.current) == intent.from.as_ref()
                || (state.current == intent.to && state.previous == intent.from)
        }
    };
    if !agrees {
        return Err("installation state disagrees with its intent predecessor".into());
    }
    Ok(())
}

fn encoded<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|_| "installation record cannot be encoded".into())
}

#[cfg(target_os = "linux")]
fn history(prefix: &Path, intent: &Intent) -> Result<(), String> {
    let path = prefix.join("history.jsonl");
    let mut bytes = encoded(intent)?;
    bytes.push(b'\n');
    let before = match std::fs::symlink_metadata(&path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.nlink() == 1 =>
        {
            metadata.len()
        }
        Ok(_) => return Err("installation history must be a regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(format!(
                "installation history is unreadable: {:?}",
                error.kind()
            ))
        }
    };
    if before > HISTORY_BYTES.saturating_sub(bytes.len() as u64) {
        return Err("installation history is full; archive history.jsonl before retrying".into());
    }
    io::directory(prefix)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("installation history cannot be opened: {:?}", error.kind()))?;
    let opened = file.metadata().map_err(|error| {
        format!(
            "installation history cannot be identified: {:?}",
            error.kind()
        )
    })?;
    let named = std::fs::symlink_metadata(prefix.join("history.jsonl")).map_err(|error| {
        format!(
            "installation history cannot be identified: {:?}",
            error.kind()
        )
    })?;
    if !opened.is_file()
        || opened.nlink() != 1
        || named.file_type().is_symlink()
        || named.dev() != opened.dev()
        || named.ino() != opened.ino()
    {
        return Err("installation history must be a regular file".into());
    }
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("installation history cannot be synced: {:?}", error.kind()))?;
    io::sync_directory(prefix)
}

// Replace the bounded Windows log instead of appending through a possible
// hard-link alias. History records observations; it never chooses recovery.
#[cfg(windows)]
fn history_bytes(prefix: &Path) -> Result<Vec<u8>, String> {
    let path = prefix.join("history.jsonl");
    match std::fs::symlink_metadata(&path) {
        Ok(_) => io::read(&path, HISTORY_BYTES),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!(
            "installation history is unreadable: {:?}",
            error.kind()
        )),
    }
}

#[cfg(windows)]
fn history_capacity(prefix: &Path, extra: usize) -> Result<Vec<u8>, String> {
    let bytes = history_bytes(prefix)?;
    if bytes.len() as u64 > HISTORY_BYTES.saturating_sub(extra as u64) {
        return Err("installation history is full; archive history.jsonl before retrying".into());
    }
    Ok(bytes)
}

#[cfg(windows)]
fn history(prefix: &Path, intent: &Intent) -> Result<(), String> {
    let mut row = encoded(intent)?;
    row.push(b'\n');
    let mut bytes = history_capacity(prefix, row.len())?;
    bytes.extend_from_slice(&row);
    io::replace(&prefix.join("history.jsonl"), &bytes)
}

fn finish(prefix: &Path, intent: &Intent) -> Result<(), String> {
    io::replace(
        &prefix.join("state.json"),
        &encoded(&State {
            schema: STATE_SCHEMA.to_owned(),
            current: intent.to.clone(),
            previous: intent.from.clone(),
        })?,
    )?;
    history(prefix, intent)?;
    std::fs::remove_file(prefix.join("transition.json"))
        .map_err(|error| format!("installation intent cannot be removed: {:?}", error.kind()))?;
    io::sync_directory(prefix)
}

fn intent(prefix: &Path) -> Result<Option<Intent>, String> {
    let Some(bytes) = optional(&prefix.join("transition.json"))? else {
        return Ok(None);
    };
    let intent: Intent =
        serde_json::from_slice(&bytes).map_err(|_| "installation intent is malformed")?;
    if intent.schema != INTENT_SCHEMA
        || !package::commit(&intent.to)
        || intent
            .from
            .as_ref()
            .is_some_and(|from| !package::commit(from) || from == &intent.to)
    {
        return Err("installation intent names an invalid transition".into());
    }
    Ok(Some(intent))
}

// Activation order, read by retention (prune) and by the launcher notice's
// wording. This cannot choose activation recovery, and malformed or
// unreadable history is never treated as missing.
pub(super) fn activations(prefix: &Path) -> Result<Vec<String>, String> {
    let path = prefix.join("history.jsonl");
    let bytes = match std::fs::symlink_metadata(&path) {
        Ok(_) => io::read(&path, HISTORY_BYTES)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "installation history is unreadable: {:?}",
                error.kind()
            ))
        }
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| "installation history is not UTF-8")?;
    let mut activations = Vec::new();
    for line in text.lines() {
        if line.is_empty() || line.len() as u64 > RECORD_BYTES {
            return Err("installation history has an invalid record bound".into());
        }
        let row: Intent =
            serde_json::from_str(line).map_err(|_| "installation history is malformed")?;
        if row.schema != INTENT_SCHEMA
            || !package::commit(&row.to)
            || row
                .from
                .as_ref()
                .is_some_and(|from| !package::commit(from) || from == &row.to)
        {
            return Err("installation history names an invalid activation".into());
        }
        activations.push(row.to);
    }
    Ok(activations)
}

// Preview the same recovery decision without writing. Actual recovery still
// occurs under activation exclusion, and the chosen predecessor is checked again.
pub(super) fn revert_predecessor(prefix: &Path) -> Result<Option<String>, String> {
    let planned = if let Some(intent) = intent(prefix)? {
        let active = pointer(prefix)?;
        if active.as_ref() == Some(&intent.to) {
            verify_predecessor(prefix, &intent)?;
            Some(State {
                schema: STATE_SCHEMA.to_owned(),
                current: intent.to,
                previous: intent.from,
            })
        } else if active == intent.from {
            state(prefix)?
        } else {
            return Err("installation current disagrees with both sides of its intent".into());
        }
    } else {
        state(prefix)?
    };
    Ok(planned.and_then(|state| state.previous))
}

pub(super) fn verified_current(prefix: &Path) -> Result<Option<package::Package>, String> {
    verified_current_reporting(prefix, None)
}

pub(super) fn verified_current_reporting(
    prefix: &Path,
    reporter: Option<&progress::Reporter>,
) -> Result<Option<package::Package>, String> {
    pointer(prefix)?
        .map(|current| {
            package::Package::verify_with(
                &prefix.join("versions").join(current).join("bin"),
                reporter,
            )
        })
        .transpose()
}

#[cfg(test)]
pub(super) fn recover(prefix: &Path) -> Result<(), String> {
    let verified = verified_current(prefix)?;
    recover_verified(prefix, verified.as_ref())
}

pub(super) fn recover_verified(
    prefix: &Path,
    verified: Option<&package::Package>,
) -> Result<(), String> {
    let current = pointer(prefix)?;
    if current.as_deref() != verified.map(|package| package.commit.as_str()) {
        return Err("active installation changed during verification; retry".into());
    }
    if let Some(package) = verified {
        package.recheck(&prefix.join("versions").join(&package.commit).join("bin"))?;
    }
    let Some(intent) = intent(prefix)? else {
        state(prefix)?;
        return Ok(());
    };
    let active = pointer(prefix)?;
    if active.as_ref() == Some(&intent.to) {
        verify_predecessor(prefix, &intent)?;
        finish(prefix, &intent)?;
    } else if active == intent.from {
        state(prefix)?;
        std::fs::remove_file(prefix.join("transition.json")).map_err(|error| {
            format!("installation intent cannot be removed: {:?}", error.kind())
        })?;
        io::sync_directory(prefix)?;
    } else {
        return Err("installation current disagrees with both sides of its intent".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Boundary {
    Intent,
    Pointer,
    State,
}

pub(super) fn activate_verified(prefix: &Path, verified: &package::Package) -> Result<(), String> {
    activate_verified_with(prefix, verified, |_| Ok(()))
}

#[cfg(test)]
pub(super) fn activate(prefix: &Path, to: &str) -> Result<(), String> {
    activate_with(prefix, to, |_| Ok(()))
}

// The closure is injected by unit fixtures only; normal callers never expose
// an environment-driven crash switch or a second activation path.
#[cfg(test)]
pub(super) fn activate_with(
    prefix: &Path,
    to: &str,
    boundary: impl Fn(Boundary) -> Result<(), String>,
) -> Result<(), String> {
    let verified = package::Package::verify(&prefix.join("versions").join(to).join("bin"))?;
    activate_verified_with(prefix, &verified, boundary)
}

fn activate_verified_with(
    prefix: &Path,
    verified: &package::Package,
    boundary: impl Fn(Boundary) -> Result<(), String>,
) -> Result<(), String> {
    let to = verified.commit.as_str();
    if !package::commit(to) {
        return Err("installation target identity is invalid".into());
    }
    let current = state(prefix)?.map(|state| state.current);
    if current.as_deref() == Some(to) {
        return Ok(());
    }
    let installed = prefix.join("versions").join(to).join("bin");
    // Full hashing happened before activation exclusion. Keep this check cheap.
    verified.recheck(&installed)?;
    let intent = Intent {
        schema: INTENT_SCHEMA.to_owned(),
        from: current,
        to: to.to_owned(),
    };
    // Capacity is checked before activation; history never decides recovery.
    #[cfg(target_os = "linux")]
    {
        match std::fs::symlink_metadata(prefix.join("history.jsonl")) {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.nlink() == 1
                    && metadata.len()
                        <= HISTORY_BYTES.saturating_sub(encoded(&intent)?.len() as u64 + 1) => {}
            Ok(_) => return Err(
                "installation history is nonregular or full; archive history.jsonl before retrying"
                    .into(),
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "installation history is unreadable: {:?}",
                    error.kind()
                ))
            }
        }
    }
    #[cfg(windows)]
    history_capacity(prefix, encoded(&intent)?.len() + 1)?;
    // Intent is create-new. An existing transition must be recovered first.
    io::write_new(&prefix.join("transition.json"), &encoded(&intent)?)?;
    io::sync_directory(prefix)?;
    boundary(Boundary::Intent)?;
    switch_pointer(prefix, to)?;
    boundary(Boundary::Pointer)?;
    io::replace(
        &prefix.join("state.json"),
        &encoded(&State {
            schema: STATE_SCHEMA.to_owned(),
            current: intent.to.clone(),
            previous: intent.from.clone(),
        })?,
    )?;
    boundary(Boundary::State)?;
    history(prefix, &intent)?;
    std::fs::remove_file(prefix.join("transition.json"))
        .map_err(|error| format!("installation intent cannot be removed: {:?}", error.kind()))?;
    io::sync_directory(prefix)
}

#[cfg(target_os = "linux")]
fn switch_pointer(prefix: &Path, to: &str) -> Result<(), String> {
    let temporary = prefix.join("current.new");
    match std::fs::symlink_metadata(&temporary) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            std::fs::remove_file(&temporary).map_err(|error| {
                format!(
                    "installation pointer temporary cannot be removed: {:?}",
                    error.kind()
                )
            })?;
        }
        Ok(_) => return Err("installation pointer temporary is not a symlink".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "installation pointer temporary is unreadable: {:?}",
                error.kind()
            ))
        }
    }
    std::os::unix::fs::symlink(Path::new("versions").join(to), &temporary)
        .map_err(|error| format!("installation pointer cannot be created: {:?}", error.kind()))?;
    std::fs::rename(temporary, prefix.join("current")).map_err(|error| {
        format!(
            "installation pointer cannot be switched: {:?}",
            error.kind()
        )
    })?;
    io::sync_directory(prefix)?;
    Ok(())
}

#[cfg(windows)]
fn switch_pointer(prefix: &Path, to: &str) -> Result<(), String> {
    super::windows_pointer::publish(prefix, to)
}
