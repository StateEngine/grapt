//! CLI-COMPATIBILITY-MATRIX-01: witnesses for the rows of
//! `docs/cli-compatibility/MATRIX.md` that this CLI implements or refuses.

use grapt_cli::{compatibility, CliCommand, CommandError, PromptSource, SessionSelector};

use super::*;

fn parse(arguments: &[&str]) -> Result<CliCommand, CommandError> {
    CliCommand::parse(arguments.iter().copied())
}

fn run_parts(arguments: &[&str]) -> TestResult<(SessionSelector, PromptSource, OutputMode, bool)> {
    match parse(arguments)? {
        CliCommand::Run {
            selector,
            prompt,
            output,
            reasoning,
            ..
        } => Ok((selector, prompt, output, reasoning)),
        other => Err(format!("not a run: {other:?}").into()),
    }
}

fn refused(arguments: &[&str]) -> TestResult<compatibility::Refusal> {
    match parse(arguments) {
        Err(CommandError::Unsupported(refusal)) => Ok(refusal),
        other => Err(format!("{arguments:?} was not refused by name: {other:?}").into()),
    }
}

/// Every refusal the CLI carries is reachable from the oracle's argv that
/// named it, and renders the stable sentence the matrix cites.
#[test]
fn every_typescript_surface_outside_the_contract_is_refused_by_name() -> TestResult {
    let cases: &[(&[&str], &str)] = &[
        (&["api", "GET", "/api/session"], "api"),
        (&["auth", "connect", "https://example.test"], "auth"),
        (&["console", "login"], "console"),
        (&["debug", "agents"], "debug"),
        (&["mcp", "serve"], "mcp"),
        (&["migrate"], "migrate"),
        (&["mini"], "mini"),
        (&["pair"], "pair"),
        (&["plugin", "list"], "plugin"),
        (&["serve", "--port", "4096"], "serve"),
        (&["upgrade"], "upgrade"),
        (&["service", "get"], "service get"),
        (&["service", "restart"], "service restart"),
        (&["service", "set", "key", "value"], "service set"),
        (&["service", "stop"], "service stop"),
        (&["service", "unset", "key"], "service unset"),
        (&["-c"], "grapt (interactive)"),
        (&["--standalone"], "grapt (interactive)"),
        (&["run", "--service", "r", "-f", "a.txt", "hi"], "--file"),
        (
            &["run", "--service", "r", "--file", "a.txt", "hi"],
            "--file",
        ),
        (&["run", "--standalone", "hi"], "--standalone"),
        (
            &["run", "--server", "http://127.0.0.1:4096", "hi"],
            "--server",
        ),
        (
            &["run", "--service", "r", "--format", "json", "hi"],
            "--format json",
        ),
    ];
    let mut covered = std::collections::BTreeSet::new();
    for (arguments, surface) in cases {
        let refusal = refused(arguments)?;
        assert_eq!(refusal.surface, *surface, "{arguments:?}");
        let rendered = CommandError::Unsupported(refusal).to_string();
        assert_eq!(
            rendered,
            format!(
                "`{}` is not part of the Grapt CLI contract: {}",
                refusal.surface, refusal.reason
            )
        );
        covered.insert(refusal.surface);
    }
    let carried = compatibility::all()
        .map(|refusal| refusal.surface)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(covered, carried, "a carried refusal has no witness");
    Ok(())
}

#[test]
fn run_takes_the_typescript_message_words_and_defaults_to_a_new_session() -> TestResult {
    let (selector, prompt, output, reasoning) =
        run_parts(&["run", "--service", "r", "fix", "the", "bug"])?;
    assert_eq!(selector, SessionSelector::New);
    assert_eq!(prompt, PromptSource::Argument("fix the bug".to_owned()));
    assert_eq!(output, OutputMode::Text);
    assert!(!reasoning);

    // A word with a space is quoted, with its quotes escaped, as the oracle did.
    let (_, prompt, _, _) = run_parts(&["run", "--service", "r", "say", "a \"b\" c"])?;
    assert_eq!(
        prompt,
        PromptSource::Argument("say \"a \\\"b\\\" c\"".to_owned())
    );

    // Everything after `--` is message, including dashes.
    let (_, prompt, _, _) = run_parts(&["run", "--service", "r", "--", "--literal", "-x"])?;
    assert_eq!(prompt, PromptSource::Argument("--literal -x".to_owned()));

    // Exactly one prompt source.
    assert!(matches!(
        parse(&["run", "--service", "r", "--prompt", "a", "b"]),
        Err(CommandError::PromptSource)
    ));
    assert!(matches!(
        parse(&["run", "--service", "r"]),
        Err(CommandError::PromptSource)
    ));
    Ok(())
}

#[test]
fn run_takes_the_typescript_resume_forms() -> TestResult {
    let resume = SessionSelector::ResumeLast { project: None };
    for flag in ["-c", "--continue", "--resume"] {
        let (selector, _, _, _) = run_parts(&["run", "--service", "r", flag, "go"])?;
        assert_eq!(selector, resume, "{flag}");
    }
    let session = SessionSelector::Id {
        session: "ses_a".parse()?,
        project: None,
    };
    for flag in ["-s", "--session"] {
        let (selector, _, _, _) = run_parts(&["run", "--service", "r", flag, "ses_a", "go"])?;
        assert_eq!(selector, session, "{flag}");
    }
    assert!(parse(&["attach", "--service", "r", "--resume", "--thinking"]).is_err());
    Ok(())
}

#[test]
fn run_takes_the_typescript_default_format_and_thinking() -> TestResult {
    let (_, _, output, reasoning) = run_parts(&[
        "run",
        "--service",
        "r",
        "--format",
        "default",
        "--thinking",
        "go",
    ])?;
    assert_eq!(output, OutputMode::Text);
    assert!(reasoning);
    assert!(matches!(
        parse(&[
            "run",
            "--service",
            "r",
            "--format",
            "default",
            "--jsonl",
            "go"
        ]),
        Err(CommandError::Duplicate("output mode"))
    ));
    assert!(matches!(
        parse(&["run", "--service", "r", "--format", "yaml", "go"]),
        Err(CommandError::Format(_))
    ));
    Ok(())
}

fn grapt(arguments: &[&str]) -> TestResult<std::process::Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_grapt"))
        .args(arguments)
        .stdin(Stdio::null())
        .output()?)
}

#[test]
fn the_binary_answers_version_and_help() -> TestResult {
    let version = grapt(&["--version"])?;
    assert_eq!(version.status.code(), Some(0));
    // A test build has no release manifest beside it, so it says so rather
    // than presenting the crate version as its identity.
    assert_eq!(
        String::from_utf8(version.stdout)?,
        concat!(
            "grapt ",
            env!("CARGO_PKG_VERSION"),
            "\nbuild unattested (no release-manifest.json beside this binary)\n"
        )
    );
    for flag in ["--help", "-h", "help"] {
        let help = grapt(&[flag])?;
        assert_eq!(help.status.code(), Some(0), "{flag}");
        let text = String::from_utf8(help.stdout)?;
        assert!(text.starts_with("usage: grapt <command>"), "{flag}: {text}");
        assert!(text.contains("service status"), "{flag}: {text}");
        assert!(text.contains("\n  code    "), "{flag}: {text}");
    }
    Ok(())
}

#[test]
fn recovery_directions_name_the_shipped_terminal_and_update_commands() -> TestResult {
    let terminal = "start the native terminal with grapt or grapt code";
    let cases: &[(&[&str], &str, &str)] = &[
        (&["mini"], "mini", terminal),
        (&["-c"], "grapt (interactive)", terminal),
        (&["--standalone"], "grapt (interactive)", terminal),
        (
            &["upgrade"],
            "upgrade",
            "install a verified local package with grapt update --from <package-directory>",
        ),
    ];
    for (arguments, surface, reason) in cases {
        let refusal = refused(arguments)?;
        assert_eq!(refusal.surface, *surface);
        assert_eq!(refusal.reason, *reason);
        let output = grapt(arguments)?;
        assert_eq!(
            output.status.code(),
            Some(i32::from(ExitClass::Usage.code()))
        );
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr)?,
            format!("grapt: `{surface}` is not part of the Grapt CLI contract: {reason}\n")
        );
    }

    // The public binary handles empty argv before this noninteractive grammar.
    let missing = match parse(&[]) {
        Err(error) => error,
        Ok(command) => return Err(format!("empty argv parsed as {command:?}").into()),
    };
    assert_eq!(missing, CommandError::MissingCommand);
    assert_eq!(
        missing.to_string(),
        "a command is required: status, launch, attach, run, service, session, mcp, import, or acp (start the native terminal with grapt or grapt code; see grapt --help)"
    );
    Ok(())
}

#[test]
fn the_binary_refuses_a_typescript_command_with_its_reason_and_exit_usage() -> TestResult {
    let output = grapt(&["mcp", "serve"])?;
    assert_eq!(
        output.status.code(),
        Some(i32::from(ExitClass::Usage.code()))
    );
    assert!(output.stdout.is_empty());
    let refusal = compatibility::command("mcp", None).ok_or("mcp refusal absent")?;
    assert_eq!(
        String::from_utf8(output.stderr)?,
        format!(
            "grapt: `mcp` is not part of the Grapt CLI contract: {}\n",
            refusal.reason
        )
    );
    Ok(())
}

const PROMPT_ACCEPTED: &str = r#"{"data":{"admittedSeq":2,"id":"msg_user2","sessionID":"ses_a","timeCreated":3.0,"type":"user","data":{"text":"go"},"delivery":"queue"}}"#;

fn assistant(id: &str, text: &str, reasoning: &str, completed: bool) -> String {
    let completed = if completed { r#","completed":2.0"# } else { "" };
    format!(
        r#"{{"type":"assistant","id":"{id}","time":{{"created":1.0{completed}}},"agent":"build","model":{{"id":"test-model","providerID":"vendor"}},"content":[{{"type":"reasoning","text":"{reasoning}"}},{{"type":"text","text":"{text}"}}]}}"#
    )
}

fn page(rows: &[String]) -> String {
    format!(r#"{{"data":[{}],"cursor":{{}}}}"#, rows.join(","))
}

/// The /proc state char of `pid`, or None once it is reaped.
#[cfg(target_os = "linux")]
fn pid_state_of(pid: u32) -> TestResult<Option<char>> {
    pid_state_from(pid, std::fs::read_to_string(format!("/proc/{pid}/stat")))
}

/// Only an absent process is None. Any other read failure, or stat text
/// without a state field, is an error: unreadable evidence is not a reap.
#[cfg(target_os = "linux")]
fn pid_state_from(pid: u32, read: std::io::Result<String>) -> TestResult<Option<char>> {
    // ESRCH: the process went away between opening its stat file and reading it.
    const ESRCH: i32 = 3;
    let stat = match read {
        Ok(stat) => stat,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(ESRCH) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(format!("/proc/{pid}/stat unreadable: {error}").into()),
    };
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.strip_prefix(' '))
        .and_then(|rest| rest.chars().next())
        .filter(char::is_ascii_alphabetic);
    match state {
        Some(state) => Ok(Some(state)),
        None => Err(format!("/proc/{pid}/stat has no state field: {stat:?}").into()),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn pid_state_evidence_is_absent_only_when_the_process_is_gone() -> TestResult {
    use std::io::{Error, ErrorKind};
    assert_eq!(
        pid_state_from(7, Ok("7 (sleep) S 1 7 7 0".to_owned()))?,
        Some('S')
    );
    assert_eq!(
        pid_state_from(7, Ok("7 (a) b) Z 1".to_owned()))?,
        Some('Z'),
        "the state follows the LAST ')' of the command name"
    );
    assert_eq!(pid_state_from(7, Err(ErrorKind::NotFound.into()))?, None);
    assert_eq!(pid_state_from(7, Err(Error::from_raw_os_error(3)))?, None);
    assert!(
        pid_state_from(7, Err(ErrorKind::PermissionDenied.into())).is_err(),
        "unreadable evidence must not read as reaped"
    );
    for malformed in ["", "7 (sleep)", "7 (sleep) ", "7 sleep S", "7 (sleep) 9"] {
        assert!(
            pid_state_from(7, Ok(malformed.to_owned())).is_err(),
            "malformed stat {malformed:?} must not read as reaped"
        );
    }
    let own = pid_state_of(std::process::id())?;
    assert!(
        matches!(own, Some('R' | 'S')),
        "this test process is live: {own:?}"
    );
    Ok(())
}

/// The oracle's permission flags: one preset flag per trust level, the
/// strictest of several winning, and the client-side auto-reply.
#[test]
fn permission_flags_choose_the_strictest_preset_and_the_auto_reply() -> TestResult {
    let chosen = |arguments: &[&str]| -> TestResult<(Option<&'static str>, grapt_cli::AutoReply)> {
        match parse(arguments)? {
            CliCommand::Run { preset, auto, .. } => Ok((preset, auto)),
            other => Err(format!("not a run: {other:?}").into()),
        }
    };
    use grapt_cli::AutoReply;
    let run = |flags: &[&'static str]| {
        let mut arguments = vec!["run", "--service", "r"];
        arguments.extend_from_slice(flags);
        arguments.push("hi");
        arguments
    };
    assert_eq!(chosen(&run(&[]))?, (None, AutoReply::Off));
    assert_eq!(
        chosen(&run(&["--author"]))?,
        (Some("author"), AutoReply::Off)
    );
    assert_eq!(
        chosen(&run(&["--maintainer", "--inspect"]))?,
        (Some("inspect"), AutoReply::Off),
        "a careless pair granted more than the stricter flag"
    );
    assert_eq!(chosen(&run(&["--auto"]))?.1, AutoReply::Auto);
    assert_eq!(chosen(&run(&["--yolo"]))?.1, AutoReply::Auto);
    assert_eq!(
        chosen(&run(&["--auto", "--dangerously-skip-permissions"]))?.1,
        AutoReply::Everything
    );
    // Run-only: a preset is a trust level for a turn the CLI starts.
    assert!(parse(&["attach", "--service", "r", "--inspect"]).is_err());
    Ok(())
}

#[cfg(unix)]
#[path = "compatibility/argv.rs"]
mod argv;
#[path = "compatibility/launch.rs"]
mod launch;
#[cfg(target_os = "linux")]
#[path = "compatibility/launch_window.rs"]
mod launch_window;
#[cfg(target_os = "linux")]
#[path = "compatibility/launch_window_real.rs"]
mod launch_window_real;
#[cfg(target_os = "linux")]
#[path = "compatibility/output_pipe.rs"]
mod output_pipe;
#[path = "compatibility/rename.rs"]
mod rename;
#[path = "compatibility/render.rs"]
mod render;
#[path = "compatibility/resume_fork.rs"]
mod resume_fork;
#[path = "compatibility/status.rs"]
mod status;

#[test]
fn reasoning_effort_is_a_run_only_selection_and_not_the_thinking_display_flag() -> TestResult {
    let command = parse(&[
        "run",
        "--service",
        "/profile",
        "--reasoning-effort",
        "high",
        "task",
    ])?;
    assert!(
        matches!(command, CliCommand::Run { reasoning_effort: Some(ref effort), reasoning: false, .. } if effort == "high")
    );
    assert!(parse(&[
        "attach",
        "--service",
        "/profile",
        "--reasoning-effort",
        "high"
    ])
    .is_err());
    assert!(parse(&[
        "run",
        "--service",
        "/profile",
        "--reasoning-effort",
        "high",
        "--reasoning-effort",
        "low",
        "task"
    ])
    .is_err());
    Ok(())
}

#[tokio::test]
async fn mcp_list_reports_selected_catalog_without_configuration_or_terminal_controls() -> TestResult
{
    for (json_output, status, empty) in [
        (true, 200, false),
        (false, 200, false),
        (false, 200, true),
        (false, 503, false),
    ] {
        let root = Scratch::new("mcp-list")?;
        let pid = std::process::id();
        let build = "grapt-cli-public-test";
        let health = format!(
            r#"{{"schema":"secode.api.health.v1","pid":{pid},"build":"{build}","environment":"test","service_instance":"instance-live"}}"#
        );
        let data = if empty {
            "[]"
        } else {
            r#"[{"name":"docs","status":{"status":"failed","error":"server \u001b[31mfailed"}},{"name":"local","status":{"status":"disabled"}}]"#
        };
        let body = format!(
            r#"{{"data":{data},"location":{{"directory":"/repo with space","project":{{"id":"prj_a","directory":"/repo with space"}}}}}}"#
        );
        let (endpoint, server) = spawn_server(vec![
            Script::json("GET", "/api/health", 200, &health),
            Script::json(
                "GET",
                "/api/mcp?location%5Bdirectory%5D=%2Frepo+with+space",
                status,
                if status == 200 {
                    &body
                } else {
                    "unavailable \u{1b}[31mserver"
                },
            ),
        ])
        .await?;
        write_discovery_fixture(root.path(), pid, &endpoint, build)?;
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"));
        command
            .args(["mcp", "list", "--service"])
            .arg(root.path())
            .args(["--directory", "/repo with space"])
            .kill_on_drop(true);
        if json_output {
            command.arg("--json");
        }
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(10), command.output()).await??;
        server.await??;
        if status != 200 {
            assert_ne!(output.status.code(), Some(0));
            assert!(output.stdout.is_empty());
            assert!(!output.stderr.contains(&0x1b));
            continue;
        }
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.contains(&0x1b));
        let text = String::from_utf8(output.stdout)?;
        if json_output {
            let value: serde_json::Value = serde_json::from_str(&text)?;
            assert_eq!(value["data"][0]["name"], "docs");
            assert_eq!(value["location"]["directory"], "/repo with space");
        } else if empty {
            assert!(text.contains("No MCP servers"));
        } else {
            assert!(text.contains("docs") && text.contains("failed") && text.contains("disabled"));
        }
    }
    Ok(())
}

#[tokio::test]
async fn mcp_management_commands_send_explicit_bounded_declarations() -> TestResult {
    for (arguments, expected) in [
        (
            vec!["--env", "MODE=private-test", "--", "peer", "--stdio"],
            serde_json::json!({"action":"add","config":{"type":"local","command":["peer","--stdio"],"environment":{"MODE":"private-test"}}}),
        ),
        (
            vec!["--url", "https://mcp.example.test/mcp"],
            serde_json::json!({"action":"add","config":{"type":"remote","url":"https://mcp.example.test/mcp","oauth":true}}),
        ),
        (vec![], serde_json::json!({"action":"remove"})),
    ] {
        let root = Scratch::new("mcp-management")?;
        let pid = std::process::id();
        let build = "grapt-cli-public-test";
        let health = format!(
            r#"{{"schema":"secode.api.health.v1","pid":{pid},"build":"{build}","environment":"test","service_instance":"instance-live"}}"#
        );
        let (endpoint, server) = spawn_server(vec![
            Script::json("GET", "/api/health", 200, &health),
            Script::status("POST", "/api/mcp/docs/configuration", 204).expect_json(expected),
        ])
        .await?;
        write_discovery_fixture(root.path(), pid, &endpoint, build)?;
        let verb = if arguments.is_empty() {
            "remove"
        } else {
            "add"
        };
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
                .args(["mcp", verb, "docs", "--service"])
                .arg(root.path())
                .args(arguments)
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        server.await??;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-test"));
    }
    for arguments in [
        vec![
            "mcp",
            "add",
            "docs",
            "--service",
            "/profile",
            "--url",
            "https://example.test",
            "--",
            "peer",
        ],
        vec![
            "mcp",
            "add",
            "docs",
            "--service",
            "/profile",
            "--env",
            "MODE=a",
            "--env",
            "MODE=b",
            "--",
            "peer",
        ],
        vec![
            "mcp",
            "add",
            "docs",
            "--service",
            "/profile",
            "--url",
            "file:///secret",
        ],
    ] {
        assert!(parse(&arguments).is_err());
    }
    Ok(())
}
