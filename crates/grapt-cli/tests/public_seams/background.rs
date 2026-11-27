use super::*;
use grapt_cli::{CliCommand, CommandError, SessionSelector};

const ACCEPTED: &str = r#"{"data":{"admittedSeq":1,"id":"msg_user","sessionID":"ses_a","timeCreated":1.0,"type":"user","data":{"text":"hello"},"delivery":"queue"}}"#;

#[test]
fn background_and_worktree_flags_validate_lifetime_and_placement() -> TestResult {
    let parse = |args: &[&str]| CliCommand::parse(args.iter().copied());
    assert!(matches!(
        parse(&[
            "run",
            "--service",
            "r",
            "--background",
            "--worktree",
            "../task",
            "hello"
        ])?,
        CliCommand::Run {
            background: true,
            worktree: Some(_),
            selector: SessionSelector::New,
            ..
        }
    ));
    for extra in [
        &["-s", "ses_a"][..],
        &["-c"],
        &["--remote", "https://example.invalid", "--credential", "key"],
    ] {
        let args = [
            &["run", "--service", "r", "--worktree", "../task", "hello"][..],
            extra,
        ]
        .concat();
        assert_eq!(parse(&args), Err(CommandError::WorktreeTarget));
    }
    for extra in [
        &["--auto"][..],
        &["--dangerously-skip-permissions"],
        &["--embedded", "--product", "binary"],
    ] {
        let args = [
            &["run", "--service", "r", "--background", "hello"][..],
            extra,
        ]
        .concat();
        assert_eq!(parse(&args), Err(CommandError::BackgroundTarget));
    }
    for flag in ["--background", "--worktree"] {
        assert!(parse(&["attach", "--service", "r", "--new", flag, "task"]).is_err());
    }
    Ok(())
}

#[tokio::test]
async fn background_returns_acceptance_without_subscribing_polling_or_interrupting() -> TestResult {
    for queued in [None, Some("provider unavailable")] {
        let mut accepted: serde_json::Value = serde_json::from_str(ACCEPTED)?;
        if let Some(reason) = queued {
            accepted["queued"] = serde_json::json!({"reason":reason});
        }
        let scripts = vec![Script::json(
            "POST",
            "/api/session/ses_a/prompt",
            200,
            &accepted.to_string(),
        )
        .expect_json(serde_json::json!({"text":"hello"}))];
        let (endpoint, server) = spawn_server(scripts).await?;
        let mut output = BoundedOutput::new(
            Vec::new(),
            Vec::new(),
            OutputMode::Json,
            CliLimits::production(),
        );
        let runner = Runner::new(client(&endpoint)?);
        let receipt = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            runner.start_background(
                SessionSelector::Id {
                    session: "ses_a".parse()?,
                    project: None,
                },
                "hello".into(),
                &mut output,
            ),
        )
        .await??;
        assert_eq!(receipt.session.as_str(), "ses_a");
        assert_eq!(receipt.message.as_str(), "msg_user");
        assert_eq!(receipt.queued.as_deref(), queued);
        assert_eq!(server.await??, ["POST /api/session/ses_a/prompt"]);
        drop(runner);
    }
    Ok(())
}

#[tokio::test]
async fn background_admission_failure_is_visible_and_never_retried() -> TestResult {
    let (endpoint, server) = spawn_server(vec![Script::status(
        "POST",
        "/api/session/ses_a/prompt",
        503,
    )])
    .await?;
    let runner = Runner::new(client(&endpoint)?);
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Text,
        CliLimits::production(),
    );
    assert!(runner
        .start_background(
            SessionSelector::Id {
                session: "ses_a".parse()?,
                project: None
            },
            "hello".into(),
            &mut output,
        )
        .await
        .is_err());
    assert_eq!(server.await??, ["POST /api/session/ses_a/prompt"]);
    Ok(())
}
