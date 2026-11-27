//! Names from before the product rename that existing installs still use.
//!
//! Every reader prefers the current name and falls back to the legacy one
//! only when the current one is genuinely absent; with both present the
//! current one wins. Either way one notice line names the legacy name used or
//! ignored. Writers write the file that was read, so a legacy profile stays a
//! single set of files. Nothing is moved automatically.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use grapt_types::profile_names::{choose, Spelling};

/// The explicit configuration document. StateEngine's launcher sets this name.
pub(super) const LEGACY_CONFIG: &str = "SECODE_CONFIG"; // grapt-rename: keep
/// Inline configuration content.
pub(super) const LEGACY_CONFIG_CONTENT: &str = "SECODE_CONFIG_CONTENT"; // grapt-rename: keep
/// The user configuration root override.
pub(super) const LEGACY_CONFIG_DIR: &str = "SECODE_CONFIG_DIR"; // grapt-rename: keep
/// The skill metadata key that hides a skill from slash commands.
/// The home override StateEngine's embedded journey still sets by its
/// pre-rename name.
pub(super) const LEGACY_TEST_HOME: &str = "SECODE_TEST_HOME"; // grapt-rename: keep
/// The plugin runtime overrides from before the product rename.
pub(super) const LEGACY_PLUGIN_RUNTIME_MANIFEST: &str = "SECODE_PLUGIN_RUNTIME_MANIFEST"; // grapt-rename: keep
pub(super) const LEGACY_PLUGIN_NODE: &str = "SECODE_PLUGIN_NODE"; // grapt-rename: keep
pub(super) const LEGACY_PLUGIN_BRIDGE: &str = "SECODE_PLUGIN_BRIDGE"; // grapt-rename: keep
pub(super) const LEGACY_SKILL_SLASH: &str = "secode/slash"; // grapt-rename: keep
/// The skill metadata key that keeps a skill from model invocation.
pub(super) const LEGACY_SKILL_AUTOINVOKE: &str = "secode/autoinvoke"; // grapt-rename: keep

/// Both project directory names. Protection and search exclusion name both
/// regardless of which one a repository uses.
pub(super) const PROJECT_DIRECTORIES: [&str; 2] = [
    Spelling::Current.project_dir(),
    Spelling::Legacy.project_dir(),
];

/// Whether `name` is either project directory name; case-insensitive on
/// Windows, like the filesystem.
pub(super) fn is_project_directory(name: &str) -> bool {
    PROJECT_DIRECTORIES.iter().any(|candidate| {
        if cfg!(windows) {
            name.eq_ignore_ascii_case(candidate)
        } else {
            name == *candidate
        }
    })
}

/// The spelling chosen for one location, and the notices that choice owes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Resolved {
    pub spelling: Spelling,
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    Absent,
    Present,
    /// Unreadable for another reason. It counts as present, so the current
    /// name keeps its own diagnostic, but never earns an "ignoring" notice.
    Unknown,
}

fn probe(path: &Path) -> Probe {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Probe::Present,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Probe::Absent,
        Err(_) => Probe::Unknown,
    }
}

fn using(legacy: &Path, current: &str) -> String {
    format!(
        "using legacy {}; rename to {current} when convenient",
        legacy.display()
    )
}

fn ignoring(legacy: &Path, current: &Path) -> String {
    format!(
        "ignoring legacy {} because {} exists",
        legacy.display(),
        current.display()
    )
}

/// Which configuration file names `directory` uses, without reporting it.
/// An approved profile keeps the names it was approved under, so a file
/// created after approval can never redirect its reads.
pub(super) fn config_resolution(directory: &Path) -> Resolved {
    let names = |spelling: Spelling| [spelling.config_json(), spelling.config_jsonc()];
    let captured = |spelling: Spelling| {
        grapt_platform::profile_configuration::captured_identity(
            &directory.join(spelling.config_jsonc()),
        )
        .is_ok_and(|identity| identity.is_some())
    };
    let legacy_notices = |current: Option<&Path>| {
        names(Spelling::Legacy)
            .into_iter()
            .zip(names(Spelling::Current))
            .map(|(legacy, renamed)| (directory.join(legacy), renamed))
            .filter(|(path, _)| probe(path) == Probe::Present)
            .map(|(path, renamed)| match current {
                Some(current) => ignoring(&path, current),
                None => using(&path, renamed),
            })
            .collect::<Vec<_>>()
    };
    if captured(Spelling::Current) {
        return Resolved {
            spelling: Spelling::Current,
            notices: Vec::new(),
        };
    }
    if captured(Spelling::Legacy) {
        return Resolved {
            spelling: Spelling::Legacy,
            notices: legacy_notices(None),
        };
    }
    let current = names(Spelling::Current)
        .into_iter()
        .map(|name| directory.join(name))
        .find(|path| probe(path) != Probe::Absent);
    let legacy_present = names(Spelling::Legacy)
        .into_iter()
        .any(|name| probe(&directory.join(name)) != Probe::Absent);
    let choice = choose(current.is_some(), legacy_present);
    let notices = match (choice.spelling, choice.ignored, &current) {
        (Spelling::Legacy, _, _) => legacy_notices(None),
        (Spelling::Current, Some(_), Some(current)) => legacy_notices(Some(current)),
        _ => Vec::new(),
    };
    Resolved {
        spelling: choice.spelling,
        notices,
    }
}

/// Which configuration file names `directory` uses, reporting a legacy use.
pub(super) fn config_spelling(directory: &Path) -> Spelling {
    let resolved = config_resolution(directory);
    report(resolved.notices);
    resolved.spelling
}

/// The two configuration documents `directory` is read and written through:
/// JSON first, then JSONC.
pub(super) fn config_files(directory: &Path) -> [PathBuf; 2] {
    let spelling = config_spelling(directory);
    [
        directory.join(spelling.config_json()),
        directory.join(spelling.config_jsonc()),
    ]
}

/// The JSONC configuration document `directory` is read and written through.
pub(super) fn config_jsonc(directory: &Path) -> PathBuf {
    directory.join(config_spelling(directory).config_jsonc())
}

/// Every configuration document name in `directory`, both spellings, for the
/// sets that protect them whichever one is in use.
pub(super) fn every_config_file(directory: &Path) -> [PathBuf; 4] {
    [
        directory.join(Spelling::Current.config_json()),
        directory.join(Spelling::Current.config_jsonc()),
        directory.join(Spelling::Legacy.config_json()),
        directory.join(Spelling::Legacy.config_jsonc()),
    ]
}

/// Which of the two directory names under `parent` to use, without reporting
/// it. `name` gives each spelling's directory name; `below`, when given, is
/// the entry inside it whose presence decides instead of the directory's own.
pub(super) fn directory_resolution(
    parent: &Path,
    name: fn(Spelling) -> &'static str,
    below: Option<&str>,
) -> (PathBuf, Resolved) {
    let path = |spelling: Spelling| {
        let directory = parent.join(name(spelling));
        match below {
            Some(below) => directory.join(below),
            None => directory,
        }
    };
    let (current, legacy) = (path(Spelling::Current), path(Spelling::Legacy));
    let current_probe = probe(&current);
    let legacy_probe = probe(&legacy);
    let choice = choose(
        current_probe != Probe::Absent,
        legacy_probe != Probe::Absent,
    );
    let notices = match (choice.spelling, choice.ignored) {
        (Spelling::Legacy, _) => vec![using(&legacy, name(Spelling::Current))],
        (Spelling::Current, Some(_)) if legacy_probe == Probe::Present => {
            vec![ignoring(&legacy, &current)]
        }
        _ => Vec::new(),
    };
    let chosen = parent.join(name(choice.spelling));
    (
        chosen,
        Resolved {
            spelling: choice.spelling,
            notices,
        },
    )
}

/// `parent`'s directory of the chosen spelling, reporting a legacy use.
pub(super) fn directory(
    parent: &Path,
    name: fn(Spelling) -> &'static str,
    below: Option<&str>,
) -> PathBuf {
    let (chosen, resolved) = directory_resolution(parent, name, below);
    report(resolved.notices);
    chosen
}

/// The project directory (`.grapt`, or a legacy one) of the repository
/// directory `parent`.
pub(super) fn project_directory(parent: &Path) -> PathBuf {
    directory(parent, Spelling::project_dir, None)
}

/// Read the variable `current`, else `legacy` when `current` is genuinely
/// unset, reporting the legacy use. `read` is the caller's bounded reader.
pub(super) fn environment(
    current: &str,
    legacy: &str,
    read: impl Fn(&str) -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    let (value, notice) = environment_resolution(current, legacy, read)?;
    report(notice);
    Ok(value)
}

/// [`environment`] without reporting: the value and the notice it owes.
pub(super) fn environment_resolution(
    current: &str,
    legacy: &str,
    read: impl Fn(&str) -> Result<Option<String>, String>,
) -> Result<(Option<String>, Option<String>), String> {
    if let Some(value) = read(current)? {
        let notice = std::env::var_os(legacy).map(|_| {
            format!("ignoring legacy environment variable {legacy} because {current} is set")
        });
        return Ok((Some(value), notice));
    }
    let value = read(legacy)?;
    let notice = value.as_ref().map(|_| {
        format!("using legacy environment variable {legacy}; rename to {current} when convenient")
    });
    Ok((value, notice))
}

/// The most distinct notices one process reports; later ones are dropped so
/// a long-lived service cannot grow this set without bound.
const MAX_NOTICES: usize = 256;

/// Report each distinct notice once per process on the service's diagnostic
/// stream. Catalog sources are re-read on every request, so a repeat is
/// suppressed rather than logged again.
pub(super) fn report(notices: impl IntoIterator<Item = String>) {
    static REPORTED: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    let reported = REPORTED.get_or_init(|| Mutex::new(BTreeSet::new()));
    let mut reported = match reported.lock() {
        Ok(reported) => reported,
        Err(poisoned) => poisoned.into_inner(),
    };
    for notice in notices {
        if reported.len() < MAX_NOTICES && reported.insert(notice.clone()) {
            crate::server_log::warn(&notice);
        }
    }
}
