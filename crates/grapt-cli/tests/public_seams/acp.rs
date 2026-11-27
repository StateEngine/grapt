use super::*;

#[path = "../../../../test-support/executable.rs"]
mod executable;

#[tokio::test]
async fn binary_acp_returns_initialize_while_stdin_remains_open() -> TestResult {
    // This is a protocol/duplex witness, not a cold executable I/O benchmark.
    // Preparation precedes the unchanged five-second initialize allowance.
    static PREPARED: std::sync::OnceLock<Result<u64, String>> = std::sync::OnceLock::new();
    PREPARED
        .get_or_init(|| executable::prepare(Path::new(env!("CARGO_BIN_EXE_grapt"))))
        .as_ref()
        .map_err(|error| format!("ACP {error}"))?;
    let root = Scratch::new("acp-live-initialize")?;
    let pid = std::process::id();
    let build = "grapt-cli-public-test";
    let health = format!(
        "{{\"schema\":\"secode.api.health.v1\",\"pid\":{pid},\"build\":\"{build}\",\"environment\":\"test\",\"service_instance\":\"instance-live\"}}"
    );
    let (endpoint, server) =
        spawn_server(vec![Script::json("GET", "/api/health", 200, &health)]).await?;
    write_discovery_fixture(root.path(), pid, &endpoint, build)?;

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
        .arg("acp")
        .arg("--service")
        .arg(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("binary stdin pipe absent")?;
    let stdout = child.stdout.take().ok_or("binary stdout pipe absent")?;
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":1,\"clientCapabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1.0.0\"}}}\n")
        .await?;
    let mut line = String::new();
    let mut reader = BufReader::new(stdout);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read_line(&mut line),
    )
    .await
    .map_err(|error| {
        format!(
            "ACP initialize response exceeded 5s: {error}; health fixture finished={}; child status={:?}",
            server.is_finished(),
            child.try_wait()
        )
    })??;
    assert!(line.contains("\"protocolVersion\":1"));
    assert!(
        child.try_wait()?.is_none(),
        "ACP exited while stdin remained open"
    );
    drop(stdin);
    let status = child.wait().await?;
    assert!(status.success());
    assert_eq!(server.await??, ["GET /api/health"]);
    Ok(())
}

#[tokio::test]
async fn acp_broken_stdout_terminates_an_inflight_connection() -> TestResult {
    let (agent_input, mut client_input) = tokio::io::duplex(4_096);
    let (agent_output, client_output) = tokio::io::duplex(4_096);
    drop(client_output);
    let task = tokio::spawn(serve_acp(
        client("http://127.0.0.1:9")?,
        agent_input,
        agent_output,
        tokio::io::sink(),
        CliLimits::production(),
    ));
    write_rpc(&mut client_input, serde_json::json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"fixture","version":"1"}}
    })).await?;
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), task).await??;
    assert!(
        matches!(
            outcome,
            Err(AcpConnectionError::Write(_) | AcpConnectionError::OutputClosed)
        ),
        "broken ACP stdout did not terminate the connection: {outcome:?}"
    );
    drop(client_input);
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn acp_session_registration_is_live_and_deduplicated_per_session_config() -> TestResult {
    let (cwd, encoded_cwd) = if cfg!(windows) {
        ("C:/workspace", "C%3A%2Fworkspace")
    } else {
        ("/workspace", "%2Fworkspace")
    };
    let query = format!("?location%5Bdirectory%5D={encoded_cwd}");
    let default = r#"{"data":{"providerID":"vendor/slash","id":"model/name","variant":"high"}}"#;
    let models = r#"{"data":[{"providerID":"vendor/slash","id":"model/name","name":"Model","enabled":true,"variants":[{"id":"high"},{"id":"low"}]}]}"#;
    let agents = r#"{"data":[{"id":"build","name":"Build","mode":"primary","hidden":false},{"id":"review","name":"Review","mode":"primary","hidden":false}]}"#;
    let empty = r#"{"data":[]}"#;
    let session = format!(
        r#"{{"data":{{"id":"ses_1","title":"ACP","location":{{"directory":"{cwd}"}},"agent":"build","model":{{"providerID":"vendor/slash","id":"model/name","variant":"high"}}}}}}"#
    );
    let mut scripts = Vec::new();
    for _ in 0..3 {
        scripts.extend([
            Script::json("GET", format!("/api/model/default{query}"), 200, default),
            Script::json("GET", format!("/api/model{query}"), 200, models),
            Script::json("GET", format!("/api/agent{query}"), 200, agents),
            Script::json("GET", format!("/api/command{query}"), 200, empty),
            Script::json("GET", format!("/api/skill{query}"), 200, empty),
        ]);
    }
    scripts.extend([
        Script::json("POST", "/api/session", 200, &session),
        Script::json("GET", "/api/session/ses_1", 200, &session),
        Script::json("GET", "/api/session/ses_1", 200, &session),
        Script::status("PUT", format!("/api/mcp/tools{query}"), 204),
        Script::status("PUT", format!("/api/mcp/docs%2Fapi{query}"), 204),
        Script::status("PUT", format!("/api/mcp/tools{query}"), 204),
        Script::status("POST", "/api/session/ses_1/model", 204).expect_json(
            serde_json::json!({"model":{"providerID":"vendor/slash","id":"model/name","variant":"low"}}),
        ),
        Script::status("POST", "/api/session/ses_1/agent", 204)
            .expect_json(serde_json::json!({"agent":"review"})),
    ]);
    let (endpoint, server) = spawn_server(scripts).await?;
    let (agent_input, mut client_input) = tokio::io::duplex(65_536);
    let (agent_output, client_output) = tokio::io::duplex(65_536);
    let task = tokio::spawn(serve_acp(
        client(&endpoint)?,
        agent_input,
        agent_output,
        tokio::io::sink(),
        CliLimits::production(),
    ));
    let mut output = BufReader::new(client_output);
    write_rpc(&mut client_input, serde_json::json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"fixture","version":"1"}}
    })).await?;
    let initialized = read_rpc_response(&mut output, 1).await?;
    assert_eq!(
        initialized["result"]["agentCapabilities"]["mcpCapabilities"]["http"],
        true
    );

    let local = serde_json::json!({"name":"tools","command":"runner","args":["serve"],"env":[{"name":"TOKEN","value":"private"}]});
    let remote = serde_json::json!({"type":"http","name":"docs/api","url":"https://example.com/mcp","headers":[{"name":"Authorization","value":"Bearer private"}]});
    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"session/new",
        "params":{"cwd":cwd,"mcpServers":[local.clone(),local.clone(),remote.clone()]}
        }),
    )
    .await?;
    let created = read_rpc_response(&mut output, 2).await?;
    assert_eq!(created["result"]["sessionId"], "ses_1", "{created}");
    assert_eq!(
        created["result"]["configOptions"][0]["currentValue"],
        "vendor/slash/model/name"
    );

    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":3, "method":"session/resume",
        "params":{"cwd":cwd,"sessionId":"ses_1","mcpServers":[local.clone(),remote]}
        }),
    )
    .await?;
    let resumed = read_rpc_response(&mut output, 3).await?;
    assert!(
        resumed.get("error").is_none(),
        "unchanged MCP registration was not deduplicated: {resumed}"
    );

    write_rpc(&mut client_input, serde_json::json!({
        "jsonrpc":"2.0", "id":4, "method":"session/resume",
        "params":{"cwd":cwd,"sessionId":"ses_1","mcpServers":[{"name":"tools","command":"runner","args":["changed"],"env":[]}]}
    })).await?;
    let changed = read_rpc_response(&mut output, 4).await?;
    assert!(
        changed.get("error").is_none(),
        "changed MCP registration did not reconcile: {changed}"
    );
    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":5, "method":"session/set_config_option",
            "params":{"sessionId":"ses_1","configId":"effort","value":"low"}
        }),
    )
    .await?;
    let effort = read_rpc_response(&mut output, 5).await?;
    assert_eq!(
        effort["result"]["configOptions"][1]["currentValue"], "low",
        "{effort}"
    );
    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":6, "method":"session/set_mode",
            "params":{"sessionId":"ses_1","modeId":"review"}
        }),
    )
    .await?;
    assert!(read_rpc_response(&mut output, 6)
        .await?
        .get("error")
        .is_none());
    drop(client_input);
    task.await??;
    let requests = server.await??;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("PUT /api/mcp/"))
            .count(),
        3,
        "duplicate or unchanged ACP MCP registration reached the runtime"
    );
    Ok(())
}

/// Benchmark rerun 2026-10-07: selecting a model chose its FIRST effort, so
/// every ACP session sent "low" where Codex sends the account default. No
/// effort is chosen until the client chooses one, and "default" clears it.
#[tokio::test]
async fn acp_leaves_an_unchosen_effort_to_the_account_default() -> TestResult {
    let (cwd, encoded_cwd) = if cfg!(windows) {
        ("C:/workspace", "C%3A%2Fworkspace")
    } else {
        ("/workspace", "%2Fworkspace")
    };
    let query = format!("?location%5Bdirectory%5D={encoded_cwd}");
    let default = r#"{"data":{"providerID":"vendor","id":"model"}}"#;
    let models = r#"{"data":[{"providerID":"vendor","id":"model","name":"Model","enabled":true,"variants":[{"id":"low"},{"id":"medium"},{"id":"high"}]}]}"#;
    let agents = r#"{"data":[{"id":"build","name":"Build","mode":"primary","hidden":false}]}"#;
    let empty = r#"{"data":[]}"#;
    let session = format!(
        r#"{{"data":{{"id":"ses_1","title":"ACP","location":{{"directory":"{cwd}"}},"agent":"build","model":{{"providerID":"vendor","id":"model"}}}}}}"#
    );
    let unchosen = serde_json::json!({"model":{"providerID":"vendor","id":"model"}});
    let scripts = vec![
        Script::json("GET", format!("/api/model/default{query}"), 200, default),
        Script::json("GET", format!("/api/model{query}"), 200, models),
        Script::json("GET", format!("/api/agent{query}"), 200, agents),
        Script::json("GET", format!("/api/command{query}"), 200, empty),
        Script::json("GET", format!("/api/skill{query}"), 200, empty),
        Script::json("POST", "/api/session", 200, &session),
        Script::status("POST", "/api/session/ses_1/model", 204).expect_json(unchosen.clone()),
        Script::status("POST", "/api/session/ses_1/model", 204).expect_json(
            serde_json::json!({"model":{"providerID":"vendor","id":"model","variant":"high"}}),
        ),
        Script::status("POST", "/api/session/ses_1/model", 204).expect_json(unchosen),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let (agent_input, mut client_input) = tokio::io::duplex(65_536);
    let (agent_output, client_output) = tokio::io::duplex(65_536);
    let task = tokio::spawn(serve_acp(
        client(&endpoint)?,
        agent_input,
        agent_output,
        tokio::io::sink(),
        CliLimits::production(),
    ));
    let mut output = BufReader::new(client_output);
    write_rpc(&mut client_input, serde_json::json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"fixture","version":"1"}}
    })).await?;
    read_rpc_response(&mut output, 1).await?;
    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"session/new",
            "params":{"cwd":cwd,"mcpServers":[]}
        }),
    )
    .await?;
    let created = read_rpc_response(&mut output, 2).await?;
    let effort = &created["result"]["configOptions"][1];
    assert_eq!(effort["id"], "effort", "{created}");
    assert_eq!(effort["currentValue"], "default", "{created}");
    assert_eq!(effort["options"][0]["value"], "default", "{created}");
    for (id, config, value, current) in [
        (3, "model", "vendor/model", "default"),
        (4, "effort", "high", "high"),
        (5, "effort", "default", "default"),
    ] {
        write_rpc(
            &mut client_input,
            serde_json::json!({
                "jsonrpc":"2.0", "id":id, "method":"session/set_config_option",
                "params":{"sessionId":"ses_1","configId":config,"value":value}
            }),
        )
        .await?;
        let changed = read_rpc_response(&mut output, id).await?;
        assert_eq!(
            changed["result"]["configOptions"][1]["currentValue"], current,
            "{config}={value}: {changed}"
        );
    }
    drop(client_input);
    task.await??;
    server.await??;
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn acp_lifecycle_uses_durable_load_replay_resume_fork_close_delete_and_list() -> TestResult {
    let (cwd, encoded_cwd) = if cfg!(windows) {
        ("C:/workspace", "C%3A%2Fworkspace")
    } else {
        ("/workspace", "%2Fworkspace")
    };
    let query = format!("?location%5Bdirectory%5D={encoded_cwd}");
    let default = r#"{"data":{"providerID":"vendor","id":"model","variant":"high"}}"#;
    let models = r#"{"data":[{"providerID":"vendor","id":"model","name":"Model","enabled":true,"variants":[{"id":"high"}]}]}"#;
    let agents = r#"{"data":[{"id":"build","name":"Build","mode":"primary","hidden":false}]}"#;
    let commands = r#"{"data":[{"name":"review","description":"Review code"}]}"#;
    let skills = r#"{"data":[{"name":"audit","description":"Audit changes","slash":true}]}"#;
    let parent = format!(
        r#"{{"data":{{"id":"ses_parent","title":"Parent","location":{{"directory":"{cwd}"}},"agent":"build","model":{{"providerID":"vendor","id":"model","variant":"high"}}}}}}"#
    );
    // The fork response is decoded as the typed Session.Info (ApiClient::fork_session).
    let child = format!(
        r#"{{"data":{{"id":"ses_child","parentID":"ses_parent","projectID":"prj_acp","cost":0.0,"tokens":{{"input":0.0,"output":0.0,"reasoning":0.0,"cache":{{"read":0.0,"write":0.0}}}},"time":{{"created":1.0,"updated":1.0}},"title":"Child","location":{{"directory":"{cwd}"}},"agent":"build","model":{{"providerID":"vendor","id":"model","variant":"high"}}}}}}"#
    );
    let history = r#"{"data":[{"id":"msg_user","type":"user","text":"hello"},{"id":"msg_agent","type":"assistant","content":[{"type":"reasoning","text":"thought"},{"type":"text","text":"answer"}]}]}"#;
    let mut scripts = Vec::new();
    for _ in 0..3 {
        scripts.extend([
            Script::json("GET", format!("/api/model/default{query}"), 200, default),
            Script::json("GET", format!("/api/model{query}"), 200, models),
            Script::json("GET", format!("/api/agent{query}"), 200, agents),
            Script::json("GET", format!("/api/command{query}"), 200, commands),
            Script::json("GET", format!("/api/skill{query}"), 200, skills),
        ]);
    }
    scripts.extend([
        Script::json("GET", "/api/session/ses_parent", 200, &parent),
        Script::json(
            "GET",
            "/api/session/ses_parent/message?order=asc&limit=200",
            200,
            history,
        ),
        Script::status("POST", "/api/session/ses_parent/interrupt", 204),
        Script::json("GET", "/api/session/ses_parent", 200, &parent),
        Script::json("POST", "/api/session/ses_parent/fork", 200, &child)
            .expect_json(serde_json::json!({})),
        Script::json(
            "GET",
            "/api/session/ses_child/message?order=asc&limit=200",
            200,
            history,
        ),
        Script::status("DELETE", "/api/session/ses_child", 204),
        Script::json(
            "GET",
            format!("/api/session?order=desc&limit=100&directory={encoded_cwd}&cursor=cursor_1"),
            200,
            &format!(r#"{{"data":[{{"id":"ses_parent","title":"Parent","location":{{"directory":"{cwd}"}}}}],"cursor":{{"next":"cursor_2"}}}}"#),
        ),
    ]);
    let (endpoint, server) = spawn_server(scripts).await?;
    let (agent_input, mut client_input) = tokio::io::duplex(65_536);
    let (agent_output, client_output) = tokio::io::duplex(65_536);
    let task = tokio::spawn(serve_acp(
        client(&endpoint)?,
        agent_input,
        agent_output,
        tokio::io::sink(),
        CliLimits::production(),
    ));
    let mut output = BufReader::new(client_output);
    write_rpc(&mut client_input, serde_json::json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"fixture","version":"1"}}
    })).await?;
    assert!(read_rpc_response(&mut output, 1)
        .await?
        .get("error")
        .is_none());

    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"session/load",
            "params":{"cwd":cwd,"sessionId":"ses_parent","mcpServers":[]}
        }),
    )
    .await?;
    let (loaded, load_frames) = read_rpc_response_with_frames(&mut output, 2).await?;
    assert!(loaded.get("error").is_none(), "{loaded}");
    let replayed = serde_json::to_string(&load_frames)?;
    assert!(replayed.contains("available_commands_update"));
    assert!(replayed.contains("review") && replayed.contains("audit"));
    assert!(replayed.contains("thought") && replayed.contains("answer"));

    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":3, "method":"session/close",
            "params":{"sessionId":"ses_parent"}
        }),
    )
    .await?;
    assert!(read_rpc_response(&mut output, 3)
        .await?
        .get("error")
        .is_none());

    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":4, "method":"session/resume",
            "params":{"cwd":cwd,"sessionId":"ses_parent"}
        }),
    )
    .await?;
    assert!(read_rpc_response(&mut output, 4)
        .await?
        .get("error")
        .is_none());

    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":5, "method":"session/fork",
            "params":{"cwd":cwd,"sessionId":"ses_parent"}
        }),
    )
    .await?;
    let forked = read_rpc_response(&mut output, 5).await?;
    assert_eq!(forked["result"]["sessionId"], "ses_child", "{forked}");

    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":6, "method":"session/delete",
            "params":{"sessionId":"ses_child"}
        }),
    )
    .await?;
    assert!(read_rpc_response(&mut output, 6)
        .await?
        .get("error")
        .is_none());

    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":7, "method":"session/list",
            "params":{"cwd":cwd,"cursor":"cursor_1"}
        }),
    )
    .await?;
    let listed = read_rpc_response(&mut output, 7).await?;
    assert_eq!(
        listed["result"]["sessions"][0]["sessionId"], "ses_parent",
        "{listed}"
    );
    assert_eq!(listed["result"]["sessions"][0]["cwd"], cwd);
    assert_eq!(listed["result"]["nextCursor"], "cursor_2");

    drop(client_input);
    task.await??;
    let requests = server.await??;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("/message?order=asc&limit=200"))
            .count(),
        2,
        "load and fork must replay, while resume must not"
    );
    Ok(())
}

#[tokio::test]
async fn acp_new_session_retries_a_transiently_incomplete_catalog() -> TestResult {
    let (cwd, endpoint_cwd) = if cfg!(windows) {
        ("C:/catalog-retry", "C:/catalog-retry")
    } else {
        ("/catalog-retry", "/catalog-retry")
    };
    let (endpoint, stop, server) = spawn_retrying_catalog_server(endpoint_cwd).await?;
    let (agent_input, mut client_input) = tokio::io::duplex(65_536);
    let (agent_output, client_output) = tokio::io::duplex(65_536);
    let task = tokio::spawn(serve_acp(
        client(&endpoint)?,
        agent_input,
        agent_output,
        tokio::io::sink(),
        CliLimits::production(),
    ));
    let mut output = BufReader::new(client_output);
    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"fixture","version":"1"}}
        }),
    )
    .await?;
    assert!(read_rpc_response(&mut output, 1)
        .await?
        .get("error")
        .is_none());
    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"session/new",
            "params":{"cwd":cwd,"mcpServers":[]}
        }),
    )
    .await?;
    let created = read_rpc_response(&mut output, 2).await?;
    assert!(
        created["result"]["sessionId"] == "ses_retry",
        "catalog readiness was not retried to a successful session: {created}"
    );
    drop(client_input);
    task.await??;
    let _stopped = stop.send(());
    let requests = server.await??;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /api/model/default?"))
            .count(),
        2,
        "catalog readiness was not retried exactly once after composition"
    );
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn acp_prompt_projects_duplex_events_permissions_files_usage_and_terminal_reason(
) -> TestResult {
    let root = Scratch::new("acp-prompt-duplex")?;
    let cwd = root.path().to_string_lossy().replace('\\', "/");
    let first = root.path().join("first.txt");
    let second = root.path().join("second.txt");
    std::fs::write(&first, "first contents")?;
    std::fs::write(&second, "second contents")?;
    let encoded_cwd = encode_query_component(&cwd);
    let query = format!("?location%5Bdirectory%5D={encoded_cwd}");
    let default = r#"{"data":{"providerID":"vendor","id":"model","variant":"high"}}"#;
    let models = r#"{"data":[{"providerID":"vendor","id":"model","name":"Model","enabled":true,"variants":[{"id":"high"}]}]}"#;
    let agents = r#"{"data":[{"id":"build","name":"Build","mode":"primary","hidden":false}]}"#;
    let empty = r#"{"data":[]}"#;
    let session = format!(
        r#"{{"data":{{"id":"ses_1","title":"ACP","location":{{"directory":"{cwd}"}},"agent":"build","model":{{"providerID":"vendor","id":"model","variant":"high"}}}}}}"#
    );

    let mut event_values = vec![serde_json::json!({
        "type":"server.connected", "id":"evt_connected", "data":{}
    })];
    let durable =
        |kind: &str, id: &str, session_id: &str, seq: u64, version: u8, data: serde_json::Value| {
            serde_json::json!({
                "type":kind, "id":id, "created":1.0,
                "durable":{"aggregateID":session_id,"seq":seq,"version":version},
                "data":data
            })
        };
    let live = |kind: &str, id: &str, data: serde_json::Value| serde_json::json!({"type":kind,"id":id,"created":1.0,"data":data});
    event_values.extend([
        durable(
            "session.input.promoted",
            "evt_promoted",
            "ses_1",
            1,
            1,
            serde_json::json!({"sessionID":"ses_1","inputID":"inp_1"}),
        ),
        durable(
            "session.execution.started",
            "evt_execution_started",
            "ses_1",
            2,
            1,
            serde_json::json!({"sessionID":"ses_1","executionID":"exe_1"}),
        ),
        durable(
            "session.step.started",
            "evt_step",
            "ses_1",
            3,
            1,
            serde_json::json!({"sessionID":"ses_1","assistantMessageID":"msg_1"}),
        ),
        live(
            "session.text.delta",
            "evt_foreign",
            serde_json::json!({"sessionID":"ses_foreign","assistantMessageID":"msg_x","ordinal":0,"delta":"foreign"}),
        ),
        durable(
            "session.tool.input.started",
            "evt_tool_input",
            "ses_1",
            4,
            1,
            serde_json::json!({"sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1","name":"edit"}),
        ),
        durable(
            "session.tool.called",
            "evt_tool_called",
            "ses_1",
            5,
            1,
            serde_json::json!({"sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1","input":{"path":"first.txt"}}),
        ),
        durable(
            "permission.asked",
            "evt_permission",
            "ses_1",
            6,
            2,
            serde_json::json!({"sessionID":"ses_1","id":"per_1","action":"edit","source":{"callID":"call_1"},"resources":["first.txt","*"]}),
        ),
        durable(
            "form.created",
            "evt_form",
            "ses_1",
            7,
            2,
            serde_json::json!({"form":{"id":"form_1","sessionID":"ses_1"}}),
        ),
        live(
            "session.reasoning.delta",
            "evt_reasoning",
            serde_json::json!({"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"thinking"}),
        ),
        live(
            "session.text.delta",
            "evt_text",
            serde_json::json!({"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"answer"}),
        ),
        live(
            "session.usage.updated",
            "evt_usage",
            serde_json::json!({"sessionID":"ses_1","tokens":{"input":3,"output":4,"reasoning":2,"cache":{"read":5,"write":6}}}),
        ),
        durable(
            "session.tool.success",
            "evt_tool_success",
            "ses_1",
            8,
            2,
            serde_json::json!({
                "sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1",
                "content":[{"type":"text","text":"edited"}],
                "metadata":{"files":[{"file":first},{"file":second}]},"executed":true
            }),
        ),
        durable(
            "session.execution.succeeded",
            "evt_terminal",
            "ses_1",
            9,
            1,
            serde_json::json!({"sessionID":"ses_1"}),
        ),
    ]);
    let sse = event_values
        .into_iter()
        .map(|value| format!("event: message\r\ndata: {value}"))
        .collect::<Vec<_>>()
        .join("\r\n\r\n")
        + "\r\n\r\n";

    let scripts = vec![
        Script::json("GET", format!("/api/model/default{query}"), 200, default),
        Script::json("GET", format!("/api/model{query}"), 200, models),
        Script::json("GET", format!("/api/agent{query}"), 200, agents),
        Script::json("GET", format!("/api/command{query}"), 200, empty),
        Script::json("GET", format!("/api/skill{query}"), 200, empty),
        Script::json("POST", "/api/session", 200, &session).expect_json(serde_json::json!({
            "location":{"directory":cwd},
            "agent":"build",
            "model":{"providerID":"vendor","id":"model","variant":"high"}
        })),
        // Promotion reads history for MCP notices before the terminal lookup.
        Script::json(
            "GET",
            "/api/session/ses_1/message?order=desc&limit=50",
            200,
            r#"{"data":[]}"#,
        ),
        Script::sse("GET", "/api/event", &sse),
        Script::json(
            "POST",
            "/api/session/ses_1/prompt",
            200,
            r#"{"data":{"id":"inp_1"}}"#,
        )
        .expect_json(serde_json::json!({
            "text":"hello","delivery":"steer",
            "files":[
                {"uri":"file:///work/input.txt","name":"input.txt"},
                {"uri":"data:image/png;base64,AA==","name":"pixel.png"}
            ]
        })),
        Script::status("POST", "/api/session/ses_1/permission/per_1/reply", 204)
            .expect_json(serde_json::json!({"reply":"always"})),
        Script::status("POST", "/api/session/ses_1/interrupt", 204),
        Script::status("POST", "/api/session/ses_1/interrupt", 204),
        Script::status("POST", "/api/session/ses_1/form/form_1/cancel", 204)
            .expect_json(serde_json::json!({})),
        Script::json(
            "GET",
            "/api/session/ses_1/message/msg_1",
            200,
            r#"{"data":{"finish":"length"}}"#,
        ),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let (agent_input, mut client_input) = tokio::io::duplex(65_536);
    let (agent_output, client_output) = tokio::io::duplex(65_536);
    let task = tokio::spawn(serve_acp(
        client(&endpoint)?,
        agent_input,
        agent_output,
        tokio::io::sink(),
        CliLimits::production(),
    ));
    let mut output = BufReader::new(client_output);
    write_rpc(&mut client_input, serde_json::json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{"protocolVersion":1,"clientCapabilities":{"fs":{"writeTextFile":true}},"clientInfo":{"name":"fixture","version":"1"}}
    })).await?;
    assert!(read_rpc_response(&mut output, 1)
        .await?
        .get("error")
        .is_none());
    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"session/new",
            "params":{"cwd":cwd,"mcpServers":[]}
        }),
    )
    .await?;
    let created = read_rpc_response(&mut output, 2).await?;
    assert_eq!(created["result"]["sessionId"], "ses_1", "{created}");
    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":3, "method":"session/prompt",
            "params":{"sessionId":"ses_1","prompt":[
                {"type":"text","text":"hello"},
                {"type":"resource_link","uri":"file:///work/input.txt","name":"input.txt"},
                {"type":"image","mimeType":"image/png","data":"AA==","uri":"file:///pixel.png"}
            ]}
        }),
    )
    .await?;

    let mut frames = Vec::new();
    let mut duplicate_seen = false;
    let terminal = loop {
        let mut line = String::new();
        if output.read_line(&mut line).await? == 0 {
            return Err("ACP output closed before prompt terminal".into());
        }
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if value.get("method").and_then(serde_json::Value::as_str)
            == Some("session/request_permission")
        {
            let id = value
                .get("id")
                .cloned()
                .ok_or("permission request id absent")?;
            frames.push(value);
            write_rpc(
                &mut client_input,
                serde_json::json!({
                    "jsonrpc":"2.0", "method":"session/cancel",
                    "params":{"sessionId":"ses_1"}
                }),
            )
            .await?;
            write_rpc(
                &mut client_input,
                serde_json::json!({
                    "jsonrpc":"2.0", "id":4, "method":"session/close",
                    "params":{"sessionId":"ses_1"}
                }),
            )
            .await?;
            write_rpc(
                &mut client_input,
                serde_json::json!({
                    "jsonrpc":"2.0", "id":3, "method":"session/cancel",
                    "params":{"sessionId":"ses_1"}
                }),
            )
            .await?;
            write_rpc(
                &mut client_input,
                serde_json::json!({
                    "jsonrpc":"2.0","id":id,
                    "result":{"outcome":{"outcome":"selected","optionId":"always"}}
                }),
            )
            .await?;
            continue;
        }
        if value.get("method").and_then(serde_json::Value::as_str) == Some("fs/write_text_file") {
            let id = value
                .get("id")
                .cloned()
                .ok_or("file write request id absent")?;
            frames.push(value);
            write_rpc(
                &mut client_input,
                serde_json::json!({
                    "jsonrpc":"2.0","id":id,"result":{}
                }),
            )
            .await?;
            continue;
        }
        if value.get("id").and_then(serde_json::Value::as_i64) == Some(3) {
            if value.get("error").is_some() {
                assert_eq!(value["error"]["code"], -32600, "{value}");
                assert!(value["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("duplicate active request id")));
                duplicate_seen = true;
                continue;
            }
            break value;
        }
        frames.push(value);
        if frames.len() > 32 {
            return Err("ACP prompt exceeded the fixture frame bound".into());
        }
    };
    assert_eq!(terminal["result"]["stopReason"], "max_tokens", "{terminal}");
    assert!(duplicate_seen, "live duplicate request id was not refused");
    assert!(
        frames.iter().any(|frame| {
            frame.get("id").and_then(serde_json::Value::as_i64) == Some(4)
                && frame.get("result").is_some()
        }),
        "close could not complete while a prompt awaited permission"
    );
    assert_eq!(terminal["result"]["usage"]["totalTokens"], 20);
    let rendered = serde_json::to_string(&frames)?;
    assert!(rendered.contains("agent_thought_chunk"));
    assert!(rendered.contains("agent_message_chunk"));
    assert!(rendered.contains("tool_call_update"));
    assert!(
        !rendered.contains("foreign"),
        "foreign session event crossed the attachment boundary"
    );
    let writes = frames
        .iter()
        .filter(|frame| {
            frame.get("method").and_then(serde_json::Value::as_str) == Some("fs/write_text_file")
        })
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 2, "all edited files must be synchronized");
    assert!(writes
        .iter()
        .any(|frame| frame["params"]["content"] == "first contents"));
    assert!(writes
        .iter()
        .any(|frame| frame["params"]["content"] == "second contents"));
    drop(client_input);
    task.await??;
    let requests = server.await??;
    assert!(requests.contains(&"POST /api/session/ses_1/permission/per_1/reply".to_owned()));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.as_str() == "POST /api/session/ses_1/interrupt")
            .count(),
        2,
        "cancel and close did not independently reach interrupt authority"
    );
    assert!(requests.contains(&"POST /api/session/ses_1/form/form_1/cancel".to_owned()));
    Ok(())
}

/// ACP `session/fork` decodes the fork response as the typed `Session.Info`
/// (`ApiClient::fork_session`): a malformed record, missing its required
/// fields, is refused with the named decode error before anything else is
/// requested, never read loosely.
#[tokio::test]
async fn acp_fork_refuses_a_malformed_fork_record_by_name() -> TestResult {
    let cwd = if cfg!(windows) {
        "C:/acp-fork"
    } else {
        "/acp-fork"
    };
    let malformed = format!(
        r#"{{"data":{{"id":"ses_child","title":"Child","location":{{"directory":"{cwd}"}},"agent":"build"}}}}"#
    );
    let (endpoint, server) = spawn_server(vec![Script::json(
        "POST",
        "/api/session/ses_parent/fork",
        200,
        &malformed,
    )])
    .await?;
    let (agent_input, mut client_input) = tokio::io::duplex(65_536);
    let (agent_output, client_output) = tokio::io::duplex(65_536);
    let task = tokio::spawn(serve_acp(
        client(&endpoint)?,
        agent_input,
        agent_output,
        tokio::io::sink(),
        CliLimits::production(),
    ));
    let mut output = BufReader::new(client_output);
    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"fixture","version":"1"}}
        }),
    )
    .await?;
    assert!(read_rpc_response(&mut output, 1)
        .await?
        .get("error")
        .is_none());
    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"session/fork",
            "params":{"sessionId":"ses_parent","cwd":cwd,"mcpServers":[]}
        }),
    )
    .await?;
    let refused = read_rpc_response(&mut output, 2).await?;
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("ordinary API request failed")
            && message.contains("not the expected versioned DTO"),
        "CCM121 a malformed fork record was not refused by the typed decode: {refused}"
    );
    drop(client_input);
    task.await??;
    assert_eq!(
        server.await??,
        ["POST /api/session/ses_parent/fork"],
        "CCM121 the ACP fork read a malformed record and went on"
    );
    Ok(())
}
