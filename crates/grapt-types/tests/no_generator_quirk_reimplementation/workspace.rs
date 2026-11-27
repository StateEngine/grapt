use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use toml::Value;

use super::lexical::{scan_source, Findings};

// ---------------------------------------------------------------------------
// Floors
// ---------------------------------------------------------------------------

/// Aggregate Rust-file floor.
///
/// Measured at base `2e93c9d4`: **397** files. The floor sits deliberately below
/// that so ordinary growth does not churn this constant, and just as deliberately
/// far above zero: a traversal that collects nothing finds no violations and
/// would otherwise report a clean run. That failure mode — not a missed
/// violation — is what kills a keep-it-absent gate, because the property it
/// guards is trivially true today.
pub(super) const MIN_TOTAL_RUST_FILES: usize = 300;

/// Every declared root must contribute at least one file. The smallest at base is
/// `test-support/` with 2.
pub(super) const MIN_FILES_PER_ROOT: usize = 1;

/// Shared sources that are not a crate directory but are compiled INTO crates via
/// `#[path]`, so they are unambiguously "Rust crate" code for C3-4's purposes.
const EXTRA_SOURCE_ROOTS: &[&str] = &["test-support"];

// ---------------------------------------------------------------------------
// Fail-closed I/O
// ---------------------------------------------------------------------------

fn read_to_string(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => panic!("cannot read {}: {error}", path.display()),
    }
}

fn read_dir(path: &Path) -> Vec<PathBuf> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => panic!("cannot read directory {}: {error}", path.display()),
    };
    let mut found = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => found.push(entry.path()),
            Err(error) => panic!("cannot read an entry of {}: {error}", path.display()),
        }
    }
    found.sort();
    found
}

// ---------------------------------------------------------------------------
// Workspace enumeration
// ---------------------------------------------------------------------------

/// The workspace root, found by walking up from this crate.
pub(super) fn workspace_root(start: &Path) -> PathBuf {
    for dir in start.ancestors() {
        let candidate = dir.join("Cargo.toml");
        let Ok(text) = fs::read_to_string(&candidate) else {
            continue;
        };
        let Ok(value) = text.parse::<Value>() else {
            continue;
        };
        if value.get("workspace").and_then(Value::as_table).is_some() {
            return dir.to_path_buf();
        }
    }
    panic!(
        "no ancestor of {} declares a [workspace] table",
        start.display()
    );
}

/// A `[workspace]` string array, refused rather than defaulted when malformed.
fn manifest_paths(workspace: &Value, key: &str) -> Vec<String> {
    let Some(table) = workspace.get("workspace").and_then(Value::as_table) else {
        panic!("the root manifest declares no [workspace] table");
    };
    let Some(entries) = table.get(key) else {
        return Vec::new();
    };
    let Some(entries) = entries.as_array() else {
        panic!("[workspace].{key} is not an array");
    };
    entries
        .iter()
        .map(|entry| match entry {
            Value::String(path) if !path.is_empty() => path.clone(),
            other => panic!("[workspace].{key} holds a malformed entry: {other:?}"),
        })
        .collect()
}

/// Every source root this gate is responsible for, derived from the root
/// manifest. Never a hardcoded list: a crate added tomorrow must arrive here by
/// being declared, or its files go unattributed below.
pub(super) fn declared_roots(root: &Path) -> Vec<PathBuf> {
    let manifest = read_to_string(&root.join("Cargo.toml"));
    let Ok(workspace) = manifest.parse::<Value>() else {
        panic!("the root manifest at {} does not parse", root.display());
    };
    let mut roots: Vec<PathBuf> = Vec::new();
    for relative in manifest_paths(&workspace, "members")
        .into_iter()
        .chain(manifest_paths(&workspace, "exclude"))
        .chain(EXTRA_SOURCE_ROOTS.iter().map(|path| (*path).to_owned()))
    {
        let absolute = root.join(&relative);
        if !absolute.is_dir() {
            panic!(
                "declared root {relative} does not exist at {}",
                absolute.display()
            );
        }
        roots.push(absolute);
    }
    roots.sort();
    roots.dedup();
    roots
}

/// Every `.rs` file in the repository, `.git` and build output aside.
fn all_rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for path in read_dir(dir) {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if path.is_dir() {
            if name == ".git" || name == "target" {
                continue;
            }
            all_rust_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// This gate's own source, which necessarily contains the anchors as constants.
///
/// Exactly one path, named exactly. A wildcard here — "anything under tests/" —
/// would exempt every test file in the workspace from the gate, which is where a
/// reimplementation could then sit unseen.
pub(super) fn self_exclusion() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("no_generator_quirk_reimplementation.rs");
    assert!(
        path.is_file(),
        "the self-exclusion must name a real file, but {} does not exist",
        path.display()
    );
    path
}

/// The detector module, which necessarily owns the forbidden constants.
///
/// Exactly one additional file, never its directory: the workspace helper and
/// every sibling test remain subject to the same scan as ordinary Rust source.
pub(super) fn detector_exclusion() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("no_generator_quirk_reimplementation")
        .join("lexical.rs");
    assert!(
        path.is_file(),
        "the detector exclusion must name a real file, but {} does not exist",
        path.display()
    );
    path
}

// ---------------------------------------------------------------------------
// One complete pass of the gate
// ---------------------------------------------------------------------------

/// What one pass over a repository root found and how much it actually looked at.
#[derive(Debug)]
pub(super) struct ScanReport {
    pub(super) found: Findings,
    /// Every `.rs` file the traversal collected.
    pub(super) files: usize,
    /// Those it actually judged, i.e. collected minus the self-exclusion.
    pub(super) scanned: usize,
    pub(super) per_root: BTreeMap<PathBuf, usize>,
}

/// Run the whole gate — enumerate, walk, attribute, judge — over `root`.
///
/// Parameterised by root and exclusion so the shipped witnesses can drive the
/// REAL pipeline over a fixture repository. R1 EP-C6: the refusal branch
/// previously had no end-to-end witness, only a test of the roots computation,
/// and "the behaviour is correct, only the witness is indirect" is not a
/// standard this lane should hold itself to. The floors are deliberately NOT
/// applied here — they belong to the caller, because a fixture repository is
/// meant to be tiny and the production floor is meant to be large.
pub(super) fn scan_repository(root: &Path, excluded: Option<&Path>) -> ScanReport {
    let roots = declared_roots(root);
    let detector = detector_exclusion();

    let mut files = Vec::new();
    all_rust_files(root, &mut files);
    files.sort();

    let mut found = Findings::default();
    let mut per_root: BTreeMap<PathBuf, usize> = roots.iter().map(|r| (r.clone(), 0)).collect();
    let mut scanned = 0usize;

    for file in &files {
        // Attribution first. A Rust file under no declared root means the
        // workspace grew a source tree this gate does not know about, and that is
        // a refusal, never a skip: skipping is exactly how a new crate lands
        // unguarded.
        let owner = roots.iter().find(|root| file.starts_with(root));
        let Some(owner) = owner else {
            found.refusals.push(format!(
                "{} is a Rust source under no declared workspace root",
                file.display()
            ));
            continue;
        };
        if let Some(count) = per_root.get_mut(owner) {
            *count += 1;
        }
        if Some(file.as_path()) == excluded || file == &detector {
            continue;
        }
        scanned += 1;
        let label = file
            .strip_prefix(root)
            .unwrap_or(file)
            .display()
            .to_string()
            .replace('\\', "/");
        scan_source(&label, &read_to_string(file), &mut found);
    }

    ScanReport {
        found,
        files: files.len(),
        scanned,
        per_root,
    }
}
