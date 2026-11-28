use super::*;
use crate::dogfood::catalog_config::CatalogEnvironment;

#[test]
fn isolation_defaults_on_and_user_disable_turns_it_off() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!(
        "grapt-sandbox-default-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir(&root).map_err(|e| e.to_string())?;
    let result = (|| {
        let workspace = root.join("work");
        for path in [&workspace, &root.join("config")] {
            std::fs::create_dir(path).map_err(|e| e.to_string())?;
        }
        let sources = || CatalogSources::read(&workspace, CatalogEnvironment::isolated(&root));
        let default_plan =
            Plan::read(&workspace, &sources()?)?.ok_or("sandbox did not default on")?;
        assert!(default_plan.network);
        // When `workspace` lives under `std::env::temp_dir()`, the shared
        // ancestor temp directory must NOT be added to `writable`, so Linux
        // mount preparation never creates `.grapt`/`grapt.jsonc` targets in it.
        let canonical_temp =
            std::fs::canonicalize(std::env::temp_dir()).map_err(|e| e.to_string())?;
        assert!(
            !default_plan.writable.contains(&canonical_temp),
            "ancestor temp_dir was exposed writable: {:?}",
            default_plan.writable
        );
        std::fs::write(
            workspace.join("grapt.json"),
            br#"{"sandbox":{"enabled":false}}"#,
        )
        .map_err(|e| e.to_string())?;
        assert!(
            Plan::read(&workspace, &sources()?)?.is_some(),
            "repository config disabled default sandbox"
        );
        std::fs::write(
            root.join("config/grapt.json"),
            br#"{"sandbox":{"enabled":false}}"#,
        )
        .map_err(|e| e.to_string())?;
        assert!(Plan::read(&workspace, &sources()?)?.is_none());
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&root);
    result
}

#[test]
#[cfg_attr(windows, expect(clippy::cognitive_complexity, reason = "legacy"))]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn isolation_defaults_off_and_repository_settings_cannot_widen_a_user_policy() -> Result<(), String>
{
    let root = std::env::temp_dir().join(format!(
        "grapt-sandbox-config-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir(&root).map_err(|e| e.to_string())?;
    let result = (|| {
        let workspace = root.join("work");
        let granted = root.join("granted");
        let outside = root
            .ancestors()
            .last()
            .ok_or("fixture has no filesystem root")?;
        for path in [&workspace, &granted, &root.join("config")] {
            std::fs::create_dir(path).map_err(|e| e.to_string())?;
        }
        let sources = || CatalogSources::read(&workspace, CatalogEnvironment::isolated(&root));
        assert!(Plan::read(&workspace, &sources()?)?.is_some());
        std::fs::write(root.join("config/grapt.json"), serde_json::json!({"sandbox":{"enabled":true,"writable_roots":[granted],"network":true}}).to_string()).map_err(|e| e.to_string())?;
        std::fs::write(workspace.join("grapt.json"), serde_json::json!({"sandbox":{"enabled":false,"writable_roots":[outside],"network":false}}).to_string()).map_err(|e| e.to_string())?;
        let plan = Plan::read(&workspace, &sources()?)?.ok_or("user sandbox policy disappeared")?;
        let workspace_root = std::fs::canonicalize(&workspace).map_err(|e| e.to_string())?;
        assert!(!workspace.join(".git").exists());
        for control in ["config", "config.worktree", "hooks"] {
            assert!(plan
                .protected
                .contains(&workspace_root.join(".git").join(control)));
        }
        assert!(plan
            .writable
            .contains(&std::fs::canonicalize(&granted).map_err(|e| e.to_string())?));
        let outside = std::fs::canonicalize(outside).map_err(|e| e.to_string())?;
        assert!(!plan
            .writable
            .iter()
            .any(|granted| outside.starts_with(granted)));
        assert!(!plan.network);
        let user_config = root.join("config/grapt.json");
        let original = std::fs::read(&user_config).map_err(|error| error.to_string())?;
        std::fs::write(
            &user_config,
            serde_json::json!({"sandbox":{"enabled":true,"writable_roots":[outside]}}).to_string(),
        )
        .map_err(|error| error.to_string())?;
        assert!(
            Plan::read(&workspace, &sources()?).is_err(),
            "user root grant widened containment"
        );
        std::fs::write(&user_config, original).map_err(|error| error.to_string())?;
        assert!(
            Plan::read(&outside, &sources()?).is_err(),
            "workspace root widened containment"
        );
        #[cfg(windows)]
        {
            let system = std::fs::canonicalize(PathBuf::from(
                std::env::var_os("SystemRoot").ok_or("SystemRoot absent")?,
            ))
            .map_err(|error| error.to_string())?;
            assert!(plan.readable.iter().any(|path| system.starts_with(path)));
        }
        assert!(plan.protected.contains(
            &std::fs::canonicalize(root.join("config/grapt.json")).map_err(|e| e.to_string())?
        ));
        let nested = workspace.join("nested");
        std::fs::create_dir(&nested).map_err(|e| e.to_string())?;
        std::fs::create_dir(workspace.join(".git")).map_err(|e| e.to_string())?;
        let plan = Plan::read(&workspace, &sources()?)?.ok_or("repository-root policy absent")?;
        let controls = std::fs::canonicalize(workspace.join(".git")).map_err(|e| e.to_string())?;
        for control in ["config", "config.worktree", "hooks"] {
            assert!(plan.protected.contains(&controls.join(control)));
        }
        let plan = Plan::read(&nested, &sources()?)?.ok_or("nested repository policy absent")?;
        let nested_root = std::fs::canonicalize(&nested).map_err(|e| e.to_string())?;
        for control in ["config", "config.worktree", "hooks"] {
            assert!(
                plan.protected.contains(&controls.join(control)),
                "ancestor repository control is not protected: {control}"
            );
            assert!(plan
                .protected
                .contains(&nested_root.join(".git").join(control)));
        }
        std::fs::remove_dir(workspace.join(".git")).map_err(|e| e.to_string())?;
        let gitdir = root.join("real-git");
        std::fs::create_dir(&gitdir).map_err(|e| e.to_string())?;
        let common = root.join("common");
        std::fs::create_dir(&common).map_err(|e| e.to_string())?;
        let common = std::fs::canonicalize(common).map_err(|e| e.to_string())?;
        std::fs::write(gitdir.join("commondir"), "../common").map_err(|e| e.to_string())?;
        std::fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .map_err(|e| e.to_string())?;
        let plan = Plan::read(&nested, &sources()?)?.ok_or("nested worktree policy absent")?;
        assert!(
            plan.protected.contains(&gitdir.join("config"))
                || plan
                    .protected
                    .iter()
                    .any(|path| path.ends_with("real-git/config"))
        );
        for control in ["config", "config.worktree", "hooks"] {
            assert!(
                plan.protected.contains(&common.join(control)),
                "linked worktree common control is not protected: {control}"
            );
        }
        let plan = Plan::read(&workspace, &sources()?)?.ok_or("worktree-root policy absent")?;
        let pointer =
            std::fs::canonicalize(workspace.join(".git")).map_err(|error| error.to_string())?;
        assert!(plan.protected.contains(&pointer));
        assert!(
            !plan
                .protected
                .iter()
                .any(|path| path != &pointer && path.starts_with(&pointer)),
            "a Git pointer file cannot contain protected config or hooks"
        );
        let gitdir = std::fs::canonicalize(&gitdir).map_err(|error| error.to_string())?;
        for control in ["config", "config.worktree", "hooks"] {
            assert!(plan.protected.contains(&gitdir.join(control)));
            assert!(plan.protected.contains(&common.join(control)));
        }
        Ok(())
    })();
    std::fs::remove_dir_all(&root).map_err(|e| e.to_string())?;
    result
}

#[test]
fn default_policy_denies_ssh_and_protects_agents_md_and_git_controls() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!(
        "grapt-sandbox-defaults-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir(&root).map_err(|e| e.to_string())?;
    let result = (|| {
        let workspace = root.join("work");
        let nested = workspace.join("sub");
        for path in [
            &workspace,
            &nested,
            &workspace.join(".git"),
            &root.join("config"),
        ] {
            std::fs::create_dir(path).map_err(|e| e.to_string())?;
        }
        std::fs::write(
            root.join("config/grapt.json"),
            serde_json::json!({"sandbox":{"enabled":true}}).to_string(),
        )
        .map_err(|e| e.to_string())?;
        let sources = CatalogSources::read(&nested, CatalogEnvironment::isolated(&root))?;
        let plan = Plan::read(&nested, &sources)?.ok_or("sandbox plan absent")?;
        let workspace_root = std::fs::canonicalize(&workspace).map_err(|e| e.to_string())?;
        let nested_root = std::fs::canonicalize(&nested).map_err(|e| e.to_string())?;
        let expected_ssh = PathBuf::from(crate::dogfood::permission_preset::resolved_path(
            &root.join("home").join(".ssh"),
        )?);
        assert!(plan.denied.contains(&expected_ssh));
        assert!(plan.policy().denied_paths.contains(&expected_ssh));
        assert!(plan.protected.contains(&workspace_root.join("AGENTS.md")));
        assert!(plan.protected.contains(&nested_root.join("AGENTS.md")));
        for control in ["config", "config.worktree", "hooks"] {
            assert!(plan
                .protected
                .contains(&workspace_root.join(".git").join(control)));
        }
        Ok(())
    })();
    std::fs::remove_dir_all(&root).map_err(|e| e.to_string())?;
    result
}

#[test]
fn callback_output_cannot_replace_native_receipts_or_change_interpolation_bytes(
) -> Result<(), String> {
    let actual = Receipt::started(RealizedEnforcement::PlatformSandbox {
        backend: "mxc".into(),
    });
    let altered = actual.output(
        br#"{"output":"kept","enforcement":{"kind":"harness_only"},"ran":false}"#.to_vec(),
        false,
        false,
    )?;
    let receipt = Receipt::parse(std::str::from_utf8(&altered).map_err(|e| e.to_string())?)
        .ok_or("receipt missing after callback")?;
    assert_eq!(receipt.enforcement, actual.enforcement);
    assert_eq!(receipt.ran, Some(true));
    let command = "value\nCommand exited with code 0.";
    let durable = actual.output(command.as_bytes().to_vec(), true, true)?;
    assert_eq!(
        command_text(std::str::from_utf8(&durable).map_err(|e| e.to_string())?)?,
        command
    );
    assert!(
        actual.output(vec![0xff], true, true).is_err(),
        "machine output accepted altered UTF-8 bytes"
    );
    let replaced = actual.output(vec![0xff], true, false)?;
    let replaced: serde_json::Value =
        serde_json::from_slice(&replaced).map_err(|e| e.to_string())?;
    assert!(replaced["output"]
        .as_str()
        .ok_or("model output absent")?
        .contains("invalid UTF-8 bytes were replaced explicitly"));
    let public = actual.output("界\"\n".repeat(1000).into_bytes(), true, false)?;
    let public = project_receipt(
        std::str::from_utf8(&public).map_err(|e| e.to_string())?,
        4096,
    )
    .ok_or("public receipt absent")??;
    assert!(public.len() <= 4096);
    let decoded = super::receipt(&public).ok_or("public receipt was cut into invalid JSON")?;
    assert_eq!(decoded["enforcement"]["kind"], "platform_sandbox");
    assert_eq!(decoded["ran"], true);
    assert!(decoded["omitted_output_bytes"]
        .as_u64()
        .is_some_and(|n| n > 0));
    let oversized = vec![1; grapt_runtime::TOOL_ARTIFACT_MAX_BYTES as usize];
    let bounded = actual.output(oversized.clone(), true, false)?;
    assert!(bounded.len() as u64 <= grapt_runtime::TOOL_ARTIFACT_MAX_BYTES);
    assert!(Receipt::parse(std::str::from_utf8(&bounded).map_err(|e| e.to_string())?).is_some());
    assert!(actual.output(oversized, true, true).is_err());
    let seatbelt_started = Receipt::started(RealizedEnforcement::PlatformSandbox {
        backend: "seatbelt".into(),
    });
    let seatbelt_out = seatbelt_started.output(b"ok".to_vec(), true, false)?;
    let parsed_started =
        Receipt::parse(std::str::from_utf8(&seatbelt_out).map_err(|e| e.to_string())?)
            .ok_or("seatbelt platform_sandbox receipt was rejected")?;
    assert_eq!(parsed_started.enforcement, seatbelt_started.enforcement);
    assert_eq!(parsed_started.ran, Some(true));
    let seatbelt_refusal = refusal(
        &RealizedEnforcement::Degraded {
            backend: "seatbelt".into(),
            reason: "macOS Seatbelt shell containment is not wired into this build".into(),
        },
        "macOS Seatbelt sandbox unavailable; no command ran; set \"sandbox\": {\"enabled\": false} in user grapt.json to opt out",
    );
    let parsed_degraded =
        Receipt::parse(&seatbelt_refusal).ok_or("seatbelt degraded receipt was rejected")?;
    assert_eq!(parsed_degraded.ran, Some(false));
    let preserved = parsed_degraded.error(&seatbelt_refusal);
    assert!(
        preserved.contains("opt out") && preserved.contains("seatbelt"),
        "seatbelt degraded refusal lost opt-out detail: {preserved}"
    );
    Ok(())
}

#[test]
fn instruction_slots_targets_and_denied_ssh_enter_policy_and_fingerprint() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!(
        "grapt-sandbox-instructions-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir(&root).map_err(|e| e.to_string())?;
    let result = (|| {
        let repo = root.join("repo");
        let subdir = repo.join("sub");
        let external = root.join("external-agents.md");
        for dir in [&repo, &repo.join(".git"), &subdir, &root.join("config")] {
            std::fs::create_dir(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&external, "rules").map_err(|e| e.to_string())?;
        std::fs::write(
            root.join("config/grapt.json"),
            br#"{"sandbox":{"enabled":true}}"#,
        )
        .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, subdir.join("AGENTS.md"))
            .map_err(|e| e.to_string())?;
        #[cfg(windows)]
        std::fs::write(subdir.join("AGENTS.md"), "rules").map_err(|e| e.to_string())?;
        let sources = CatalogSources::read(&subdir, CatalogEnvironment::isolated(&root))?;
        let plan = Plan::read(&subdir, &sources)?.ok_or("sandbox plan absent")?;
        let repo_root = std::fs::canonicalize(&repo).map_err(|e| e.to_string())?;
        let subdir_root = std::fs::canonicalize(&subdir).map_err(|e| e.to_string())?;
        for expected in [
            subdir_root.join("AGENTS.md"),
            subdir_root.join("AGENTS.override.md"),
            repo_root.join("AGENTS.md"),
            repo_root.join("AGENTS.override.md"),
        ] {
            assert!(
                plan.protected.contains(&expected),
                "missing protected instruction path: {}",
                expected.display()
            );
        }
        #[cfg(unix)]
        {
            let target = std::fs::canonicalize(&external).map_err(|e| e.to_string())?;
            assert!(
                plan.protected.contains(&target),
                "missing resolved symlink target: {}",
                target.display()
            );
        }
        let ssh = PathBuf::from(crate::dogfood::permission_preset::resolved_path(
            &sources.environment.home.join(".ssh"),
        )?);
        assert!(plan.denied.contains(&ssh), "missing denied .ssh path");
        let ssh_id = grapt_types::PathIdentity::canonical(crate::dogfood::tools::path_text(&ssh));
        assert!(
            plan.filesystem().read_deny().contains(&ssh_id),
            "denied .ssh path missing from capability read_deny domain"
        );
        let protected_only = grapt_types::ResolvedCapability::new(
            grapt_types::EffectClass::ProcessSpawn,
            None,
            grapt_types::FilesystemAuthority::new(
                plan.filesystem().read().to_vec(),
                plan.filesystem().write().to_vec(),
                vec![ssh_id.clone()],
            ),
            plan.egress(),
            plan.intended(),
        );
        let protected_and_denied = grapt_types::ResolvedCapability::new(
            grapt_types::EffectClass::ProcessSpawn,
            None,
            grapt_types::FilesystemAuthority::new(
                plan.filesystem().read().to_vec(),
                plan.filesystem().write().to_vec(),
                vec![ssh_id.clone()],
            )
            .with_read_deny(vec![ssh_id]),
            plan.egress(),
            plan.intended(),
        );
        assert_ne!(
            grapt_types::CapabilityId::derive(&protected_only),
            grapt_types::CapabilityId::derive(&protected_and_denied),
            "denied_paths must produce a distinct capability ID from protected_paths alone"
        );
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&root);
    result
}

#[test]
fn workspace_under_temp_dir_never_exposes_ancestor_or_obstructs_sibling() -> Result<(), String> {
    let dir = tempfile::Builder::new()
        .prefix("grapt-sandbox-tmpdir-sibling-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let workspace = dir.path().join("workspace-a");
    let sibling = dir.path().join("workspace-b");
    let config = dir.path().join("config");
    for path in [&workspace, &sibling, &config] {
        std::fs::create_dir(path).map_err(|e| e.to_string())?;
    }
    let sources_a = CatalogSources::read(&workspace, CatalogEnvironment::isolated(dir.path()))?;
    let plan = Plan::read(&workspace, &sources_a)?.ok_or("sandbox plan absent")?;
    let canonical_temp = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| e.to_string())?;
    let canonical_sibling = std::fs::canonicalize(&sibling).map_err(|e| e.to_string())?;
    assert!(
        !plan.writable.contains(&canonical_temp),
        "shared ancestor temp_dir must not be writable: {:?}",
        plan.writable
    );
    assert!(
        !plan
            .writable
            .iter()
            .any(|writable| canonical_sibling.starts_with(writable)),
        "sibling workspace must not fall under any writable root: {:?}",
        plan.writable
    );
    // Sibling catalog discovery remains completely unobstructed.
    let _sources_b = CatalogSources::read(&sibling, CatalogEnvironment::isolated(dir.path()))?;
    assert!(
        plan.private_temp_parent().is_some(),
        "planner must record private_temp_parent when ancestor temp_dir is omitted"
    );
    // The reserved private scratch root must be counted inside the 128-path
    // policy bound so a valid plan never overflows Policy's 128-path limit.
    assert!(config::check_path_budget(127, true).is_ok());
    assert!(config::check_path_budget(128, true).is_err());
    assert!(config::check_path_budget(128, false).is_ok());
    #[cfg(unix)]
    {
        // When a user explicitly grants temp as a writable root, the Unix
        // 0700 scratch anchor must be protected so no sandboxed command under
        // the same UID can tamper with sibling scratch directories.
        std::fs::write(
            config.join("grapt.json"),
            format!(
                r#"{{"sandbox":{{"writable_roots":[{:?}]}}}}"#,
                canonical_temp.to_string_lossy()
            ),
        )
        .map_err(|e| e.to_string())?;
        let sources_temp =
            CatalogSources::read(&workspace, CatalogEnvironment::isolated(dir.path()))?;
        let temp_plan = Plan::read(&workspace, &sources_temp)?.ok_or("sandbox plan absent")?;
        let anchor =
            canonical_temp.join(format!("grapt-scratch-{}", nix::unistd::geteuid().as_raw()));
        assert!(
            temp_plan.protected.contains(&anchor),
            "Unix scratch anchor must be protected when user writable_roots overlaps temp: {:?}",
            temp_plan.protected
        );
    }
    Ok(())
}
