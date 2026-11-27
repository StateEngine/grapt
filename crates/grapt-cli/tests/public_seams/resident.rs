use super::*;
use grapt_cli::SessionSelector;

#[tokio::test]
async fn binary_keyed_initial_stdin_reaches_prompt_json_without_replaying_uncertain_acceptance(
) -> TestResult {
    use grapt_cli::{CliCommand, CommandError};
    let base: Vec<_> = "run --service profile --stdin --input-id msg_KEY"
        .split_whitespace()
        .collect();
    for selector in [
        &[][..],
        &["--new"],
        &["--continue"],
        &["--session", "ses_a", "--fork"],
    ] {
        assert_eq!(
            CliCommand::parse([&base[..], selector].concat()),
            Err(CommandError::InputIdTarget)
        );
    }
    let root = Scratch::new("keyed-input")?;
    let pid = std::process::id();
    let build = "keyed-input-test";
    let health = format!(
        r#"{{"schema":"secode.api.health.v1","pid":{pid},"build":"{build}","environment":"test","service_instance":"instance-live"}}"#
    );
    let (endpoint, server) = spawn_server(vec![
        Script::json("GET", "/api/health", 200, &health),
        Script::sse("GET", "/api/event", ""),
        // An invalid HTTP status prevents a complete response after input was sent.
        Script::status("POST", "/api/session/ses_a/prompt", 0).expect_json(serde_json::json!({
            "id":"msg_KEY","text":"hello","delivery":"queue",
            "metadata":{"origin":"seat-startup"}
        })),
    ])
    .await?;
    write_discovery_fixture(root.path(), pid, &endpoint, build)?;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
        .args(["run", "--service"])
        .arg(root.path())
        .args("--session ses_a --stdin --jsonl --resident --input-id msg_KEY".split_whitespace())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().ok_or("stdin pipe absent")?;
    input.write_all(b"hello").await?;
    drop(input);
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output())
        .await??;
    assert_eq!(
        output.status.code(),
        Some(i32::from(ExitClass::Remote.code()))
    );
    assert!(String::from_utf8(output.stderr)?.contains("request transport failed"));
    let requests = server.await??;
    assert_eq!(
        requests.iter().filter(|r| r.ends_with("/prompt")).count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn a_resident_with_closed_stdout_keeps_its_approver_for_later_work() -> TestResult {
    let accepted = r#"{"data":{"admittedSeq":1,"id":"msg_user","sessionID":"ses_a","timeCreated":1.0,"type":"user","data":{"text":"hello"},"delivery":"queue"}}"#;
    let initial = r#"{"data":[{"type":"user","id":"msg_user","time":{"created":0.5},"text":"hello"}],"cursor":{}}"#;
    let later = r#"{"data":[{"type":"user","id":"msg_mail","time":{"created":2.0},"text":"later work"}],"cursor":{}}"#;
    let permissions = r#"{"data":[],"hasMore":false}"#;
    let pending = r#"{"data":[{"id":"per_1","sessionID":"ses_a","action":"glob","resources":["*"],"status":"pending","source":{"type":"tool","messageID":"msg_mail","callID":"call_1"},"metadata":{"policy":{"ownerFloor":false,"removesFiles":false}}}],"hasMore":false}"#;
    let messages = "/api/session/ses_a/message";
    let permission = "/api/session/ses_a/permission";
    let (endpoint, server) = spawn_server(vec![
        Script::sse_chunks_open("/api/event", vec![
            "event: message\ndata: {\"type\":\"server.connected\",\"id\":\"evt_connected\",\"data\":{}}\n\n".into(),
        ]),
        Script::json("POST", "/api/session/ses_a/prompt", 200, accepted)
            .expect_json(serde_json::json!({
                "id":"msg_user","text":"hello","delivery":"queue",
                "metadata":{"origin":"seat-startup"}
            })),
        Script::json("GET", messages, 200, initial),
        Script::json("GET", permission, 200, permissions),
        Script::json("GET", permission, 200, permissions),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json("GET", messages, 200, initial),
        Script::json("GET", messages, 200, later),
        Script::json("GET", permission, 200, pending).require_live_sse(),
        Script::status("POST", "/api/session/ses_a/permission/per_1/reply", 204)
            .expect_json(serde_json::json!({"reply":"once"})),
        Script::status("GET", messages, 410),
    ])
    .await?;
    // A real pipe with no reader fails with BrokenPipe on both platforms.
    let (reader, writer) = std::io::pipe()?;
    drop(reader);
    let mut output = BoundedOutput::new(
        writer,
        Vec::new(),
        OutputMode::Jsonl,
        CliLimits::production(),
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Runner::new(client(&endpoint)?)
            .with_input_id(Some("msg_user".parse()?))
            .with_resident(true)
            .with_auto(grapt_cli::AutoReply::Auto)
            .run_prompt(
                SessionSelector::Id {
                    session: "ses_a".parse()?,
                    project: None,
                },
                "hello".into(),
                &mut output,
            ),
    )
    .await;
    if !matches!(result, Ok(Err(RunError::Api(_)))) {
        server.abort();
        let _joined = server.await;
        return Err(
            format!("closed stdout ended the resident before later work: {result:?}").into(),
        );
    }
    let requests = server.await??;
    assert!(requests
        .iter()
        .any(|request| request == "POST /api/session/ses_a/permission/per_1/reply"));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.ends_with("/prompt"))
            .count(),
        1,
        "stdout loss readmitted the initial prompt"
    );
    Ok(())
}

#[tokio::test]
async fn resident_keeps_approver_for_later_work_without_readmitting_and_reports_service_loss(
) -> TestResult {
    let accepted = r#"{"data":{"admittedSeq":1,"id":"msg_user","sessionID":"ses_a","timeCreated":1.0,"type":"user","data":{"text":"hello"},"delivery":"queue"}}"#;
    let initial = r#"{"data":[{"type":"user","id":"msg_user","time":{"created":0.5},"text":"hello"}],"cursor":{}}"#;
    let later = r#"{"data":[{"type":"assistant","id":"msg_mail","time":{"created":2.0,"completed":2.0},"agent":"agt_live_headless1","model":{"id":"gpt-5.6","providerID":"openai"},"content":[{"type":"text","text":"mail answer","state":{"durableBytes":11,"durableCapture":"complete","publicBoundBytes":262144,"publicProjection":"complete"}}],"finish":"stop","cost":0.0,"tokens":{"input":1.0,"output":1.0,"reasoning":0.0,"cache":{"read":0.0,"write":0.0}},"observedUsage":{"inputTotal":1,"inputFresh":1,"output":1,"reasoning":0,"cacheRead":0,"cacheWrite":0,"total":2}}],"cursor":{}}"#;
    let permissions = r#"{"data":[],"hasMore":false}"#;
    let pending = r#"{"data":[{"id":"per_1","sessionID":"ses_a","action":"glob","resources":["*"],"status":"pending","source":{"type":"tool","messageID":"msg_mail","callID":"call_1"},"metadata":{"policy":{"ownerFloor":false,"removesFiles":false}}}],"hasMore":false}"#;
    let messages = "/api/session/ses_a/message";
    let permission = "/api/session/ses_a/permission";
    let (endpoint, server) = spawn_server(vec![
        Script::sse_chunks_open("/api/event", vec![
            "event: message\ndata: {\"type\":\"server.connected\",\"id\":\"evt_connected\",\"data\":{}}\n\n".into(),
        ]),
        Script::json("POST", "/api/session/ses_a/prompt", 200, accepted),
        Script::json("GET", messages, 200, initial),
        Script::json("GET", permission, 200, permissions),
        Script::json("GET", permission, 200, permissions),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json("GET", messages, 200, initial),
        Script::json("GET", messages, 200, later),
        Script::json("GET", permission, 200, pending).require_live_sse(),
        Script::status("POST", "/api/session/ses_a/permission/per_1/reply", 204)
            .expect_json(serde_json::json!({"reply":"once"})),
        Script::status("GET", messages, 410),
    ])
    .await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Jsonl,
        CliLimits::production(),
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Runner::new(client(&endpoint)?)
            .with_resident(true)
            .with_auto(grapt_cli::AutoReply::Auto)
            .run_prompt(
                SessionSelector::Id {
                    session: "ses_a".parse()?,
                    project: None,
                },
                "hello".into(),
                &mut output,
            ),
    )
    .await?;
    assert!(
        matches!(result, Err(RunError::Api(_))),
        "idle ended the resident attachment: {result:?}"
    );
    let (stdout, _, _) = output.into_parts();
    assert!(
        String::from_utf8(stdout)?.contains("mail answer"),
        "later answer absent: {result:?}"
    );
    assert_eq!(
        server
            .await??
            .iter()
            .filter(|r| r.ends_with("/prompt"))
            .count(),
        1
    );
    Ok(())
}
