//! USER Path ownership. Only an isolated, time-bounded worker writes HKCU.
//! Path comparison reuses the bounded Path-Contains check.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Modified: preserve original segments and registry type; remove only our entry.
use super::io;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[path = "user_path_resolution.rs"]
mod resolution;

const CAP: usize = 65_534;
const RECORD: &str = "user-path.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    // None means the registry value was absent, not an empty REG_SZ.
    kind: Option<bool>, // true: REG_EXPAND_SZ; false: REG_SZ.
    bytes: Vec<u8>,
}

impl Snapshot {
    fn text(&self) -> Result<String, String> {
        if self.bytes.len() > CAP || !self.bytes.len().is_multiple_of(2) {
            return Err("USER Path is outside its UTF-16 byte bound".into());
        }
        if self.kind.is_none() {
            return if self.bytes.is_empty() {
                Ok(String::new())
            } else {
                Err("absent USER Path carries bytes".into())
            };
        }
        let units: Vec<u16> = self
            .bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let units = units
            .strip_suffix(&[0])
            .ok_or("USER Path is not terminated UTF-16")?;
        if units.contains(&0) {
            return Err("USER Path contains an embedded NUL".into());
        }
        String::from_utf16(units).map_err(|_| "USER Path contains invalid UTF-16".into())
    }
    fn with_text(&self, text: &str) -> Result<Self, String> {
        let value = Self {
            kind: Some(self.kind.unwrap_or(false)),
            bytes: text
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect(),
        };
        value.text()?;
        Ok(value)
    }
}

fn normalize(entry: &str) -> String {
    entry
        .trim()
        .trim_matches('"')
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_lowercase()
}
fn contains(text: &str, entry: &str) -> bool {
    let entry = normalize(entry);
    text.split(';')
        .any(|part| !part.is_empty() && normalize(part) == entry)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    schema: String,
    entry: String,
    before: Snapshot,
    after: Snapshot,
    committed: bool,
}

#[cfg(not(test))]
fn read() -> Result<Snapshot, String> {
    use winreg::{
        enums::{HKEY_CURRENT_USER, KEY_READ},
        RegKey,
    };
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ)
        .map_err(|error| format!("USER environment cannot be opened: {:?}", error.kind()))?;
    read_value(key, "USER", "Path")
}

#[cfg(not(test))]
fn machine() -> Result<Snapshot, String> {
    use winreg::{
        enums::{HKEY_LOCAL_MACHINE, KEY_READ},
        RegKey,
    };
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
            KEY_READ,
        )
        .map_err(|error| format!("System environment cannot be opened: {:?}", error.kind()))?;
    read_value(key, "System", "Path")
}

#[cfg(not(test))]
fn read_value(key: winreg::RegKey, scope: &str, name: &str) -> Result<Snapshot, String> {
    use winreg::enums::{REG_EXPAND_SZ, REG_SZ};
    if key
        .query_info()
        .map_err(|error| error.to_string())?
        .max_value_len as usize
        > CAP
    {
        return Err(format!(
            "{scope} environment has a value above the registry read bound"
        ));
    }
    let value = match key.get_raw_value(name) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Snapshot {
                kind: None,
                bytes: Vec::new(),
            })
        }
        Err(error) => return Err(format!("{scope} {name} is unreadable: {:?}", error.kind())),
    };
    let kind = if value.vtype == REG_EXPAND_SZ {
        true
    } else if value.vtype == REG_SZ {
        false
    } else {
        return Err(format!(
            "{scope} {name} must be REG_SZ or REG_EXPAND_SZ; it was not changed"
        ));
    };
    let snapshot = Snapshot {
        kind: Some(kind),
        bytes: value.bytes.into_owned(),
    };
    snapshot.text()?;
    Ok(snapshot)
}

/// Resolve before USER environment merging. Never consult inherited process
/// values: a USER override may already have replaced the System value there.
#[cfg(not(test))]
fn system_value(name: &str) -> Result<Option<String>, String> {
    use winreg::{
        enums::{HKEY_LOCAL_MACHINE, KEY_READ},
        RegKey,
    };
    if name.is_empty() || name.len() > 256 || name.contains(['\\', '/', '=', '\0']) {
        return Err("System Path contains an invalid variable name".into());
    }
    let system = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
            KEY_READ,
        )
        .map_err(|error| format!("System environment is unreadable: {:?}", error.kind()))?;
    let mut value = read_value(system, "System", name)?;
    if value.kind.is_none()
        && (name.eq_ignore_ascii_case("SystemRoot") || name.eq_ignore_ascii_case("windir"))
    {
        let windows = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", KEY_READ)
            .map_err(|error| {
                format!("System Windows directory is unreadable: {:?}", error.kind())
            })?;
        value = read_value(windows, "System", "SystemRoot")?;
    }
    if value.kind.is_none() {
        return Ok(None);
    }
    let text = value.text()?;
    if value.kind == Some(true) && text.contains('%') {
        return Err("System Path depends on a nested expandable System variable; use a literal System value before installation".into());
    }
    Ok(Some(text))
}

#[cfg(not(test))]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Change {
    before: Snapshot,
    after: Snapshot,
    entry: String,
    installing: bool,
}

/// Internal CLI worker; the parent bounds the third-party registry mutex and
/// the WM_SETTINGCHANGE broadcast. No payload or user-supplied code executes.
#[cfg(windows)]
pub fn worker() -> i32 {
    #[cfg(not(test))]
    let result = (|| -> Result<(), String> {
        use std::io::Read;
        let mut raw = Vec::new();
        std::io::stdin()
            .take((8 * CAP + 16 * 1024 + 1) as u64)
            .read_to_end(&mut raw)
            .map_err(|error| error.to_string())?;
        if raw.len() > 8 * CAP + 16 * 1024 {
            return Err("PATH worker input exceeds its bound".into());
        }
        let local = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or("LOCALAPPDATA is unknown")?;
        io::directory(&local)?;
        let lock = local.join("secode-path-writer");
        super::commands::prefix_directory(&lock)?;
        let _guard = grapt_service::installation::PrefixGuard::exclusive(&lock)
            .map_err(|error| format!("another Grapt USER Path writer is active: {error}"))?;
        let change: Change =
            serde_json::from_slice(&raw).map_err(|_| "PATH worker input is malformed")?;
        let text = change.after.text()?;
        change.before.text()?;
        if read()? != change.before {
            return Err("USER Path changed concurrently; retry installation".into());
        }
        if !Path::new(&change.entry).is_absolute() || change.entry.len() > 4096 {
            return Err("PATH worker command directory is invalid".into());
        }
        if change.installing {
            let system = machine()?;
            let bundled = match std::fs::symlink_metadata(Path::new(&change.entry).join("se.exe")) {
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => {
                    return Err(format!(
                        "bundled launcher cannot be inspected: {:?}",
                        error.kind()
                    ))
                }
            };
            resolution::verify(
                &system.text()?,
                system.kind == Some(true),
                &change.entry,
                bundled,
            )?;
        }
        if change.before == change.after {
            return Ok(());
        }
        match change.after.kind {
            Some(true) => windows_env::set_expand_string("Path", text),
            Some(false) => windows_env::set("Path", text),
            None => windows_env::remove("Path"),
        }
        .map_err(|error| error.to_string())?;
        if read()? != change.after {
            return Err("USER Path write did not retain its exact registry type and bytes".into());
        }
        Ok(())
    })();
    #[cfg(test)]
    let result: Result<(), String> =
        Err("native PATH worker is qualified by the snapshot/restore walk".into());
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("grapt: {error}");
            2
        }
    }
}

#[cfg(not(test))]
async fn write(change: Change) -> Result<(), String> {
    use std::{process::Stdio, time::Duration};
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::from(grapt_service::launcher_environment::command(
        std::env::current_exe().map_err(|error| error.to_string())?,
    ))
    .arg("--installation-user-path")
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::inherit())
    .kill_on_drop(true)
    .spawn()
    .map_err(|error| format!("PATH worker cannot start: {:?}", error.kind()))?;
    let mut input = child.stdin.take().ok_or("PATH worker has no input pipe")?;
    let raw = serde_json::to_vec(&change).map_err(|_| "PATH worker input cannot be encoded")?;
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        input
            .write_all(&raw)
            .await
            .map_err(|error| error.to_string())?;
        drop(input);
        child.wait().await.map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(_)) => Err(
            "USER Path worker refused the change; resolve its diagnostic and rerun installation"
                .into(),
        ),
        Ok(Err(error)) => Err(error),
        Err(_) => {
            child.start_kill().map_err(|error| error.to_string())?;
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .map_err(|_| "PATH worker could not be reaped within five seconds")?
                .map_err(|error| error.to_string())?;
            Err("USER Path worker exceeded 15 seconds; rerun installation to reconcile its recorded change".into())
        }
    }
}

fn ownership(prefix: &Path, entry: &str) -> Result<Option<Ownership>, String> {
    let path = prefix.join(RECORD);
    let record: Ownership = match std::fs::symlink_metadata(&path) {
        Ok(_) => serde_json::from_slice(&io::read(&path, (16 * CAP) as u64)?)
            .map_err(|_| "USER Path ownership record is malformed")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "USER Path ownership is unreadable: {:?}",
                error.kind()
            ))
        }
    };
    if record.schema != "secode.install.user-path.v1" || record.entry != entry {
        return Err("USER Path ownership does not match this installation".into());
    }
    record.before.text()?;
    record.after.text()?;
    let expected = plan_install(&record.before, entry, None)?;
    if expected.is_none_or(|expected| expected.after != record.after) {
        return Err("USER Path ownership does not attest an added command directory".into());
    }
    Ok(Some(record))
}

fn plan_install(
    current: &Snapshot,
    entry: &str,
    previous: Option<&Ownership>,
) -> Result<Option<Ownership>, String> {
    if entry.is_empty()
        || entry.len() > 4096
        || entry.contains([';', '\0', '"'])
        || entry.chars().any(char::is_control)
    {
        return Err("launcher directory cannot be represented as a USER Path entry".into());
    }
    let text = current.text()?;
    if current.kind == Some(true) && entry.contains('%') {
        return Err("launcher directory contains a percent sign that REG_EXPAND_SZ could expand; choose a prefix without percent signs".into());
    }
    if let Some(record) = previous.filter(|record| !record.committed) {
        if *current == record.after {
            return Ok(Some(Ownership {
                schema: record.schema.clone(),
                entry: record.entry.clone(),
                before: record.before.clone(),
                after: record.after.clone(),
                committed: true,
            }));
        }
        if *current != record.before {
            return Err("USER Path changed since interrupted installation; its owned intent requires inspection".into());
        }
    }
    if contains(&text, entry) {
        // Preserve an entry we did not add. Refuse ambiguous precedence rather
        // than silently moving it or accepting an earlier shadowing command.
        if text
            .split(';')
            .next()
            .is_none_or(|first| normalize(first) != normalize(entry))
        {
            return Err("the command directory already occurs behind other USER Path entries that may shadow se or grapt; move that existing entry to the front and retry".into());
        }
        return Ok(None);
    }
    let after = current.with_text(&format!(
        "{entry}{}{text}",
        if text.is_empty() { "" } else { ";" }
    ))?;
    Ok(Some(Ownership {
        schema: "secode.install.user-path.v1".into(),
        entry: entry.into(),
        before: current.clone(),
        after,
        committed: false,
    }))
}

fn plan_remove(current: &Snapshot, record: &Ownership) -> Result<Snapshot, String> {
    if *current == record.after {
        return Ok(record.before.clone());
    }
    if !record.committed && *current != record.before {
        return Err(
            "USER Path changed since interrupted installation; removal requires inspection".into(),
        );
    }
    let text = current.text()?;
    let parts: Vec<_> = text.split(';').collect();
    let matches = parts.iter().filter(|part| **part == record.entry).count();
    if matches > 1 {
        return Err("USER Path has duplicate owned entries; removal requires inspection".into());
    }
    if matches == 0 {
        return Ok(current.clone());
    }
    current.with_text(
        &parts
            .into_iter()
            .filter(|part| *part != record.entry)
            .collect::<Vec<_>>()
            .join(";"),
    )
}

#[cfg(not(test))]
pub(super) async fn install(prefix: &Path) -> Result<String, String> {
    let entry = super::display_path(&prefix.join("bin"));
    let current = read()?;
    let previous = ownership(prefix, &entry)?;
    if let Some(mut record) = plan_install(&current, &entry, previous.as_ref())? {
        let save = |record: &Ownership| {
            io::replace(
                &prefix.join(RECORD),
                &serde_json::to_vec(record).map_err(|_| "USER Path ownership cannot be encoded")?,
            )
        };
        save(&record)?;
        write(Change {
            before: current,
            after: record.after.clone(),
            entry: entry.clone(),
            installing: true,
        })
        .await?;
        record.committed = true;
        save(&record)?;
    } else {
        // A no-op still needs effective resolution: System Path precedes USER.
        write(Change {
            before: current.clone(),
            after: current,
            entry,
            installing: true,
        })
        .await?;
    }
    Ok("Commands are on USER Path. Open a new terminal to use them.".into())
}

#[cfg(not(test))]
pub(super) async fn uninstall(prefix: &Path) -> Result<String, String> {
    let entry = super::display_path(&prefix.join("bin"));
    if let Some(record) = ownership(prefix, &entry)? {
        let current = read()?;
        let after = plan_remove(&current, &record)?;
        if current != after {
            write(Change {
                before: current,
                after,
                entry: entry.clone(),
                installing: false,
            })
            .await?;
        }
    }
    Ok(
        "Removed this installation's owned USER Path entry. Versions and profiles were retained."
            .into(),
    )
}

// Unit tests never write the account's real registry. The executable walk uses
// the production adapter, with a raw registry snapshot restored in finally.
#[cfg(all(test, windows))]
pub(super) async fn install(_prefix: &Path) -> Result<String, String> {
    Ok("Native USER Path changes are covered by the snapshot/restore executable walk.".into())
}
#[cfg(all(test, windows))]
pub(super) async fn uninstall(_prefix: &Path) -> Result<String, String> {
    Ok("Native USER Path removal is covered by the snapshot/restore executable walk.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expand_strings_and_empty_segments_round_trip_without_expansion() -> Result<(), String> {
        let snapshot = Snapshot {
            kind: Some(true),
            bytes: Vec::new(),
        }
        .with_text(r"%TOOLS%\bin;;C:\Other;")?;
        assert_eq!(snapshot.text()?, r"%TOOLS%\bin;;C:\Other;");
        assert_eq!(snapshot.kind, Some(true));
        assert!(contains(
            r#"C:\Other;"c:/users/example/bin/";"#,
            r"C:\Users\example\bin"
        ));
        assert!(!contains(
            r"C:\Users\example\bin-extra",
            r"C:\Users\example\bin"
        ));
        Ok(())
    }
    #[test]
    fn malformed_or_oversized_registry_values_refuse() {
        for bytes in [
            vec![0],
            vec![65, 0],
            vec![0, 0, 0, 0],
            vec![0, 216, 0, 0],
            vec![0; CAP + 2],
        ] {
            assert!(Snapshot {
                kind: Some(false),
                bytes
            }
            .text()
            .is_err());
        }
        assert!(Snapshot {
            kind: None,
            bytes: vec![0, 0]
        }
        .text()
        .is_err());
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    fn snapshot(text: &str) -> Result<Snapshot, String> {
        Snapshot {
            kind: Some(true),
            bytes: Vec::new(),
        }
        .with_text(text)
    }
    #[test]
    fn existing_entries_are_never_claimed_and_empty_segments_survive() -> Result<(), String> {
        let current = snapshot(r#""C:/My Tool/bin/";%TOOLS%;;"#)?;
        assert!(plan_install(&current, r"c:\my tool\bin", None)?.is_none());
        let plan = plan_install(&current, r"C:\New\bin", None)?.ok_or("missing addition")?;
        assert_eq!(
            plan.after.text()?,
            r#"C:\New\bin;"C:/My Tool/bin/";%TOOLS%;;"#
        );
        assert_eq!(plan.after.kind, Some(true));
        assert_eq!(plan_remove(&plan.after, &plan)?, current);
        Ok(())
    }
    #[test]
    fn a_preexisting_command_directory_must_have_unambiguous_precedence() -> Result<(), String> {
        let entry = r"C:\Owned\bin";
        let before = snapshot(r"C:\Shadow;C:\Owned\bin;C:\Other")?;
        assert!(
            plan_install(&before, entry, None).is_err_and(|reason| reason.contains("may shadow")),
            "shadowed command directory was accepted"
        );
        assert!(plan_install(&snapshot(r"C:\Owned\bin;C:\Shadow")?, entry, None)?.is_none());
        assert_eq!(before.text()?, r"C:\Shadow;C:\Owned\bin;C:\Other");
        Ok(())
    }
    #[test]
    fn percent_directories_cannot_change_meaning_in_an_expandable_path() -> Result<(), String> {
        let entry = r"C:\%OTHER%\bin";
        assert!(
            plan_install(&snapshot(r"C:\Other")?, entry, None)
                .is_err_and(|reason| reason.contains("REG_EXPAND_SZ")),
            "expandable Path accepted a literal percent directory"
        );
        let literal = Snapshot {
            kind: Some(false),
            bytes: Vec::new(),
        }
        .with_text(r"C:\Other")?;
        let plan = plan_install(&literal, entry, None)?.ok_or("literal REG_SZ addition absent")?;
        assert_eq!(plan.after.kind, Some(false));
        assert_eq!(plan.after.text()?, r"C:\%OTHER%\bin;C:\Other");
        Ok(())
    }
    #[test]
    fn committed_removal_preserves_foreign_edits_and_modified_entries() -> Result<(), String> {
        let before = snapshot(r"%TOOLS%;;C:\Other")?;
        let mut plan = plan_install(&before, r"C:\New\bin", None)?.ok_or("missing addition")?;
        plan.committed = true;
        let edited = snapshot(&format!("{};C:\\Later", plan.after.text()?))?;
        assert_eq!(
            plan_remove(&edited, &plan)?.text()?,
            r"%TOOLS%;;C:\Other;C:\Later"
        );
        let edited = snapshot(r"%TOOLS%;c:\new\BIN;C:\Other")?;
        assert_eq!(plan_remove(&edited, &plan)?, edited);
        let duplicate = snapshot(&format!("{};{}", plan.after.text()?, plan.entry))?;
        assert!(plan_remove(&duplicate, &plan).is_err());
        Ok(())
    }
    #[test]
    fn interrupted_writes_reconcile_only_exact_before_or_after() -> Result<(), String> {
        let before = Snapshot {
            kind: None,
            bytes: Vec::new(),
        };
        let plan = plan_install(&before, r"C:\New\bin", None)?.ok_or("missing addition")?;
        assert!(
            !plan_install(&before, &plan.entry, Some(&plan))?
                .ok_or("missing retry")?
                .committed
        );
        assert!(
            plan_install(&plan.after, &plan.entry, Some(&plan))?
                .ok_or("missing reconciliation")?
                .committed
        );
        assert_eq!(plan_remove(&plan.after, &plan)?, before);
        let foreign = snapshot(r"C:\Foreign;C:\New\bin")?;
        assert!(plan_install(&foreign, &plan.entry, Some(&plan)).is_err());
        assert!(plan_remove(&foreign, &plan).is_err());
        Ok(())
    }
    #[test]
    fn ownership_record_is_bounded_and_prefix_bound() -> Result<(), String> {
        let root =
            crate::testfs::ScratchDir::new("path-record").map_err(|error| error.to_string())?;
        let before = snapshot(r"%TOOLS%")?;
        let mut plan = plan_install(&before, r"C:\New\bin", None)?.ok_or("missing addition")?;
        io::replace(
            &root.join(RECORD),
            &serde_json::to_vec(&plan).map_err(|error| error.to_string())?,
        )?;
        assert!(ownership(&root, &plan.entry)?.is_some());
        assert!(ownership(&root, r"C:\Another\bin").is_err());
        plan.before = Snapshot {
            kind: None,
            bytes: Vec::new(),
        };
        io::replace(
            &root.join(RECORD),
            &serde_json::to_vec(&plan).map_err(|error| error.to_string())?,
        )?;
        assert!(
            ownership(&root, &plan.entry).is_err(),
            "forged before snapshot must not authorize removing foreign Path segments"
        );
        Ok(())
    }
}
