//! Persist the namespace after publishing content-addressed bytes.
use std::path::Path;

/// POSIX rename persists a name only after its directory is synced. The shard
/// entry in objects/meta needs its containing directory synced as well.
pub(super) fn sync_parents(
    target: &Path,
    mut sync: impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| std::io::Error::other("artifact has no parent"))?;
    sync(parent)?;
    if let Some(area) = parent.parent() {
        sync(area)?;
    }
    Ok(())
}

pub(super) fn publish(
    from: &Path,
    to: &Path,
    sync: impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if to.exists() {
        // Another writer published identical content; its directory sync may
        // still be pending, so this path must flush the same parents too.
        let _ = std::fs::remove_file(from);
    } else {
        std::fs::rename(from, to)?;
    }
    sync_parents(to, sync)
}

#[cfg(unix)]
pub(super) fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

// std provides no portable directory flush outside Unix. Preserve the existing
// file-sync-and-rename behavior on those platforms; POSIX gains directory fsync.
#[cfg(not(unix))]
pub(super) fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> std::io::Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "grapt-publish-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn rename_and_duplicate_flush_the_object_and_shard_parents() -> std::io::Result<()> {
        let root = Scratch::new()?;
        let source = root.0.join("temporary");
        let target = root.0.join("objects/aa/object");
        for _ in 0..2 {
            std::fs::write(&source, "complete")?;
            let mut calls = Vec::new();
            publish(&source, &target, |parent| {
                assert!(
                    !source.exists(),
                    "flush must follow publication or duplicate removal"
                );
                assert_eq!(std::fs::read_to_string(&target)?, "complete");
                calls.push(parent.to_path_buf());
                sync_directory(parent)
            })?;
            assert_eq!(calls, [root.0.join("objects/aa"), root.0.join("objects")]);
        }
        Ok(())
    }
    #[test]
    fn failed_directory_sync_refuses_acknowledgment_and_retry_flushes_again() -> std::io::Result<()>
    {
        let root = Scratch::new()?;
        let source = root.0.join("temporary");
        let target = root.0.join("meta/aa/metadata");
        for fail_at in 0..2 {
            std::fs::write(&source, "complete")?;
            let mut calls = 0;
            let result = publish(&source, &target, |parent| {
                let index = calls;
                calls += 1;
                if index == fail_at {
                    return Err(std::io::Error::other("injected directory sync refusal"));
                }
                sync_directory(parent)
            });
            assert!(
                result.is_err(),
                "a failed sync cannot return a committed artifact"
            );
            assert_eq!(
                std::fs::read_to_string(&target)?,
                "complete",
                "failure may leave only complete unreferenced bytes"
            );
        }
        std::fs::write(&source, "complete")?;
        publish(&source, &target, sync_directory)?;
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn an_actual_directory_flush_error_is_propagated() {
    // /dev/null opens successfully but Linux refuses fsync on this device;
    // this distinguishes a real sync failure from an open-path failure.
    assert!(sync_directory(Path::new("/dev/null")).is_err());
}
