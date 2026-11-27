//! Native Windows relocation and stale-snapshot counterexamples.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{io, read, Stamp};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "grapt-windows-snapshot-{}-{}",
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
fn a_staged_directory_moves_before_final_name_identity_retention() -> Result {
    let root = Scratch::new()?;
    let partial = root.0.join("partial");
    let bin = partial.join("bin");
    std::fs::create_dir(&partial)?;
    std::fs::create_dir(&bin)?;
    io::write_new(&bin.join("input"), b"verified")?;
    let published = root.0.join("published");
    std::fs::rename(&partial, &published)?;
    let directory = Stamp::directory(&published)?;
    let nested = Stamp::directory(&published.join("bin"))?;
    let (bytes, input) = read(&published.join("bin/input"), 8)?;
    assert_eq!(bytes, b"verified");
    directory.recheck(&published)?;
    nested.recheck(&published.join("bin"))?;
    input.recheck(&published.join("bin/input"))?;
    assert!(directory.recheck(&partial).is_err());
    assert!(std::fs::rename(&published, root.0.join("blocked")).is_err());
    Ok(())
}

#[test]
fn replacing_identical_bytes_does_not_resurrect_a_retained_file_identity() -> Result {
    let root = Scratch::new()?;
    let path = root.0.join("input");
    io::write_new(&path, b"verified")?;
    let (_, stamp) = read(&path, 8)?;
    std::fs::remove_file(&path)?;
    io::write_new(&path, b"verified")?;
    assert!(stamp.recheck(&path).is_err());
    let (_, replacement) = read(&path, 8)?;
    replacement.recheck(&path)?;
    Ok(())
}

#[test]
fn replacing_a_directory_does_not_resurrect_its_root_identity() -> Result {
    let root = Scratch::new()?;
    let path = root.0.join("partial");
    std::fs::create_dir(&path)?;
    let stamp = Stamp::directory(&path)?;
    std::fs::rename(&path, root.0.join("old"))?;
    std::fs::create_dir(&path)?;
    assert!(stamp.recheck(&path).is_err());
    Ok(())
}

#[test]
fn changed_bytes_and_overlong_records_refuse_before_activation() -> Result {
    let root = Scratch::new()?;
    let path = root.0.join("input");
    io::write_new(&path, b"verified")?;
    let (_, stamp) = read(&path, 8)?;
    std::fs::write(&path, b"changed and longer")?;
    assert!(stamp.recheck(&path).is_err());
    assert!(read(&path, 8).is_err());
    Ok(())
}

#[test]
fn retained_identity_has_metadata_but_no_data_read_access() -> Result {
    use std::io::Read;

    let root = Scratch::new()?;
    let path = root.0.join("input");
    io::write_new(&path, b"verified")?;
    let (_, stamp) = read(&path, 8)?;
    assert_eq!(stamp.identity.as_file().metadata()?.len(), 8);
    let mut bytes = [0u8; 1];
    assert_eq!(
        stamp
            .identity
            .as_file()
            .read(&mut bytes)
            .err()
            .ok_or("retained metadata identity unexpectedly has data access")?
            .kind(),
        std::io::ErrorKind::PermissionDenied,
    );
    stamp.recheck(&path)?;
    Ok(())
}

#[test]
fn descendant_metadata_handles_refuse_moves_until_their_handles_close() -> Result {
    for (label, hold_root, hold_nested, hold_file) in [
        ("root", true, false, false),
        ("nested", false, true, false),
        ("file", false, false, true),
        ("nested-and-file", false, true, true),
    ] {
        let root = Scratch::new()?;
        let partial = root.0.join("partial");
        let bin = partial.join("bin");
        std::fs::create_dir(&partial)?;
        std::fs::create_dir(&bin)?;
        io::write_new(&bin.join("input"), b"verified")?;
        let directory = hold_root.then(|| Stamp::directory(&partial)).transpose()?;
        let nested = hold_nested.then(|| Stamp::directory(&bin)).transpose()?;
        let input = hold_file.then(|| read(&bin.join("input"), 8)).transpose()?;
        let published = root.0.join("published");
        let moved = std::fs::rename(&partial, &published);
        let error = moved.as_ref().err().map(std::io::Error::kind);
        assert_eq!(
            error,
            (hold_nested || hold_file).then_some(std::io::ErrorKind::PermissionDenied),
        );
        if moved.is_ok() {
            if let Some(stamp) = &directory {
                stamp.recheck(&published)?;
            }
            if let Some(stamp) = &nested {
                stamp.recheck(&published.join("bin"))?;
            }
            if let Some((_, stamp)) = &input {
                stamp.recheck(&published.join("bin/input"))?;
            }
        }
        drop(input);
        drop(nested);
        drop(directory);
        if moved.is_err() {
            assert_eq!(error, Some(std::io::ErrorKind::PermissionDenied));
            std::fs::rename(&partial, &published)?;
        }
        assert_eq!(std::fs::read(published.join("bin/input"))?, b"verified");
        println!(
            "metadata lease {label}: held rename={error:?}; closed rename=success; bytes unchanged"
        );
    }
    Ok(())
}
