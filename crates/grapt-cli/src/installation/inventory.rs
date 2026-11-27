//! The two complete binary inventories across the one-way product rename.
//!
//! A package declares exactly one of them and never a mix. Code that operates
//! on a specific version asks that version's recorded inventory for names
//! instead of assuming the current spelling.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::{io, package, stage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Inventory {
    /// The five `grapt*` binaries.
    Current,
    /// The five pre-rename `secode*` binaries. Only this inventory ever
    /// shipped the retired terminal frontend in place of the native one.
    Legacy,
}

/// Each inventory lists exactly this many binaries.
pub(super) const COUNT: usize = 5;

// Order: server, command-line client, engine daemon, migration operator,
// terminal frontend.
#[cfg(target_os = "linux")]
const CURRENT: [&str; COUNT] = [
    "grapt-server",
    "grapt",
    "grapt-engined",
    "grapt-migrate",
    "grapt-tui",
];
#[cfg(windows)]
const CURRENT: [&str; COUNT] = [
    "grapt-server.exe",
    "grapt.exe",
    "grapt-engined.exe",
    "grapt-migrate.exe",
    "grapt-tui.exe",
];
#[cfg(target_os = "linux")]
const LEGACY: [&str; COUNT] = [
    "secode-server",
    "secode",
    "secode-engined",
    "secode-migrate",
    "secode-tui",
];
#[cfg(windows)]
const LEGACY: [&str; COUNT] = [
    "secode-server.exe",
    "secode.exe",
    "secode-engined.exe",
    "secode-migrate.exe",
    "secode-tui.exe",
];
#[cfg(target_os = "linux")]
const RETIRED_TUI: &str = "secode-tui-r";
#[cfg(windows)]
const RETIRED_TUI: &str = "secode-tui-r.exe";

/// The only compatibility aliases a package may declare, alias -> target:
/// the names StateEngine launches today. They are not signed inventory
/// entries; installation copies each from its verified target.
#[cfg(target_os = "linux")]
const ALIASES: [(&str, &str); 2] = [("secode", "grapt"), ("secode-server", "grapt-server")];
#[cfg(windows)]
const ALIASES: [(&str, &str); 2] = [
    ("secode.exe", "grapt.exe"),
    ("secode-server.exe", "grapt-server.exe"),
];
const REMOVAL_BYTES: usize = 1024;

impl Inventory {
    fn names(self) -> [&'static str; COUNT] {
        match self {
            Self::Current => CURRENT,
            Self::Legacy => LEGACY,
        }
    }

    pub(super) fn server(self) -> &'static str {
        self.names()[0]
    }

    pub(super) fn cli(self) -> &'static str {
        self.names()[1]
    }

    fn expected(self, retired: bool) -> BTreeSet<&'static str> {
        let mut names: BTreeSet<_> = self.names().into_iter().collect();
        if self == Self::Legacy && retired {
            names.remove(LEGACY[4]);
            names.insert(RETIRED_TUI);
        }
        names
    }
}

/// The one inventory whose binary set equals every declared name set (the
/// manifest's `binaries` keys, `entry_points` and `signing` keys). Anything
/// else, including a mix of both spellings, has no inventory.
pub(super) fn declared(sets: &[BTreeSet<&str>]) -> Option<Inventory> {
    let retired = sets.first()?.contains(RETIRED_TUI);
    [Inventory::Current, Inventory::Legacy]
        .into_iter()
        .find(|inventory| {
            let expected = inventory.expected(retired);
            sets.iter().all(|set| *set == expected)
        })
}

/// The inventory a manifest declares in its `binaries` keys, its five
/// `entry_points` and its `signing` keys, when all three agree on one.
pub(super) fn complete<'a>(
    binaries: impl Iterator<Item = &'a String>,
    entry_points: &[String],
    signing: impl Iterator<Item = &'a String>,
) -> Option<Inventory> {
    if entry_points.len() != COUNT {
        return None;
    }
    declared(&[
        binaries.map(String::as_str).collect(),
        entry_points.iter().map(String::as_str).collect(),
        signing.map(String::as_str).collect(),
    ])
}

/// A name no package input other than a binary may take.
pub(super) fn reserved(name: &str) -> bool {
    CURRENT.contains(&name)
        || LEGACY.contains(&name)
        || name == RETIRED_TUI
        || ALIASES.iter().any(|(alias, _)| *alias == name)
}

/// Accept exactly the two compatibility aliases, only in a Current package
/// and only with a removal note. Absence (packages built before the aliases
/// existed, and every pre-rename package) declares none.
pub(super) fn compat_aliases(
    inventory: Inventory,
    aliases: &Value,
    removal: &Value,
) -> Result<&'static [(&'static str, &'static str)], String> {
    match (aliases, removal) {
        (Value::Null, Value::Null) => Ok(&[]),
        (Value::Object(map), Value::String(note))
            if inventory == Inventory::Current
                && !note.trim().is_empty()
                && note.len() <= REMOVAL_BYTES
                && map.len() == ALIASES.len()
                && ALIASES.iter().all(|(alias, target)| {
                    map.get(*alias).and_then(Value::as_str) == Some(*target)
                }) =>
        {
            Ok(&ALIASES)
        }
        _ => Err(
            "package compatibility aliases are not exactly the allowed pair for its inventory"
                .into(),
        ),
    }
}

/// The compatibility alias of `target`, when one is allowed.
pub(super) fn alias_of(target: &str) -> Option<&'static str> {
    ALIASES
        .iter()
        .find(|(_, aliased)| *aliased == target)
        .map(|(alias, _)| *alias)
}

#[derive(Deserialize)]
struct Declared {
    binaries: BTreeMap<String, Value>,
    entry_points: Vec<String>,
    signing: BTreeMap<String, Value>,
}

/// The recorded inventory of an installed version, read from its manifest.
/// Activation verified the content; this only selects that version's names.
pub(super) fn installed(bin: &Path) -> Result<Inventory, String> {
    let bytes = io::read(&bin.join("release-manifest.json"), package::MANIFEST_BYTES)?;
    let manifest: Declared =
        serde_json::from_slice(&bytes).map_err(|_| "installed package manifest is malformed")?;
    complete(
        manifest.binaries.keys(),
        &manifest.entry_points,
        manifest.signing.keys(),
    )
    .ok_or_else(|| "installed package does not declare one known binary inventory".into())
}

/// Copy each declared compatibility alias from its verified target into the
/// version's own `bin`, under the writer lock and before activation. Both
/// platforms use a byte copy re-hashed against the target's signed digest;
/// an existing alias is accepted only when it hashes the same.
pub(super) fn materialize(verified: &package::Package) -> Result<(), String> {
    for (alias, target) in verified.compat_aliases() {
        let input = verified
            .inputs
            .iter()
            .find(|input| input.relative == Path::new(target))
            .ok_or("verified package has no compatibility alias target")?;
        let path = verified.root.join(alias);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let temporary = verified.root.join(format!("{alias}.new"));
                match std::fs::symlink_metadata(&temporary) {
                    Ok(_) => {
                        drop(io::regular(&temporary)?);
                        std::fs::remove_file(&temporary).map_err(|error| {
                            format!(
                                "compatibility alias partial cannot be removed: {:?}",
                                error.kind()
                            )
                        })?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!(
                            "compatibility alias partial is unreadable: {:?}",
                            error.kind()
                        ))
                    }
                }
                stage::copy(&verified.root.join(target), &temporary, input)?;
                std::fs::rename(&temporary, &path).map_err(|error| {
                    format!(
                        "compatibility alias cannot be published: {:?}",
                        error.kind()
                    )
                })?;
                io::sync_directory(&verified.root)?;
            }
            Err(error) => {
                return Err(format!(
                    "compatibility alias is unreadable: {:?}",
                    error.kind()
                ))
            }
        }
        let (sha, bytes, executable, stamp) = package::fingerprint(&path, input.bytes)?;
        drop(stamp);
        if !sha.eq_ignore_ascii_case(&input.sha256) || bytes != input.bytes || !executable {
            return Err(format!(
                "compatibility alias {} differs from its signed target; remove it and retry",
                super::display_path(&path)
            ));
        }
    }
    Ok(())
}
