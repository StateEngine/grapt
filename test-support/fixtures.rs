use std::ffi::OsStr;
use std::path::{Path, PathBuf};

const FIXTURES_ROOT_ENV: &str = "SECODE_FIXTURES";
const REMOTE_ARTIFACT_ROOT_ENV: &str = "REMOTE_COMPUTE_ARTIFACT_ROOT";
const REMOTE_FIXTURES_RELATIVE_ROOT: &str = "fixtures/rust-reconstruction";

pub fn fixture_path_from_roots(
    explicit: Option<&OsStr>,
    remote: Option<&OsStr>,
    relative: &Path,
) -> PathBuf {
    let root = explicit
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            remote
                .filter(|value| !value.is_empty())
                .map(|value| PathBuf::from(value).join(REMOTE_FIXTURES_RELATIVE_ROOT))
        })
        .unwrap_or_else(|| {
            panic!("set {FIXTURES_ROOT_ENV} to the required fixture or evidence directory")
        });
    root.join(relative)
}

pub fn fixture_path(relative: &Path) -> PathBuf {
    let path = fixture_path_from_roots(
        std::env::var_os(FIXTURES_ROOT_ENV).as_deref(),
        std::env::var_os(REMOTE_ARTIFACT_ROOT_ENV).as_deref(),
        relative,
    );
    assert!(
        path.exists(),
        "required frozen fixture is absent at {}; set {FIXTURES_ROOT_ENV} to the fixtures/rust-reconstruction directory; a fixture run without its frozen row proves nothing",
        path.display()
    );
    path
}
