#[path = "../../../test-support/fixtures.rs"]
mod fixtures;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[test]
fn explicit_fixture_root_has_first_precedence() {
    let selected = fixtures::fixture_path_from_roots(
        Some(OsStr::new("/srv/explicit-fixtures")),
        Some(OsStr::new("/srv/run-artifacts")),
        Path::new("scenario/manifest.json"),
    );
    assert_eq!(
        selected,
        PathBuf::from("/srv/explicit-fixtures").join("scenario/manifest.json")
    );
}

#[test]
fn remote_artifact_root_maps_to_the_frozen_fixture_tree() {
    let selected = fixtures::fixture_path_from_roots(
        None,
        Some(OsStr::new("/srv/run-artifacts")),
        Path::new("scenario/actions.jsonl"),
    );
    assert_eq!(
        selected,
        PathBuf::from("/srv/run-artifacts")
            .join("fixtures/rust-reconstruction/scenario/actions.jsonl")
    );
}

#[test]
fn empty_roots_refuse_without_an_operator_default() {
    assert!(
        std::panic::catch_unwind(|| fixtures::fixture_path_from_roots(
            Some(OsStr::new("")),
            Some(OsStr::new("")),
            Path::new("scenario/expected/state.json"),
        ))
        .is_err()
    );
}

#[test]
fn missing_fixture_panics_instead_of_skipping() -> Result<(), Box<dyn std::error::Error>> {
    let relative = Path::new("fixture-root-test-must-not-exist/manifest.json");
    const CHILD: &str = "GRAPT_MISSING_FIXTURE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let refusal = std::panic::catch_unwind(|| fixtures::fixture_path(relative));
        assert!(refusal.is_err(), "missing fixture must panic loudly");
        return Ok(());
    }
    let root = std::env::temp_dir().join(format!("grapt-missing-fixture-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let selected = fixtures::fixture_path_from_roots(Some(root.as_os_str()), None, relative);
    assert!(!selected.exists(), "test precondition: missing path exists");
    let result = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "missing_fixture_panics_instead_of_skipping"])
        .env("SECODE_FIXTURES", &root)
        .env(CHILD, "1")
        .output();
    let cleanup = std::fs::remove_dir(&root);
    let output = result?;
    cleanup?;
    assert!(
        output.status.success(),
        "missing-file consumer: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
