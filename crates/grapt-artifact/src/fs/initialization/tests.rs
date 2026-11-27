use super::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::Duration;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "grapt-artifact-init-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn barrier(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = ["objects", "meta", "tmp"]
        .map(|area| root.join(area))
        .into();
    paths.extend(
        root.ancestors()
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf),
    );
    if root.is_relative() {
        paths.push(PathBuf::from("."));
    }
    paths
}

#[test]
fn ready_is_published_only_after_complete_namespace_barrier() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let root = scratch.0.join("new/nested/store");
    let expected = barrier(&root);
    let calls = RefCell::new(Vec::new());
    initialize(
        &root,
        |path| {
            if ready(&root)? {
                assert_eq!(*calls.borrow(), expected, "marker preceded namespace sync");
                assert_eq!(path, root);
                assert_eq!(std::fs::read(ready_path(&root))?, content().as_bytes());
            }
            calls.borrow_mut().push(path.to_path_buf());
            Ok(())
        },
        |file| {
            assert_eq!(
                *calls.borrow(),
                expected,
                "file flush preceded namespace sync"
            );
            assert!(!ready(&root)?, "marker published before its file sync");
            file.sync_all()
        },
    )?;
    let mut complete = expected;
    complete.push(root.clone());
    assert_eq!(*calls.borrow(), complete, "marker rename needs a root sync");
    assert!(ready(&root)?);
    Ok(())
}

#[test]
fn a_ready_open_performs_no_file_or_directory_sync() -> io::Result<()> {
    let scratch = Scratch::new()?;
    open(&scratch.0)?;
    initialize(
        &scratch.0,
        |_| Err(io::Error::other("warm directory sync")),
        |_| Err(io::Error::other("warm marker file sync")),
    )?;
    Ok(())
}

#[test]
fn legacy_and_older_version_roots_complete_the_barrier_once() -> io::Result<()> {
    let scratch = Scratch::new()?;
    for area in ["objects", "meta", "tmp"] {
        std::fs::create_dir(scratch.0.join(area))?;
    }
    std::fs::write(scratch.0.join(".secode-artifact-ready-v0"), "old contract")?;
    let mut calls = Vec::new();
    initialize(
        &scratch.0,
        |path| {
            calls.push(path.to_path_buf());
            Ok(())
        },
        File::sync_all,
    )?;
    let mut expected = barrier(&scratch.0);
    expected.push(scratch.0.clone());
    assert_eq!(calls, expected);
    assert!(ready(&scratch.0)?);
    initialize(
        &scratch.0,
        |_| panic!("legacy retry synced"),
        |_| panic!("legacy retry flushed"),
    )?;
    Ok(())
}

#[test]
fn every_directory_sync_failure_leaves_no_ready_marker_and_retry_repairs() -> io::Result<()> {
    let layout = Scratch::new()?;
    let count = barrier(&layout.0).len() + 1;
    for fail_at in 0..count {
        let scratch = Scratch::new()?;
        let mut calls = 0;
        let result = initialize(
            &scratch.0,
            |_| {
                let index = calls;
                calls += 1;
                if index == fail_at {
                    Err(io::Error::other("injected namespace sync failure"))
                } else {
                    Ok(())
                }
            },
            File::sync_all,
        );
        assert!(
            result.is_err(),
            "sync {fail_at} acknowledged initialization"
        );
        assert!(!ready(&scratch.0)?, "sync {fail_at} left a ready marker");
        let mut repaired = Vec::new();
        initialize(
            &scratch.0,
            |path| {
                repaired.push(path.to_path_buf());
                Ok(())
            },
            File::sync_all,
        )?;
        let mut expected = barrier(&scratch.0);
        expected.push(scratch.0.clone());
        assert_eq!(repaired, expected, "retry skipped an interrupted barrier");
    }
    Ok(())
}

#[test]
fn marker_file_sync_failure_never_publishes_readiness() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let result = initialize(
        &scratch.0,
        |_| Ok(()),
        |_| Err(io::Error::other("injected marker flush failure")),
    );
    assert!(result.is_err());
    assert!(!ready(&scratch.0)?);
    assert_eq!(std::fs::read_dir(scratch.0.join("tmp"))?.count(), 0);
    open(&scratch.0)?;
    assert!(ready(&scratch.0)?);
    Ok(())
}

#[test]
fn another_initializer_can_finish_while_the_creator_is_paused_before_sync() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let root = scratch.0.join("new/nested/store");
    let first_root = root.clone();
    let (created, reached) = sync_channel(1);
    let (resume, released) = sync_channel(1);
    let creator = std::thread::spawn(move || {
        let mut paused = false;
        initialize(
            &first_root,
            |path| {
                if !paused {
                    paused = true;
                    created.send(()).map_err(io::Error::other)?;
                    released
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(io::Error::other)?;
                }
                sync_directory(path)
            },
            File::sync_all,
        )
    });
    reached
        .recv_timeout(Duration::from_secs(10))
        .map_err(io::Error::other)?;
    assert!(!ready(&root)?);
    let mut calls = Vec::new();
    let second = initialize(
        &root,
        |path| {
            calls.push(path.to_path_buf());
            sync_directory(path)
        },
        File::sync_all,
    );
    let marker_before_release = ready(&root);
    let publication = second.and_then(|()| {
        use crate::{
            ArtifactKind, ArtifactStore, CaptureBound, FilesystemArtifactStore, Sensitivity,
        };
        #[derive(Debug)]
        struct Clock;
        impl grapt_types::ClockAuthority for Clock {
            fn now(&self) -> grapt_types::Timestamp {
                grapt_types::Timestamp::from_millis(1)
            }
        }
        let store = FilesystemArtifactStore::open(&root, b"test-key".to_vec(), Box::new(Clock))?;
        let bound = CaptureBound::new(32, 16).ok_or_else(|| io::Error::other("invalid bound"))?;
        store
            .capture(
                b"durable before A resumes".as_slice(),
                bound,
                ArtifactKind::CapturedOutput,
                Sensitivity::Ordinary,
            )
            .map_err(io::Error::other)?;
        Ok(())
    });
    let release = resume.send(());
    let first = creator
        .join()
        .map_err(|_| io::Error::other("initializer panicked"))?;
    release.map_err(io::Error::other)?;
    first?;
    publication?;
    assert!(marker_before_release?, "B acknowledged before readiness");
    let mut expected = barrier(&root);
    expected.push(root.clone());
    assert_eq!(
        calls, expected,
        "visible mkdirs were mistaken for durability"
    );
    assert!(ready(&root)?);
    Ok(())
}
