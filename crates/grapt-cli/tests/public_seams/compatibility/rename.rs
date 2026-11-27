//! CLI-COMPATIBILITY-MATRIX-01 witnesses: `run --title` names the run's
//! Session before the prompt (CCM-RUN-09).

use super::*;

#[test]
fn session_rename_validates_the_exact_target_and_title_without_service_work() -> TestResult {
    for tail in [&[][..], &["--service", "profile"][..]] {
        let args = [&["session", "rename", "ses_a", "  My title  "][..], tail].concat();
        let CliCommand::SessionRename {
            connection,
            session,
            title,
        } = parse(&args)?
        else {
            return Err("not a Session rename".into());
        };
        assert_eq!(session.as_str(), "ses_a");
        assert_eq!(title, "My title");
        assert_eq!(connection.is_some(), !tail.is_empty());
    }
    let overlong = "a".repeat(101);
    for title in ["", "   ", "a\nb", overlong.as_str()] {
        assert_eq!(
            parse(&["session", "rename", "ses_a", title]),
            Err(CommandError::Title)
        );
    }
    let exact = "界".repeat(100);
    assert!(parse(&["session", "rename", "ses_a", &exact]).is_ok());
    for args in [
        &["session", "rename", "ses_a"][..],
        &["session", "rename", "ses_a", "title", "extra"],
        &[
            "session",
            "rename",
            "ses_a",
            "title",
            "--embedded",
            "--service",
            "r",
            "--product",
            "p",
        ],
        &["session", "rename", "ses_a", "title", "--stdin"],
        &["session", "rename", "ses_a", "title", "--background"],
    ] {
        assert_eq!(parse(args), Err(CommandError::RenameArguments));
    }
    assert!(matches!(
        parse(&["session", "rename", "--help"])?,
        CliCommand::FocusedHelp(_)
    ));
    Ok(())
}

#[tokio::test]
async fn binary_session_rename_only_writes_the_addressed_title_and_reports_refusals() -> TestResult
{
    for status in [204, 404] {
        let root = Scratch::new("session-rename")?;
        let profile = root.path().join("grapt").join("default");
        std::fs::create_dir_all(&profile)?;
        let pid = std::process::id();
        let build = "session-rename-test";
        let health = format!(
            r#"{{"schema":"secode.api.health.v1","pid":{pid},"build":"{build}","environment":"test","service_instance":"instance-live"}}"#
        );
        let (endpoint, server) = spawn_server(vec![
            Script::json("GET", "/api/health", 200, &health),
            Script::status("POST", "/api/session/ses_a/rename", status)
                .expect_json(serde_json::json!({"title":"My title"})),
        ])
        .await?;
        write_discovery_fixture(&profile, pid, &endpoint, build)?;
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"));
        command
            .args(["session", "rename", "ses_a", "  My title  "])
            .env("XDG_CONFIG_HOME", root.path())
            .env("APPDATA", root.path())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if status == 204 {
            command.arg("--service").arg(&profile);
        }
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(10), command.output()).await??;
        assert_eq!(output.status.success(), status == 204, "{output:?}");
        if status != 204 {
            assert!(output.stdout.is_empty());
            assert!(!output.stderr.is_empty());
        }
        assert_eq!(
            server.await??,
            ["GET /api/health", "POST /api/session/ses_a/rename"]
        );
    }
    Ok(())
}

/// `--title` selects the run's Session name: trimmed, admitted once, and
/// admitted by `run` only.
#[test]
fn run_title_names_the_run_session_and_attach_does_not_take_it() -> TestResult {
    for (raw, expected) in [("My title", "My title"), ("  Padded  ", "Padded")] {
        let command = parse(&["run", "--service", "r", "--title", raw, "go"])?;
        assert!(
            matches!(&command, CliCommand::Run { title, .. } if title.as_deref() == Some(expected)),
            "{raw:?}"
        );
    }
    let refused = parse(&[
        "run",
        "--service",
        "r",
        "--title",
        "t",
        "--title",
        "u",
        "go",
    ]);
    assert_eq!(refused, Err(CommandError::Duplicate("--title")));
    let refused = parse(&["attach", "--service", "r", "-s", "ses_a", "--title", "t"]);
    assert!(
        matches!(refused, Err(CommandError::UnknownOption(_))),
        "{refused:?}"
    );
    Ok(())
}

/// The Session store's title rules, checked before any service work: trimmed,
/// nonempty, no control characters, at most 100 characters.
#[test]
fn run_title_refuses_empty_overlong_and_control_names() -> TestResult {
    let overlong = "a".repeat(101);
    for raw in ["", "   ", "a\nb", overlong.as_str()] {
        let refused = parse(&["run", "--service", "r", "--title", raw, "go"]);
        assert_eq!(refused, Err(CommandError::Title), "{raw:?}");
    }
    let exact = "b".repeat(100);
    let command = parse(&["run", "--service", "r", "--title", exact.as_str(), "go"])?;
    assert!(
        matches!(&command, CliCommand::Run { title, .. } if title.as_deref() == Some(exact.as_str()))
    );
    Ok(())
}

/// CCM-RUN-09: `--title` renames the run's Session through the store's route
/// before the prompt is admitted.
#[tokio::test]
async fn run_title_renames_the_session_before_the_prompt() -> TestResult {
    let scripts = vec![
        Script::status("POST", "/api/session/ses_a/rename", 204)
            .expect_json(serde_json::json!({"title": "My title"})),
        Script::json("POST", "/api/session/ses_a/prompt", 200, PROMPT_ACCEPTED),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Json,
        CliLimits::production(),
    );
    let receipt = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        Runner::new(client(&endpoint)?)
            .with_title(Some("My title".to_owned()))
            .start_background(
                SessionSelector::Id {
                    session: "ses_a".parse()?,
                    project: None,
                },
                "go".to_owned(),
                &mut output,
            ),
    )
    .await??;
    let requests = server.await??;
    assert_eq!(
        receipt.session.to_string(),
        "ses_a",
        "the prompt went to the wrong Session: {requests:?}"
    );
    assert_eq!(
        requests.as_slice(),
        [
            "POST /api/session/ses_a/rename",
            "POST /api/session/ses_a/prompt"
        ],
        "the rename must carry the title and precede the prompt"
    );
    Ok(())
}
