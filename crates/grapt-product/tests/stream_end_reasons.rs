//! The product states why it ends live event streams. The hub's reasonless
//! `invalidate()` / `reconcile()` record "unstated", which is what left the
//! benchmark's endless rebinds unexplained, so production code may only use
//! `invalidate_because` / `reconcile_because`.

use std::path::{Path, PathBuf};

fn sources(directory: &Path, found: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| panic!("directory entry: {error}"))
            .path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "tests") {
                continue;
            }
            sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("_tests.rs") || name == "tests.rs")
        {
            found.push(path);
        }
    }
}

#[test]
fn production_code_never_ends_streams_without_a_reason() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() > 50, "found only {} sources", files.len());
    let mut unstated = Vec::new();
    for file in files {
        let source = std::fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        // Only the production part: everything before an inline test module.
        let production = source.split("#[cfg(test)]").next().unwrap_or_default();
        for (index, line) in production.lines().enumerate() {
            if line.contains(".invalidate()") || line.contains(".reconcile()") {
                unstated.push(format!("{}:{}: {}", file.display(), index + 1, line.trim()));
            }
        }
    }
    assert!(
        unstated.is_empty(),
        "streams ended without a reason:\n{}",
        unstated.join("\n")
    );
}
