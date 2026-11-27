//! Legal live SSE fields can combine into an oversized ACP final snapshot.
use super::*;

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn binary_acp_live_tool_final_fits_and_cannot_regress() -> TestResult {
    let root = Scratch::new("acp-live-tool-bound")?;
    let cwd = root.path().to_string_lossy().replace('\\', "/");
    let query = format!("?location%5Bdirectory%5D={}", encode_query_component(&cwd));
    let pid = std::process::id();
    let build = "acp-live-tool-bound";
    let health = json!({"schema":"secode.api.health.v1","pid":pid,"build":build,"environment":"test","service_instance":"instance-live"});
    let session = json!({"data":{"id":"ses_1","title":"ACP","location":{"directory":cwd},"agent":"build","model":{"providerID":"vendor","id":"model","variant":"high"}}});
    let original = "z".repeat(128 * 1024);
    let arguments = json!({"path":"x".repeat(128*1024)});
    let success = json!({"sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1","content":[{"type":"text","text":original}],"metadata":{},"executed":true});
    let stream = sse(vec![
        json!({"type":"server.connected","id":"evt_connected","data":{}}),
        durable(
            "session.input.promoted",
            1,
            1,
            json!({"sessionID":"ses_1","inputID":"inp_1"}),
        ),
        durable(
            "session.step.started",
            2,
            1,
            json!({"sessionID":"ses_1","assistantMessageID":"msg_1"}),
        ),
        durable(
            "session.tool.input.started",
            3,
            1,
            json!({"sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1","name":"read"}),
        ),
        durable(
            "session.tool.called",
            4,
            1,
            json!({"sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1","input":arguments}),
        ),
        durable("session.tool.success", 5, 2, success.clone()),
        durable("session.tool.success", 6, 2, success),
        durable(
            "session.tool.failed",
            7,
            2,
            json!({"sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1","error":{"type":"ToolFailed","message":"late failure"}}),
        ),
        durable(
            "session.execution.succeeded",
            8,
            1,
            json!({"sessionID":"ses_1"}),
        ),
    ]);
    let scripts = vec![
        Script::json("GET", "/api/health", 200, &health.to_string()),
        Script::json(
            "GET",
            format!("/api/model/default{query}"),
            200,
            r#"{"data":{"providerID":"vendor","id":"model","variant":"high"}}"#,
        ),
        Script::json(
            "GET",
            format!("/api/model{query}"),
            200,
            r#"{"data":[{"providerID":"vendor","id":"model","name":"Model","enabled":true,"variants":[{"id":"high"}]}]}"#,
        ),
        Script::json(
            "GET",
            format!("/api/agent{query}"),
            200,
            r#"{"data":[{"id":"build","name":"Build","mode":"primary","hidden":false}]}"#,
        ),
        Script::json("GET", format!("/api/command{query}"), 200, r#"{"data":[]}"#),
        Script::json("GET", format!("/api/skill{query}"), 200, r#"{"data":[]}"#),
        Script::json("POST", "/api/session", 200, &session.to_string()),
        Script::sse("GET", "/api/event", &stream),
        Script::json(
            "POST",
            "/api/session/ses_1/prompt",
            200,
            r#"{"data":{"id":"inp_1"}}"#,
        ),
        Script::json(
            "GET",
            "/api/session/ses_1/message?order=desc&limit=50",
            200,
            r#"{"data":[]}"#,
        ),
        Script::json(
            "GET",
            "/api/session/ses_1/message/msg_1",
            200,
            r#"{"data":{"finish":"stop"}}"#,
        ),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    write_discovery_fixture(root.path(), pid, &endpoint, build)?;
    let mut child = tokio::process::Command::new(binary())
        .args(["acp", "--service"])
        .arg(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().ok_or("ACP stdin missing")?;
    let mut output = BufReader::new(child.stdout.take().ok_or("ACP stdout missing")?);
    for (id, method, params) in [
        (
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{}}),
        ),
        (2, "session/new", json!({"cwd":cwd,"mcpServers":[]})),
    ] {
        write_rpc(
            &mut input,
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        )
        .await?;
        let (response, _, _) = read_host(&mut output, &mut input, id, None).await?;
        assert!(response.get("error").is_none(), "{response}");
    }
    write_rpc(&mut input,json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"ses_1","prompt":[{"type":"text","text":"hello"}]}})).await?;
    let (response, updates, _) = read_host(&mut output, &mut input, 3, None).await?;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    let finals = updates
        .iter()
        .filter(|update| matches!(update["status"].as_str(), Some("completed" | "failed")))
        .collect::<Vec<_>>();
    assert_eq!(
        finals.len(),
        1,
        "received tool terminal regressed or repeated"
    );
    assert_eq!(finals[0]["status"], "completed");
    let text = finals[0]["content"][0]["content"]["text"]
        .as_str()
        .ok_or("final text missing")?;
    assert!(
        text.contains("output truncated:"),
        "oversized live result was not visibly bounded"
    );
    let frame = json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"ses_1","update":finals[0]}});
    assert!(frame.to_string().len() < 262_144);
    drop(input);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await??
            .success()
    );
    let _requests = tokio::time::timeout(std::time::Duration::from_secs(15), server).await???;
    Ok(())
}
