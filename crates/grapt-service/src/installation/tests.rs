//! The prefix guard excludes transitions, independently of election or profiles.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{prefix_of_binary, PrefixError, PrefixGuard};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "grapt-prefix-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        }
        #[cfg(not(unix))]
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
fn all_shared_holders_must_close_before_a_transition_can_start() -> Result {
    let root = Scratch::new()?;
    let first = PrefixGuard::shared(&root.0)?;
    let second = PrefixGuard::shared(&root.0)?;
    assert!(matches!(
        PrefixGuard::exclusive(&root.0),
        Err(PrefixError::Busy)
    ));
    drop(first);
    assert!(matches!(
        PrefixGuard::exclusive(&root.0),
        Err(PrefixError::Busy)
    ));
    drop(second);
    let exclusive = PrefixGuard::exclusive(&root.0)?;
    assert!(matches!(
        PrefixGuard::exclusive(&root.0),
        Err(PrefixError::Busy)
    ));
    drop(exclusive);
    let _next = PrefixGuard::shared(&root.0)?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn incidental_fork_copies_do_not_extend_closed_shared_guard_lifetimes() -> Result {
    let root = Scratch::new()?;
    let first = PrefixGuard::shared(&root.0)?;
    let second = PrefixGuard::shared(&root.0)?;
    // A dup and a pre-exec fork copy share the owner's open file description.
    // These test-only copies are incidental, not independent guard owners.
    let copies = [first._file.try_clone()?, second._file.try_clone()?];
    drop(first);
    assert!(matches!(
        PrefixGuard::exclusive(&root.0),
        Err(PrefixError::Busy)
    ));
    drop(second);
    let exclusive = PrefixGuard::exclusive(&root.0)?;
    assert!(copies.iter().all(|copy| copy.metadata().is_ok()));
    drop(exclusive);
    drop(copies);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn an_incidental_fork_copy_does_not_extend_a_closed_exclusive_guard() -> Result {
    let root = Scratch::new()?;
    let exclusive = PrefixGuard::exclusive(&root.0)?;
    let copy = exclusive._file.try_clone()?;
    drop(exclusive);
    let shared = PrefixGuard::shared(&root.0)?;
    assert!(matches!(
        PrefixGuard::exclusive(&root.0),
        Err(PrefixError::Busy)
    ));
    assert!(copy.metadata().is_ok());
    drop(shared);
    drop(copy);
    Ok(())
}

#[test]
fn prefix_selection_uses_the_immutable_layout_not_a_service_profile() -> Result {
    let root = Scratch::new()?;
    let bin = root
        .0
        .join("versions")
        .join("1111111111111111111111111111111111111111")
        .join("bin");
    std::fs::create_dir_all(&bin)?;
    assert_eq!(
        prefix_of_binary(&bin.join("grapt-server"))?,
        Some(root.0.clone())
    );
    assert_eq!(prefix_of_binary(&root.0.join("grapt-server"))?, None);
    let invalid = root.0.join("versions/not-an-identity/bin/grapt-server");
    assert!(matches!(
        prefix_of_binary(&invalid),
        Err(PrefixError::Nonregular)
    ));
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_lock_name_remains_bound_until_the_guard_closes() -> Result {
    let root = Scratch::new()?;
    let guard = PrefixGuard::shared(&root.0)?;
    let lock = root.0.join("lock");
    let moved = root.0.join("moved-lock");
    assert!(
        std::fs::rename(&lock, &moved).is_err(),
        "live prefix guard allowed its named lock to be replaced"
    );
    drop(guard);
    std::fs::rename(&lock, &moved)?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_directory_lease_prevents_prefix_replacement() -> Result {
    let root = Scratch::new()?;
    let moved = Scratch(root.0.with_extension("moved"));
    let guard = PrefixGuard::shared(&root.0)?;
    assert!(
        std::fs::rename(&root.0, &moved.0).is_err(),
        "live prefix guard allowed its directory route to change"
    );
    drop(guard);
    std::fs::rename(&root.0, &moved.0)?;
    std::fs::rename(&moved.0, &root.0)?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_nonregular_and_relative_lock_routes_refuse() -> Result {
    let root = Scratch::new()?;
    std::fs::create_dir(root.0.join("lock"))?;
    assert!(matches!(
        PrefixGuard::exclusive(&root.0),
        Err(PrefixError::Nonregular)
    ));
    assert!(matches!(
        PrefixGuard::exclusive(std::path::Path::new("relative-installation")),
        Err(PrefixError::Nonregular)
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn linked_or_nonregular_lock_inputs_refuse_without_following_them() -> Result {
    let root = Scratch::new()?;
    let outside = root.0.join("outside");
    std::fs::write(&outside, b"untouched")?;
    std::os::unix::fs::symlink(&outside, root.0.join("lock"))?;
    assert!(matches!(
        PrefixGuard::exclusive(&root.0),
        Err(PrefixError::Nonregular)
    ));
    assert_eq!(std::fs::read(&outside)?, b"untouched");
    std::fs::remove_file(root.0.join("lock"))?;
    std::fs::hard_link(&outside, root.0.join("lock"))?;
    assert!(matches!(
        PrefixGuard::exclusive(&root.0),
        Err(PrefixError::Changed)
    ));
    std::fs::remove_file(root.0.join("lock"))?;
    assert!(std::process::Command::new("mkfifo")
        .arg(root.0.join("lock"))
        .status()?
        .success());
    assert!(matches!(
        PrefixGuard::exclusive(&root.0),
        Err(PrefixError::Nonregular)
    ));
    assert_eq!(std::fs::read(outside)?, b"untouched");
    Ok(())
}

#[cfg(unix)]
#[test]
fn group_or_other_write_access_refuses_before_creating_the_lock() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let root = Scratch::new()?;
    std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o777))?;
    assert!(matches!(
        PrefixGuard::shared(&root.0),
        Err(PrefixError::Writable)
    ));
    assert!(matches!(
        PrefixGuard::exclusive(&root.0),
        Err(PrefixError::Writable)
    ));
    assert!(!root.0.join("lock").exists(), "refusal created a lock file");
    Ok(())
}

#[cfg(unix)]
#[test]
fn linked_prefix_parent_refuses_guard_and_binary_selection() -> Result {
    let root = Scratch::new()?;
    let real = root.0.join("real");
    std::fs::create_dir(&real)?;
    std::fs::set_permissions(&real, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let linked = root.0.join("linked");
    std::os::unix::fs::symlink(&real, &linked)?;
    let prefix = linked.join("installation");
    std::fs::create_dir(&prefix)?;
    std::fs::set_permissions(&prefix, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    assert!(matches!(
        PrefixGuard::shared(&prefix),
        Err(PrefixError::Nonregular)
    ));
    assert!(matches!(
        PrefixGuard::exclusive(&prefix),
        Err(PrefixError::Nonregular)
    ));
    assert!(!prefix.join("lock").exists());
    let binary = prefix.join("versions/1111111111111111111111111111111111111111/bin/grapt-server");
    std::fs::create_dir_all(binary.parent().ok_or("missing bin parent")?)?;
    assert!(matches!(
        prefix_of_binary(&binary),
        Err(PrefixError::Nonregular)
    ));
    Ok(())
}
