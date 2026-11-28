use super::*;
use std::path::PathBuf;

fn test_plan_and_scratch(
    parent: &Path,
) -> Result<(PrivateScratch, PathBuf, grapt_sandbox::Policy, Vec<String>), String> {
    let canonical_parent = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
    let workspace = canonical_parent.join("workspace");
    std::fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
    let plan = Plan {
        git_metadata_read_only: false,
        readable: vec![PathBuf::from("/")],
        writable: vec![workspace],
        protected: Vec::new(),
        denied: Vec::new(),
        network: false,
        private_temp_parent: Some(canonical_parent),
        protected_scratch_anchor: None,
    };
    let mut policy = plan.policy();
    let mut env = vec![format!("TMPDIR={}", parent.display())];
    let scratch = PrivateScratch::attach(&plan, &mut policy, &mut env)?
        .ok_or("expected private scratch from planner decision")?;
    let scratch_path = std::fs::canonicalize(scratch.path().ok_or("missing scratch path")?)
        .map_err(|e| e.to_string())?;
    Ok((scratch, scratch_path, policy, env))
}

#[cfg(unix)]
fn chmod_0700(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())
}

#[test]
fn private_scratch_honors_planner_parent_and_cleans_readonly_subdir() -> Result<(), String> {
    let configured_temp = tempfile::Builder::new()
        .prefix("grapt-configured-tmpdir-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    chmod_0700(configured_temp.path())?;
    let canonical_parent =
        std::fs::canonicalize(configured_temp.path()).map_err(|e| e.to_string())?;
    let (scratch, scratch_path, policy, env) = test_plan_and_scratch(configured_temp.path())?;
    assert!(scratch_path.starts_with(&canonical_parent));
    assert!(policy.writable_roots.contains(&scratch_path));
    assert!(!policy.writable_roots.contains(&canonical_parent));
    assert!(env.contains(&format!("TMPDIR={}", scratch_path.display())));
    assert!(env.contains(&format!("TEMP={}", scratch_path.display())));
    assert!(env.contains(&format!("TMP={}", scratch_path.display())));

    // Create read-only (0500) and mode-000 subdirectories plus a read-only
    // file inside scratch, and even strip permissions from scratch root
    // itself — pinned root_fd must still relax and remove everything.
    let locked_dir = scratch_path.join("locked");
    let zero_dir = scratch_path.join("zero");
    std::fs::create_dir(&locked_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir(&zero_dir).map_err(|e| e.to_string())?;
    std::fs::write(locked_dir.join("artifact.txt"), "data").map_err(|e| e.to_string())?;
    std::fs::write(zero_dir.join("secret.txt"), "data").map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            locked_dir.join("artifact.txt"),
            std::fs::Permissions::from_mode(0o400),
        )
        .map_err(|e| e.to_string())?;
        std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o500))
            .map_err(|e| e.to_string())?;
        std::fs::set_permissions(&zero_dir, std::fs::Permissions::from_mode(0o000))
            .map_err(|e| e.to_string())?;
        std::fs::set_permissions(&scratch_path, std::fs::Permissions::from_mode(0o000))
            .map_err(|e| e.to_string())?;
    }
    scratch.cleanup()?;
    assert!(
        !scratch_path.exists(),
        "private scratch directory survived cleanup: {}",
        scratch_path.display()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn write_search_only_0300_temp_parent_attaches_and_cleans_up() -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;

    let outer = tempfile::Builder::new()
        .prefix("grapt-scratch-0300-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let parent = outer.path().join("wx-only");
    std::fs::create_dir(&parent).map_err(|e| e.to_string())?;
    let workspace = parent.join("ws");
    std::fs::create_dir(&workspace).map_err(|e| e.to_string())?;
    // Strip read bit from parent directory (0300: write + search only).
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o300))
        .map_err(|e| e.to_string())?;

    let result = (|| -> Result<(), String> {
        let plan = Plan {
            git_metadata_read_only: false,
            readable: vec![PathBuf::from("/")],
            writable: vec![workspace.clone()],
            protected: Vec::new(),
            denied: Vec::new(),
            network: false,
            private_temp_parent: Some(parent.clone()),
            protected_scratch_anchor: None,
        };
        let mut policy = plan.policy();
        let mut env = Vec::new();
        let scratch = PrivateScratch::attach(&plan, &mut policy, &mut env)?
            .ok_or("expected scratch under 0300 parent")?;
        let scratch_path = scratch.path().ok_or("missing scratch path")?.to_path_buf();
        std::fs::write(scratch_path.join("probe.txt"), "ok").map_err(|e| e.to_string())?;
        scratch.cleanup()?;
        assert!(!scratch_path.exists());
        Ok(())
    })();

    // Restore permissions so `outer` TempDir cleans up cleanly.
    let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700));
    result
}

#[cfg(unix)]
#[test]
fn unix_uid_anchor_refuses_loose_mode_and_symlinks() -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;

    let euid = nix::unistd::geteuid().as_raw();
    let anchor_name = format!("grapt-scratch-{euid}");

    let parent_loose = tempfile::Builder::new()
        .prefix("grapt-anchor-loose-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    chmod_0700(parent_loose.path())?;
    let loose_anchor = parent_loose.path().join(&anchor_name);
    std::fs::create_dir(&loose_anchor).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&loose_anchor, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    let loose_err = match test_plan_and_scratch(parent_loose.path()) {
        Ok(_) => return Err("loose 0755 anchor must be refused".to_owned()),
        Err(err) => err,
    };
    assert!(
        loose_err.contains("0700 ownership check"),
        "unexpected error for loose anchor: {loose_err}"
    );

    let parent_link = tempfile::Builder::new()
        .prefix("grapt-anchor-link-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    chmod_0700(parent_link.path())?;
    let decoy = parent_link.path().join("decoy");
    std::fs::create_dir(&decoy).map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink(&decoy, parent_link.path().join(&anchor_name))
        .map_err(|e| e.to_string())?;
    let link_err = match test_plan_and_scratch(parent_link.path()) {
        Ok(_) => return Err("symlinked anchor must be refused".to_owned()),
        Err(err) => err,
    };
    assert!(
        link_err.contains("anchor openat failed"),
        "unexpected error for symlinked anchor: {link_err}"
    );

    // Non-sticky group-writable (0775) and world-writable (0777) outer parents
    // must both be refused.
    for bad_mode in [0o775, 0o777] {
        let parent_mutable = tempfile::Builder::new()
            .prefix("grapt-anchor-mutable-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        std::fs::set_permissions(
            parent_mutable.path(),
            std::fs::Permissions::from_mode(bad_mode),
        )
        .map_err(|e| e.to_string())?;
        let mutable_res = test_plan_and_scratch(parent_mutable.path());
        let _ = chmod_0700(parent_mutable.path());
        let mutable_err = match mutable_res {
            Ok(_) => {
                return Err(format!(
                    "non-sticky {bad_mode:04o} outer parent must be refused"
                ));
            }
            Err(err) => err,
        };
        assert!(
            mutable_err.contains("must be root/euid-owned and sticky or non-group/world-writable"),
            "unexpected error for {bad_mode:04o} outer parent: {mutable_err}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn confined_cleanup_unlinks_symlinks_hardlinks_and_unix_sockets_safely() -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;

    let parent = tempfile::Builder::new()
        .prefix("gs-")
        .tempdir_in("/tmp")
        .map_err(|e| e.to_string())?;
    chmod_0700(parent.path())?;
    let outside = parent.path().join("outside");
    std::fs::create_dir(&outside).map_err(|e| e.to_string())?;
    let outside_file = outside.join("keep.txt");
    std::fs::write(&outside_file, "immutable").map_err(|e| e.to_string())?;
    std::fs::set_permissions(&outside_file, std::fs::Permissions::from_mode(0o400))
        .map_err(|e| e.to_string())?;

    let (scratch, scratch_path, _, _) = test_plan_and_scratch(parent.path())?;
    std::os::unix::fs::symlink(&outside, scratch_path.join("link-dir"))
        .map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink(&outside_file, scratch_path.join("link-file"))
        .map_err(|e| e.to_string())?;
    std::fs::hard_link(&outside_file, scratch_path.join("hard-file")).map_err(|e| e.to_string())?;
    let sock_path = scratch_path.join("s.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock_path)
        .map_err(|e| format!("bind unix socket: {e}"))?;
    drop(listener);

    scratch.cleanup()?;
    assert!(!scratch_path.exists());
    assert!(outside.is_dir());
    assert_eq!(
        std::fs::read_to_string(&outside_file).map_err(|e| e.to_string())?,
        "immutable"
    );
    let mode = std::fs::metadata(&outside_file)
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o400,
        "hardlink cleanup must never chmod the shared target inode"
    );
    Ok(())
}

#[test]
fn cleanup_after_stop_and_exit_code_preservation() -> Result<(), String> {
    let parent = tempfile::Builder::new()
        .prefix("grapt-scratch-unreaped-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    chmod_0700(parent.path())?;
    let (scratch, scratch_path, _, _) = test_plan_and_scratch(parent.path())?;
    let skipped = cleanup_after_stop(Some(scratch), false);
    assert!(
        skipped
            .as_deref()
            .is_some_and(|msg| msg.contains("child termination was not confirmed")),
        "expected explicit skip error, got {skipped:?}"
    );
    assert!(
        scratch_path.exists(),
        "unreaped cleanup must leave scratch untouched instead of walking against a live child"
    );
    let combined = combine_errors(
        "sandbox receipt was not delivered".to_owned(),
        [Some("sandbox reap failed: ECHILD".to_owned()), skipped],
    );
    assert!(combined.contains("sandbox receipt was not delivered"));
    assert!(combined.contains("sandbox reap failed: ECHILD"));
    assert!(combined.contains("child termination was not confirmed"));

    assert_eq!(finish_with_exit_code(0, Ok(())), (0, None));
    assert_eq!(finish_with_exit_code(42, Ok(())), (42, None));
    assert_eq!(
        finish_with_exit_code(0, Err("cleanup boom".to_owned())),
        (1, Some("cleanup boom".to_owned()))
    );
    assert_eq!(
        finish_with_exit_code(42, Err("cleanup boom".to_owned())),
        (42, Some("cleanup boom".to_owned()))
    );

    let _ = std::fs::remove_dir_all(&scratch_path);
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_scratch_cleans_readonly_preserves_hardlink_readonly_and_leaf_reparse(
) -> Result<(), String> {
    let parent = tempfile::Builder::new()
        .prefix("grapt-win-scratch-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let outside = parent.path().join("outside");
    std::fs::create_dir(&outside).map_err(|e| e.to_string())?;
    let outside_ro = outside.join("shared-readonly.txt");
    std::fs::write(&outside_ro, "immutable").map_err(|e| e.to_string())?;
    let mut ro_perms = std::fs::metadata(&outside_ro)
        .map_err(|e| e.to_string())?
        .permissions();
    ro_perms.set_readonly(true);
    std::fs::set_permissions(&outside_ro, ro_perms).map_err(|e| e.to_string())?;
    let outside_target = outside.join("target-dir");
    std::fs::create_dir(&outside_target).map_err(|e| e.to_string())?;
    let target_marker = outside_target.join("keep.txt");
    std::fs::write(&target_marker, "untouched").map_err(|e| e.to_string())?;

    let (scratch, scratch_path, _, _) = test_plan_and_scratch(parent.path())?;
    // (a) Nested directory + single-link read-only file inside scratch.
    let nested = scratch_path.join("a").join("b");
    std::fs::create_dir_all(&nested).map_err(|e| e.to_string())?;
    let single_ro = nested.join("single-ro.txt");
    std::fs::write(&single_ro, "temp").map_err(|e| e.to_string())?;
    let mut single_perms = std::fs::metadata(&single_ro)
        .map_err(|e| e.to_string())?
        .permissions();
    single_perms.set_readonly(true);
    std::fs::set_permissions(&single_ro, single_perms).map_err(|e| e.to_string())?;

    // (b) Multiply-linked read-only hard link inside scratch.
    std::fs::hard_link(&outside_ro, scratch_path.join("hard-ro.txt")).map_err(|e| e.to_string())?;

    // (c) Reparse point / NTFS directory junction leaf inside scratch (standard-user safe).
    let link_path = scratch_path.join("link-dir");
    let mklink = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&link_path)
        .arg(&outside_target)
        .output()
        .map_err(|e| format!("failed to invoke cmd mklink /J: {e}"))?;
    assert!(
        mklink.status.success(),
        "mklink /J failed: stdout={} stderr={}",
        String::from_utf8_lossy(&mklink.stdout),
        String::from_utf8_lossy(&mklink.stderr)
    );

    scratch.cleanup()?;
    assert!(
        !scratch_path.exists(),
        "Windows scratch survived cleanup: {}",
        scratch_path.display()
    );
    assert!(
        std::fs::metadata(&outside_ro)
            .map_err(|e| e.to_string())?
            .permissions()
            .readonly(),
        "hardlink cleanup must never clear readonly on the outside link"
    );
    assert_eq!(
        std::fs::read_to_string(&target_marker).map_err(|e| e.to_string())?,
        "untouched",
        "reparse leaf deletion must never delete target directory contents"
    );
    // Clear readonly on outside_ro so TempDir drops cleanly.
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut perms = std::fs::metadata(&outside_ro)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(&outside_ro, perms);
    }
    Ok(())
}
