use super::*;

#[cfg(any(target_os = "linux", windows))]
#[test]
fn write_targets_stay_exact_and_protected_resources_cannot_be_granted(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-write-targets-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let root = std::fs::canonicalize(root)?;
    let ordinary = root.join("ordinary");
    let protected = root.join("config");
    std::fs::write(&ordinary, "ordinary")?;
    std::fs::write(&protected, "protected")?;
    let mut policy = Policy {
        readable_roots: vec![root.clone()],
        writable_roots: vec![],
        protected_paths: vec![protected.clone(), root.join("missing-config")],
        denied_paths: vec![root.join("credentials")],
        network: NetworkAccess::Off,
    };
    let target = WriteTarget::resolve(&policy, &ordinary)?;
    let retry = policy.with_write_targets(std::slice::from_ref(&target))?;
    assert_eq!(retry.writable_roots, vec![ordinary.clone()]);
    assert_eq!(retry.protected_paths, policy.protected_paths);
    assert_eq!(retry.denied_paths, policy.denied_paths);
    assert_eq!(retry.network, NetworkAccess::Off);
    assert!(policy.writable_roots.is_empty());
    assert!(WriteTarget::resolve(&policy, &protected).is_err());
    assert!(WriteTarget::resolve(&policy, &root).is_err());
    assert!(WriteTarget::resolve(&policy, &root.join("new-file")).is_err());
    assert!(WriteTarget::resolve(&policy, std::path::Path::new("relative")).is_err());
    let alias = root.join("alias");
    std::fs::hard_link(&protected, &alias)?;
    assert!(WriteTarget::resolve(&policy, &alias).is_err());
    #[cfg(unix)]
    {
        let link = root.join("config-link");
        std::os::unix::fs::symlink(&protected, &link)?;
        assert!(WriteTarget::resolve(&policy, &link).is_err());
        std::fs::remove_file(link)?;
    }
    policy.protected_paths.push(ordinary);
    assert!(policy
        .with_write_targets(std::slice::from_ref(&target))
        .is_err());
    drop(target);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn write_target_replacement_requires_new_approval() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-write-replace-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let root = std::fs::canonicalize(root)?;
    let path = root.join("file");
    std::fs::write(&path, "before")?;
    let policy = Policy {
        readable_roots: vec![],
        writable_roots: vec![],
        protected_paths: vec![],
        denied_paths: vec![],
        network: NetworkAccess::Off,
    };
    let target = WriteTarget::resolve(&policy, &path)?;
    #[cfg(windows)]
    {
        assert!(std::fs::rename(&path, root.join("original")).is_err());
        let directory = root.join("directory");
        std::fs::create_dir(&directory)?;
        let pinned = WriteTarget::resolve(&policy, &directory)?;
        assert!(std::fs::rename(&directory, root.join("moved-directory")).is_err());
        assert!(std::fs::rename(&root, root.with_extension("moved")).is_err());
        std::fs::write(directory.join("permitted-content-write"), "allowed")?;
        drop(pinned);
        std::fs::rename(directory, root.join("moved-directory"))?;
    }
    #[cfg(target_os = "linux")]
    {
        let transferred = WriteTarget::from_file(&policy, &path, target.duplicate_file()?)?;
        assert_eq!(transferred.path(), path);
        let original = root.join("original");
        let other = root.join("other");
        std::fs::rename(&path, &original)?;
        std::fs::write(&path, "same-name replacement")?;
        assert!(WriteTarget::from_file(&policy, &path, target.duplicate_file()?).is_err());
        assert!(policy.with_write_targets(&[transferred]).is_err());
        assert!(policy
            .with_write_targets(std::slice::from_ref(&target))
            .is_err());
        std::fs::remove_file(&path)?;
        std::fs::write(&other, "replacement")?;
        std::os::unix::fs::symlink(&other, &path)?;
        assert!(policy
            .with_write_targets(std::slice::from_ref(&target))
            .is_err());
        // The native binding source stays on the approved object after the
        // approved name becomes a symlink, never on its replacement.
        use std::os::fd::AsRawFd;
        std::fs::write(
            format!("/proc/self/fd/{}", target.native_descriptor().as_raw_fd()),
            "approved object",
        )?;
        assert_eq!(std::fs::read_to_string(original)?, "approved object");
        assert_eq!(std::fs::read_to_string(&other)?, "replacement");
        let fresh = WriteTarget::resolve(&policy, &path)?;
        assert_eq!(
            policy
                .with_write_targets(std::slice::from_ref(&fresh))?
                .writable_roots,
            vec![other]
        );
    }
    drop(target);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

fn request() -> Result<ShellRequest, std::io::Error> {
    let root = std::env::current_dir()?;
    Ok(ShellRequest {
        command_line: "cmd.exe /D /C exit 0".into(),
        working_directory: root.clone(),
        environment: vec!["SystemRoot=C:\\Windows".into()],
        timeout_ms: 1000,
        policy: Policy {
            readable_roots: vec![],
            writable_roots: vec![root.clone()],
            protected_paths: vec![root.join("not-created-yet")],
            denied_paths: vec![],
            network: NetworkAccess::Off,
        },
    })
}

#[test]
fn malformed_requests_refuse_before_any_process_effect() -> Result<(), Box<dyn std::error::Error>> {
    let mut request = request()?;
    // A missing protected file is intentional and must remain protectable.
    assert!(request.validate().is_ok());
    request.policy.writable_roots.push("relative".into());
    let failure = Mxc
        .spawn(&request)
        .err()
        .ok_or("relative root was accepted")?;
    assert!(!failure.effects_possible);
    assert!(matches!(
        failure.enforcement,
        RealizedEnforcement::Degraded { .. }
    ));
    request.policy.writable_roots.pop();
    let filesystem_root = request
        .working_directory
        .ancestors()
        .last()
        .ok_or("fixture filesystem root absent")?
        .to_path_buf();
    request.policy.writable_roots.push(filesystem_root);
    let failure = Mxc
        .spawn(&request)
        .err()
        .ok_or("filesystem root was writable")?;
    assert!(!failure.effects_possible);
    assert!(failure.reason.contains("filesystem root"));
    request.policy.writable_roots.pop();
    request.environment.push("BAD\0=value".into());
    assert!(request.validate().is_err());
    request.environment.pop();
    request.command_line.push('\0');
    assert!(request.validate().is_err());
    Ok(())
}

#[cfg(not(windows))]
#[test]
fn unavailable_backend_is_a_degraded_refusal_without_fallback(
) -> Result<(), Box<dyn std::error::Error>> {
    let failure = Mxc
        .spawn(&request()?)
        .err()
        .ok_or("MXC ran on a non-Windows host")?;
    assert!(!failure.effects_possible);
    assert!(
        matches!(failure.enforcement, RealizedEnforcement::Degraded { backend, reason } if backend == "mxc" && reason.contains("requires Windows"))
    );
    #[cfg(not(target_os = "macos"))]
    {
        let failure = Seatbelt::new(std::env::current_exe()?)
            .with_executable(std::env::current_dir()?.join("absent-seatbelt"))
            .spawn(&request()?)
            .err()
            .ok_or("Seatbelt ran on a non-macOS host")?;
        assert!(!failure.effects_possible);
        assert!(matches!(failure.enforcement,
            RealizedEnforcement::Degraded { backend, reason }
            if backend == "seatbelt" && reason.contains("requires macOS")));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn missing_seatbelt_refuses_before_command_execution() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let root =
        std::env::temp_dir().join(format!("grapt-seatbelt-unavailable-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let root = std::fs::canonicalize(root)?;
    let mut request = request()?;
    request.command_line = "printf escaped > marker".into();
    request.working_directory = root.clone();
    request.policy.writable_roots = vec![root.clone()];
    request.policy.protected_paths = vec![];
    let failure = Seatbelt::new(std::env::current_exe()?)
        .with_executable(root.join("absent-seatbelt"))
        .spawn(&request)
        .err()
        .ok_or("missing Seatbelt launcher ran")?;
    assert!(!failure.effects_possible);
    assert!(matches!(failure.enforcement,
        RealizedEnforcement::Degraded { backend, reason }
        if backend == "seatbelt" && reason.contains("unavailable")));
    let launcher = root.join("deprecated-seatbelt");
    std::fs::write(
        &launcher,
        "#!/bin/sh\nprintf 'sandbox-exec is deprecated\\n' >&2\nexit 70\n",
    )?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))?;
    let failure = Seatbelt::new(std::env::current_exe()?)
        .with_executable(launcher.clone())
        .spawn(&request)
        .err()
        .ok_or("deprecated Seatbelt launcher ran a command")?;
    assert!(!failure.effects_possible);
    assert!(matches!(failure.enforcement,
        RealizedEnforcement::Degraded { backend, reason }
        if backend == "seatbelt" && reason.contains("deprecated")));
    assert!(!root.join("marker").exists());
    std::fs::remove_file(launcher)?;
    std::fs::remove_dir(root)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn seatbelt_rejects_mutable_protected_aliases_before_running_source(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-seatbelt-refusal-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let root = std::fs::canonicalize(root)?;
    let protected = root.join("config");
    let alias = root.join("alias");
    std::fs::write(&protected, "original")?;
    let mut request = request()?;
    request.command_line = "printf changed > config".into();
    request.working_directory = root.clone();
    request.policy.writable_roots = vec![root.clone()];
    request.policy.protected_paths = vec![protected.clone()];
    let backend = Seatbelt::new(std::env::current_exe()?);
    #[cfg(target_os = "linux")]
    {
        let grant = root.join("grant");
        std::fs::write(&grant, "original")?;
        let target = WriteTarget::resolve(&request.policy, &grant)?;
        let failure = backend
            .spawn_with_write_targets(&request, &[target])
            .err()
            .ok_or("unsupported Seatbelt grant was accepted")?;
        assert!(!failure.effects_possible);
        assert!(failure.reason.contains("write grants are not supported"));
        assert_eq!(std::fs::read_to_string(&protected)?, "original");
        std::fs::remove_file(grant)?;
    }
    std::fs::hard_link(&protected, &alias)?;
    let failure = backend
        .spawn(&request)
        .err()
        .ok_or("hard-link alias was accepted")?;
    assert!(!failure.effects_possible);
    assert!(failure.reason.contains("hard-link aliases"));
    std::fs::remove_file(&alias)?;
    std::os::unix::fs::symlink(&root, &alias)?;
    request.policy.protected_paths = vec![alias.join("config")];
    let failure = backend
        .spawn(&request)
        .err()
        .ok_or("writable symlink was accepted")?;
    assert!(!failure.effects_possible);
    assert!(failure.reason.contains("writable symlink"));
    // The alias is still writable when a different lexical prefix reaches it.
    let peer = root.with_file_name(format!("grapt-seatbelt-peer-{}", std::process::id()));
    std::fs::create_dir(&peer)?;
    let indirect = peer
        .join("..")
        .join(root.file_name().ok_or("root has no name")?)
        .join("alias/config");
    request.policy.protected_paths = vec![indirect];
    let failure = backend
        .spawn(&request)
        .err()
        .ok_or("indirect writable alias was accepted")?;
    assert!(!failure.effects_possible);
    assert!(
        failure.reason.contains("writable symlink"),
        "{}",
        failure.reason
    );
    std::fs::remove_dir(peer)?;
    assert_eq!(std::fs::read_to_string(&protected)?, "original");
    std::fs::remove_file(alias)?;
    use std::os::unix::ffi::OsStringExt;
    let non_unicode = root.join(std::ffi::OsString::from_vec(b"non-unicode-\xff".to_vec()));
    request.policy.protected_paths = vec![non_unicode];
    let failure = backend
        .spawn(&request)
        .err()
        .ok_or("non-Unicode policy path was accepted")?;
    assert!(!failure.effects_possible);
    assert!(failure.reason.contains("Unicode"));
    // APFS rejects non-UTF-8 directory names. Linux can also exercise a valid
    // input path whose symlink target resolves to a non-Unicode name.
    #[cfg(target_os = "linux")]
    {
        let workspace = root.join("workspace");
        let non_unicode =
            workspace.join(std::ffi::OsString::from_vec(b"non-unicode-\xff".to_vec()));
        std::fs::create_dir(&workspace)?;
        std::fs::create_dir(&non_unicode)?;
        std::fs::write(non_unicode.join("config"), "original")?;
        let alias = root.join("fixed-alias");
        std::os::unix::fs::symlink(&non_unicode, &alias)?;
        request.policy.writable_roots = vec![workspace.clone()];
        request.policy.protected_paths = vec![alias.join("config")];
        let failure = backend
            .spawn(&request)
            .err()
            .ok_or("lossy protected path was accepted")?;
        assert!(!failure.effects_possible);
        assert!(failure.reason.contains("bounded Unicode"));
        std::fs::remove_file(alias)?;
        std::fs::remove_file(non_unicode.join("config"))?;
        std::fs::remove_dir(non_unicode)?;
        std::fs::remove_dir(workspace)?;
    }
    std::fs::remove_file(protected)?;
    std::fs::remove_dir(root)?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn linux_refuses_missing_bwrap_and_mutable_protected_links_without_running_source(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-bwrap-refusal-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let outside = root.join("outside");
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(&outside, "original")?;
    let mut request = request()?;
    request.command_line = "printf SHOULD_NOT_RUN > should-not-run".into();
    request.working_directory = workspace.clone();
    request.policy.readable_roots = vec!["/".into()];
    request.policy.writable_roots = vec![workspace.clone()];
    request.policy.protected_paths.clear();
    let missing = workspace.join("missing-config");
    request.policy.protected_paths.push(missing.clone());
    request.environment = vec!["PATH=/usr/bin:/bin".into()];
    let backend = Bwrap::new(std::env::current_exe()?).with_executable(root.join("missing-bwrap"));
    let failure = backend.spawn(&request).err().ok_or("missing bwrap ran")?;
    assert!(!failure.effects_possible);
    assert!(matches!(failure.enforcement,
        RealizedEnforcement::Degraded { backend, reason } if backend == "bwrap" && !reason.is_empty()));
    assert!(!workspace.join("should-not-run").exists());
    assert!(
        !missing.exists(),
        "failed startup retained a synthetic target"
    );
    let link = workspace.join("protected-link");
    std::os::unix::fs::symlink(&outside, &link)?;
    request.policy.protected_paths.push(link.clone());
    let failure = backend
        .spawn(&request)
        .err()
        .ok_or("mutable protected link was accepted")?;
    assert!(!failure.effects_possible);
    assert!(failure.reason.contains("writable symlink"));
    assert_eq!(std::fs::read_to_string(&outside)?, "original");
    assert!(!workspace.join("should-not-run").exists());
    assert!(
        !missing.exists(),
        "policy refusal retained a synthetic target"
    );
    std::fs::remove_file(link)?;
    let alias = workspace.join("config-alias");
    std::fs::hard_link(&outside, &alias)?;
    request.policy.protected_paths = vec![outside.clone()];
    let failure = backend
        .spawn(&request)
        .err()
        .ok_or("hard-link alias was accepted")?;
    assert!(
        failure.reason.contains("hard-link aliases"),
        "{}",
        failure.reason
    );
    assert!(!failure.effects_possible);
    assert_eq!(std::fs::read_to_string(&outside)?, "original");
    std::fs::remove_file(alias)?;
    std::fs::remove_file(outside)?;
    std::fs::remove_dir(workspace)?;
    std::fs::remove_dir(root)?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn native_child_preserves_exit_and_refuses_protected_writes(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = std::env::temp_dir().join(format!("grapt-mxc-adapter-{}", std::process::id()));
    // Refuse to reuse another run's fixture, including after a crash.
    std::fs::create_dir(&fixture)?;
    let workspace = fixture.join("workspace");
    std::fs::create_dir(&workspace)?;
    let protected = workspace.join("config");
    let missing = workspace.join("missing-config");
    let nested = workspace.join("missing-parent/config");
    let outside = fixture.join("outside");
    std::fs::write(&protected, "original")?;
    let system = std::env::var("SystemRoot")?;
    let command = format!(
        "\"{system}\\System32\\cmd.exe\" /D /S /C \"echo permitted>allowed & echo changed>config & echo created>missing-config & mkdir missing-parent & echo nested>missing-parent/config & echo escaped>\"{}\" & exit /b 23\"",
        outside.display()
    );
    let mut request = request()?;
    request.command_line = command;
    request.working_directory = workspace.clone();
    request.environment = ["SystemRoot", "LOCALAPPDATA", "PATH", "ComSpec"]
        .into_iter()
        .map(|key| std::env::var(key).map(|value| format!("{key}={value}")))
        .collect::<Result<_, _>>()?;
    request.policy.readable_roots = vec![PathBuf::from(&system)];
    request.policy.writable_roots = vec![workspace.clone()];
    request.policy.protected_paths = vec![protected.clone(), missing.clone(), nested.clone()];
    let mut child = Mxc.spawn(&request)?;
    assert!(
        matches!(child.enforcement(), RealizedEnforcement::PlatformSandbox { backend } if backend == "mxc")
    );
    assert_eq!(child.wait()?, 23);
    assert!(std::fs::read_to_string(workspace.join("allowed"))?.contains("permitted"));
    assert_eq!(std::fs::read_to_string(protected)?, "original");
    assert!(!missing.exists(), "missing protected config was created");
    assert!(
        !nested.parent().ok_or("missing parent")?.exists(),
        "protected config parent was created"
    );
    assert!(!outside.exists(), "write escaped the workspace");
    std::fs::remove_dir_all(&fixture)?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn native_network_off_refuses_the_same_reachable_direct_ip(
) -> Result<(), Box<dyn std::error::Error>> {
    let system = std::env::var("SystemRoot")?;
    // A 3-second bound and distinct exits keep timeout/unreachability separate
    // from the kernel's access refusal. No DNS, proxy or offline hint is used.
    let script = "$c=New-Object Net.Sockets.TcpClient;try{$a=$c.BeginConnect('1.1.1.1',443,$null,$null);if(-not $a.AsyncWaitHandle.WaitOne(3000)){exit 78};$c.EndConnect($a);exit 0}catch{$e=$_.Exception;for($i=0;$i -lt 4 -and $e.InnerException;$i++){$e=$e.InnerException};if($e -is [Net.Sockets.SocketException] -and $e.SocketErrorCode -eq [Net.Sockets.SocketError]::AccessDenied){exit 77};exit 79}finally{$c.Dispose()}";
    let mut request = request()?;
    request.command_line = format!("\"{system}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoLogo -NoProfile -NonInteractive -Command \"{script}\"");
    request.working_directory = std::env::temp_dir();
    request.timeout_ms = 10_000;
    request.environment = ["SystemRoot", "LOCALAPPDATA", "PATH", "ComSpec"]
        .into_iter()
        .map(|key| std::env::var(key).map(|value| format!("{key}={value}")))
        .collect::<Result<_, _>>()?;
    request.policy = Policy {
        readable_roots: vec![PathBuf::from(format!(
            "{}\\",
            std::env::var("SystemDrive")?
        ))],
        writable_roots: vec![],
        protected_paths: vec![],
        denied_paths: vec![],
        network: NetworkAccess::On,
    };
    assert_eq!(
        Mxc.spawn(&request)?.wait()?,
        0,
        "direct-IP endpoint must be reachable with network on"
    );
    request.policy.network = NetworkAccess::Off;
    assert_eq!(
        Mxc.spawn(&request)?.wait()?,
        77,
        "network-off must fail with socket access denied"
    );
    Ok(())
}
