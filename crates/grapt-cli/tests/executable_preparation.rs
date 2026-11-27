//! Preparation must fail honestly before a timed child/HTTP operation begins.
#![forbid(unsafe_code)]

#[path = "../../../test-support/executable.rs"]
mod executable;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self, std::io::Error> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "grapt-executable-preparation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        Ok(Self(root))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn executable_preparation_reads_through_the_last_partial_page() -> TestResult {
    let root = Scratch::new()?;
    let path = root.path().join("fixture-program");
    let bytes = vec![7; 2 * 64 * 1024 + 37];
    std::fs::write(&path, &bytes)?;
    assert_eq!(executable::prepare(&path)?, bytes.len() as u64);
    assert_eq!(
        std::fs::read(&path)?,
        bytes,
        "preparation changed the program"
    );
    Ok(())
}

#[test]
fn missing_executable_is_a_named_preparation_failure() -> TestResult {
    let root = Scratch::new()?;
    let failure = executable::prepare(&root.path().join("absent"));
    assert!(failure.is_err(), "missing evidence passed preparation");
    assert!(failure
        .err()
        .is_some_and(|message| message.contains("executable preparation")));
    Ok(())
}

#[test]
fn empty_and_non_regular_executables_are_not_prepared() -> TestResult {
    let root = Scratch::new()?;
    let path = root.path().join("empty");
    std::fs::write(&path, [])?;
    assert!(executable::prepare(&path).is_err());
    assert!(executable::prepare(root.path()).is_err());
    Ok(())
}

#[test]
fn an_oversize_executable_fails_before_reading_its_contents() -> TestResult {
    let root = Scratch::new()?;
    let path = root.path().join("oversize");
    let file = std::fs::File::create(&path)?;
    file.set_len(executable::MAX_EXECUTABLE_BYTES + 1)?;
    let failure = executable::prepare(&path);
    assert!(failure.is_err());
    assert!(failure
        .err()
        .is_some_and(|message| message.contains("bound")));
    Ok(())
}
