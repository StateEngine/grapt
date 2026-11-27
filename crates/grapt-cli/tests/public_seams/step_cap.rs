use super::*;
use grapt_cli::{CliCommand, SessionSelector};

#[tokio::test]
async fn max_steps_is_validated_before_connect_and_reaches_background_admission() -> TestResult {
    for value in ["0", "-1", "1.5", "4294967296", "private-invalid"] {
        assert!(CliCommand::parse([
            "run",
            "--service",
            "absent",
            "--max-steps",
            value,
            "--stdin"
        ])
        .is_err());
    }
    assert!(CliCommand::parse(["attach", "--service", "absent", "--max-steps", "1"]).is_err());
    assert!(CliCommand::parse([
        "run",
        "--service",
        "absent",
        "--max-steps",
        "1",
        "--resident",
        "--jsonl",
        "go"
    ])
    .is_err());
    let CliCommand::Run { max_steps, .. } =
        CliCommand::parse(["run", "--service", "r", "--max-steps", "2", "go"])?
    else {
        return Err("expected run".into());
    };
    let accepted = r#"{"data":{"admittedSeq":1,"id":"msg_user","sessionID":"ses_a","timeCreated":1.0,"type":"user","data":{"text":"go"},"delivery":"queue"}}"#;
    let (endpoint, server) = spawn_server(vec![Script::json(
        "POST",
        "/api/session/ses_a/prompt",
        200,
        accepted,
    )
    .expect_json(serde_json::json!({"text":"go","metadata":{"maxSteps":2}}))])
    .await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Json,
        CliLimits::production(),
    );
    Runner::new(client(&endpoint)?)
        .with_max_steps(max_steps)
        .start_background(
            SessionSelector::Id {
                session: "ses_a".parse()?,
                project: None,
            },
            "go".into(),
            &mut output,
        )
        .await?;
    server.await??;
    Ok(())
}
