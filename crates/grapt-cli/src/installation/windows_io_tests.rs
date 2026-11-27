//! Native Windows adapter tests; Linux execution supplies no Windows credit.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{directory, read, regular, replace, sync_directory, write_new};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "grapt-windows-io-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn directory_flush_requires_an_actual_successful_native_flush() -> Result {
    let root = Scratch::new()?;
    sync_directory(&root.0)?;
    Ok(())
}

#[test]
fn regular_input_retains_identity_and_refuses_delete_until_drop() -> Result {
    let root = Scratch::new()?;
    let path = root.0.join("input");
    write_new(&path, b"verified input")?;
    let held = regular(&path)?;
    assert!(std::fs::remove_file(&path).is_err());
    assert!(std::fs::rename(&path, root.0.join("moved")).is_err());
    assert_eq!(read(&path, 14)?, b"verified input");
    assert!(read(&path, 13).is_err());
    drop(held);
    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn a_held_record_refuses_replace_and_preserves_both_bytes() -> Result {
    let root = Scratch::new()?;
    let current = root.0.join("current");
    write_new(&current, b"old")?;
    let held = regular(&current)?;
    assert!(replace(&current, b"new").is_err());
    assert_eq!(read(&current, 3)?, b"old");
    assert_eq!(read(&root.0.join("current.new"), 3)?, b"new");
    drop(held);
    replace(&current, b"new")?;
    assert_eq!(read(&current, 3)?, b"new");
    assert!(!root.0.join("current.new").exists());
    Ok(())
}

#[test]
fn nonregular_and_unbounded_routes_never_become_installer_inputs() -> Result {
    let root = Scratch::new()?;
    assert!(regular(&root.0).is_err());
    assert!(directory(&PathBuf::from("relative")).is_err());
    assert!(directory(&root.0.join("..")).is_err());
    assert!(directory(&PathBuf::from(r"\\server\share\installation")).is_err());
    assert!(write_new(&root.0.join("absent/input"), b"x").is_err());
    Ok(())
}
