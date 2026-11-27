//! Explicit startup choices reach Session creation before prompt admission.
use super::*;
use grapt_cli::{CliCommand, CommandError, SessionSelector, StartupSelection};

fn selection(arguments: &[&str]) -> TestResult<StartupSelection> {
    match CliCommand::parse(arguments.iter().copied())? {
        CliCommand::Run { selection, .. } => Ok(selection),
        _ => Err("expected run".into()),
    }
}

#[tokio::test]
async fn startup_selection_is_atomic_and_a_catalog_refusal_admits_no_prompt() -> TestResult {
    let chosen = selection(&[
        "run",
        "--service",
        "r",
        "-m",
        "local/owner/model",
        "--agent",
        "reviewer",
        "go",
    ])?;
    let directory = std::fs::canonicalize(std::env::current_dir()?)?;
    let (endpoint, server) = spawn_server(vec![Script::status("POST", "/api/session", 400)
        .expect_json(serde_json::json!({"location":{"directory":directory.to_str().ok_or("UTF-8 directory")?},
            "model":{"providerID":"local","id":"owner/model"},"agent":"reviewer"}))]).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Text,
        CliLimits::production(),
    );
    let result = Runner::new(client(&endpoint)?)
        .with_selection(chosen)
        .run_prompt(SessionSelector::New, "go".into(), &mut output)
        .await;
    assert!(matches!(
        result,
        Err(RunError::Api(grapt_cli::ApiClientError::Status(_)))
    ));
    assert_eq!(server.await??, ["POST /api/session"]);
    Ok(())
}

#[test]
fn explicit_choices_are_new_session_only_and_bad_values_fail_before_connection() -> TestResult {
    for target in [
        &["--session", "ses_a"][..],
        &["--continue"][..],
        &["--continue", "--fork"][..],
    ] {
        let argv = [
            &["run", "--service", "absent", "--agent", "reviewer"][..],
            target,
            &["--stdin"][..],
        ]
        .concat();
        assert_eq!(CliCommand::parse(argv), Err(CommandError::SelectionTarget));
    }
    for flags in [
        vec!["--model", "missing-provider"],
        vec!["--model", "/m"],
        vec!["--agent", "bad\nagent"],
        vec!["--model", "p/m", "-m", "p/other"],
        vec!["--agent", "a", "--agent", "b"],
    ] {
        let argv = [&["run", "--service", "absent"][..], &flags, &["go"][..]].concat();
        assert!(CliCommand::parse(argv).is_err());
    }
    assert!(CliCommand::parse(["attach", "--service", "r", "--new", "--model", "p/m"]).is_err());
    let argv = ["--model", "p/m", "--agent", "reviewer"].map(str::to_owned);
    let terminal = grapt_cli::command::terminal::TerminalCommand::parse(
        &argv,
        std::env::current_dir()?,
        Some(PathBuf::from("/profile")),
    )?;
    assert_eq!(
        terminal.selection,
        selection(&[
            "run",
            "--service",
            "r",
            "--model",
            "p/m",
            "--agent",
            "reviewer",
            "go"
        ])?
    );
    Ok(())
}

#[tokio::test]
async fn selected_and_default_session_creation_preserve_other_defaults() -> TestResult {
    let body = r#"{"data":{"id":"ses_a","projectID":"prj_a","cost":0.0,"tokens":{"input":0.0,"output":0.0,"reasoning":0.0,"cache":{"read":0.0,"write":0.0}},"time":{"created":1.0,"updated":1.0},"title":"t","location":{"directory":"/w"}}}"#;
    let (endpoint, server) = spawn_server(vec![
        Script::json("POST", "/api/session", 200, body).expect_json(
            serde_json::json!({"location":{"directory":"/w"},"model":{"providerID":"p","id":"m"}}),
        ),
        Script::json("POST", "/api/session", 200, body)
            .expect_json(serde_json::json!({"location":{"directory":"/w"}})),
    ])
    .await?;
    let client = client(&endpoint)?;
    let model = selection(&["run", "--service", "r", "--model", "p/m", "go"])?;
    client
        .create_session_at_selected(grapt_api::u3a::DtoString::try_new("/w")?, &model)
        .await?;
    client
        .create_session_at(grapt_api::u3a::DtoString::try_new("/w")?)
        .await?;
    server.await??;
    Ok(())
}
