#[path = "../../../test-support/evidence.rs"]
mod evidence;

use std::ffi::OsStr;
use std::path::PathBuf;

#[test]
fn explicit_evidence_root_selects_the_frozen_clone() {
    let selected = evidence::frozen_clone_path_from(Some(OsStr::new("/srv/grapt-evidence")));
    assert_eq!(
        selected,
        PathBuf::from("/srv/grapt-evidence").join(evidence::FROZEN_CLONE_FILE)
    );
}

#[test]
fn remote_artifact_root_uses_the_same_directory_contract() {
    assert_eq!(
        evidence::REMOTE_ARTIFACT_ROOT_ENV,
        "REMOTE_COMPUTE_ARTIFACT_ROOT"
    );
    let selected =
        evidence::frozen_clone_path_from(Some(OsStr::new("/srv/remote-compute-artifacts/run-id")));
    assert_eq!(
        selected,
        PathBuf::from("/srv/remote-compute-artifacts/run-id").join(evidence::FROZEN_CLONE_FILE)
    );
}

#[test]
#[ignore = "evidence-bearing: validates the configured frozen clone; run with --include-ignored on the evidence machine"]
fn configured_environment_resolves_the_required_frozen_clone() {
    let path = evidence::frozen_clone();
    assert_eq!(
        path.file_name(),
        Some(OsStr::new(evidence::FROZEN_CLONE_FILE)),
        "the configured evidence root must resolve the canonical frozen clone"
    );
}

#[test]
fn missing_root_refuses_without_an_operator_default() {
    assert!(std::panic::catch_unwind(|| evidence::frozen_clone_path_from(None)).is_err());
    assert!(
        std::panic::catch_unwind(|| evidence::frozen_clone_path_from(Some(OsStr::new(""))))
            .is_err()
    );
}

#[test]
fn absent_clone_panics_instead_of_skipping() {
    let missing = PathBuf::from("Z:/evidence-must-not-exist").join(evidence::FROZEN_CLONE_FILE);
    assert!(
        !missing.exists(),
        "test precondition: missing path unexpectedly exists"
    );
    let refusal = std::panic::catch_unwind(|| evidence::require_frozen_clone(missing));
    assert!(refusal.is_err(), "absent evidence must panic loudly");
}
