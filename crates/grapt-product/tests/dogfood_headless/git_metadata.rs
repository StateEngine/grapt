//! The production tool dispatcher must preserve workspace writes while the OS
//! denies changes to resolved repository metadata, including linked worktrees.
use super::*;

#[test]
fn read_only_git_metadata_preserves_workspace_writes_through_shell_pipe_and_pty(
) -> Result<(), Box<dyn std::error::Error>> {
    exercise(false)
}

#[test]
fn read_only_linked_worktree_metadata_survives_nested_session_and_false_layer(
) -> Result<(), Box<dyn std::error::Error>> {
    exercise(true)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn exercise(linked: bool) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let base = root(if linked {
        "git-readonly-linked"
    } else {
        "git-readonly"
    });
    let service = base.join("service");
    prepare(&service, 120_000);
    // Authorize the linked common-directory probe explicitly. Command consent
    // must not remove the independently configured OS write restrictions.
    let mut settings = fs::read_to_string(service.join("service.conf"))?;
    settings.push_str("permission_preset=bypass\n");
    fs::write(service.join("service.conf"), settings)?;
    let install = base.join("install");
    let home = base.join("home");
    let config = base.join("config");
    let workspace = base.join("workspace");
    for path in [&install, &home, &config, &workspace] {
        fs::create_dir(path)?;
    }
    let repository = if linked {
        base.join("repository")
    } else {
        workspace.clone()
    };
    fs::create_dir_all(&repository)?;
    git_output(&repository, &["init", "--initial-branch=main"])?;
    fs::write(repository.join("tracked"), "committed workspace file")?;
    git_output(&repository, &["add", "tracked"])?;
    git_output(&repository, &["commit", "-m", "initial"])?;
    if linked {
        git_output(
            &repository,
            &[
                "worktree",
                "add",
                "--detach",
                workspace.to_str().ok_or("workspace is not Unicode")?,
            ],
        )?;
    }
    let common = fs::canonicalize(repository.join(".git"))?;
    let git = PathBuf::from(git_output(
        &workspace,
        &["rev-parse", "--absolute-git-dir"],
    )?);
    let head = git_output(&workspace, &["rev-parse", "HEAD"])?;
    let (prefix, suffix) = head
        .split_at_checked(2)
        .ok_or("Git object identity is too short")?;
    fs::create_dir_all(common.join("objects/aa"))?;
    let mut protected = vec![
        git.join("index"),
        common.join("refs/heads/main"),
        common.join("objects").join(prefix).join(suffix),
    ];
    if linked {
        protected.extend([workspace.join(".git"), git.join("commondir")]);
    }
    let before = protected
        .iter()
        .map(fs::read)
        .collect::<Result<Vec<_>, _>>()?;
    let directory = if linked {
        workspace.join("nested")
    } else {
        workspace.clone()
    };
    fs::create_dir_all(&directory)?;
    fs::write(
        config.join("grapt.json"),
        br#"{"sandbox":{"enabled":false,"git_metadata_read_only":true}}"#,
    )?;
    fs::write(directory.join("grapt.json"), br#"{"sandbox":{"enabled":false,"git_metadata_read_only":false},"permission":{"*":"allow"}}"#)?;
    // Existing writes, creation of metadata, and replacement of pointer files
    // must all fail. The ordinary write is the positive control for startup.
    let mut denied = protected.clone();
    denied.extend([
        git.join("index.lock"),
        common.join("refs/heads/new"),
        common.join("objects/aa/new"),
    ]);
    if linked {
        denied.push(directory.join(".git"));
    }
    let ordinary = directory.join("ordinary");
    let source = source(&ordinary, &protected, &denied);
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_git_shell",
            tool: "shell",
            arguments: serde_json::json!({"command":source,"timeout_ms":20000}),
        },
        Script::ToolInput {
            call: "call_git_pipe",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":source,"tty":false,"yield_time_ms":10000,"timeout_ms":20000}),
        },
        Script::ToolInput {
            call: "call_git_pty",
            tool: "exec_command",
            arguments: serde_json::json!({"cmd":source,"tty":true,"yield_time_ms":10000,"timeout_ms":20000}),
        },
        Script::Success,
    ]);
    let binary = install.join(if cfg!(windows) {
        "grapt-server.exe"
    } else {
        "grapt-server"
    });
    fs::copy(shipped_cli::product_binary(), &binary)?;
    let mut command = grapt_service::launcher_environment::command(binary);
    command
        .args(["serve-terminal", "--service"])
        .arg(&service)
        .args(["--port", "0"])
        .stdin(Stdio::piped())
        .env("GRAPT_TEST_CATALOG_ROOT", &base)
        .env("GRAPT_TEST_HOME", &home)
        .env("GRAPT_CONFIG_DIR", &config)
        .env(
            "GRAPT_LIVE_HEADLESS_LOOPBACK",
            format!("http://{}", provider.address),
        );
    grapt_service::launcher_environment::scrub(&mut command);
    let mut child = process_fixture::spawn_captured(&service, &mut command);
    let address = await_ready(&service, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":directory}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("Session absent")?
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"check workspace and Git metadata writes"}"#,
    );
    wait_calls(&provider.calls, 4);
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    for (index, call) in ["call_git_shell", "call_git_pipe", "call_git_pty"]
        .iter()
        .enumerate()
    {
        let request: serde_json::Value =
            serde_json::from_str(bodies.get(index + 1).ok_or("tool continuation absent")?)?;
        let raw = request["input"]
            .as_array()
            .ok_or("continuation input absent")?
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == *call)
            .and_then(|item| item["output"].as_str())
            .ok_or("tool result absent")?;
        let receipt: serde_json::Value =
            serde_json::from_str(raw).map_err(|error| format!("{call}: {error}: {raw}"))?;
        assert_eq!(
            receipt["enforcement"]["kind"], "platform_sandbox",
            "{call}: {raw}"
        );
        assert!(
            receipt["output"]
                .as_str()
                .is_some_and(|text| text.contains("GIT_POLICY_ENFORCED")),
            "{call}: {raw}"
        );
        assert!(
            !receipt["output"]
                .as_str()
                .is_some_and(|text| text.contains("METADATA_WRITE_ESCAPED")),
            "{call}: {raw}"
        );
    }
    assert_eq!(fs::read_to_string(ordinary)?, "WORKSPACE_WRITE");
    for (path, expected) in protected.iter().zip(before) {
        assert_eq!(fs::read(path)?, expected, "{}", path.display());
    }
    for path in denied.iter().skip(protected.len()) {
        assert!(
            !path.exists(),
            "metadata creation escaped: {}",
            path.display()
        );
    }
    child.stop_after_witness(&service);
    drop(child);
    process_fixture::cleanup(&base);
    Ok(())
}

fn git_output(directory: &Path, arguments: &[&str]) -> Result<String, String> {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null)
        .args([
            "-c",
            &format!("core.hooksPath={null}"),
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(arguments)
        .stdin(Stdio::null());
    let (code, output, error) = project_list::bounded(command, Duration::from_secs(30));
    if code != Some(0) {
        return Err(format!("fixture Git refused: {error}"));
    }
    Ok(output.trim().to_owned())
}

fn source(ordinary: &Path, readable: &[PathBuf], denied: &[PathBuf]) -> String {
    let quote = |path: &Path| {
        format!(
            "'{}'",
            path.to_string_lossy()
                .replace('\'', if cfg!(windows) { "''" } else { "'\\''" })
        )
    };
    if cfg!(windows) {
        let reads = readable
            .iter()
            .map(|path| format!("[IO.File]::ReadAllText({}) | Out-Null; ", quote(path)))
            .collect::<String>();
        let attempts = denied.iter().map(|path|format!("try {{ [IO.File]::WriteAllText({},'ESCAPE'); Write-Output METADATA_WRITE_ESCAPED; exit 91 }} catch {{}}; ",quote(path))).collect::<String>();
        format!("powershell.exe -NoProfile -NonInteractive -Command \"$ErrorActionPreference='Stop'; {reads} [IO.File]::WriteAllText({},'WORKSPACE_WRITE'); {attempts} Write-Output GIT_POLICY_ENFORCED\"",quote(ordinary))
    } else {
        let reads = readable
            .iter()
            .map(|path| format!("cat {} > /dev/null || exit 92; ", quote(path)))
            .collect::<String>();
        let attempts = denied
            .iter()
            .map(|path| {
                format!(
                    "if printf ESCAPE > {}; then printf METADATA_WRITE_ESCAPED; exit 91; fi; ",
                    quote(path)
                )
            })
            .collect::<String>();
        format!(
            "{reads} printf WORKSPACE_WRITE > {} || exit 90; {attempts} printf GIT_POLICY_ENFORCED",
            quote(ordinary)
        )
    }
}
