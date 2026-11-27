use std::ffi::OsStr;
use std::path::PathBuf;

pub const EVIDENCE_ROOT_ENV: &str = "SECODE_EVIDENCE_ROOT";
pub const REMOTE_ARTIFACT_ROOT_ENV: &str = "REMOTE_COMPUTE_ARTIFACT_ROOT";
pub const FROZEN_CLONE_FILE: &str = "opencode-next.clone-2026-08-08.sanitized.db";

pub fn frozen_clone_path_from(root: Option<&OsStr>) -> PathBuf {
    let root = root
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!("set {EVIDENCE_ROOT_ENV} to the required fixture or evidence directory")
        });
    root.join(FROZEN_CLONE_FILE)
}

pub fn require_frozen_clone(path: PathBuf) -> PathBuf {
    assert!(
        path.is_file(),
        "required frozen clone is absent at {}; set {EVIDENCE_ROOT_ENV} to the directory containing {FROZEN_CLONE_FILE}; an evidence run without the clone proves nothing",
        path.display()
    );
    path
}

pub fn frozen_clone() -> PathBuf {
    let root =
        std::env::var_os(EVIDENCE_ROOT_ENV).or_else(|| std::env::var_os(REMOTE_ARTIFACT_ROOT_ENV));
    require_frozen_clone(frozen_clone_path_from(root.as_deref()))
}
