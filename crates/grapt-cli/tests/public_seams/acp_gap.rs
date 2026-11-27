//! A real shipped ACP process recovering from finite ordinary HTTP streams.
use super::*;
use serde_json::{json, Value};

#[path = "acp_gap/live_tool.rs"]
mod live_tool;

/// Cross-compiled witnesses receive their exact staged CLI path explicitly.
fn binary() -> PathBuf {
    let path = std::env::var_os("GRAPT_ACP_GAP_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_grapt")));
    assert!(
        path.is_file(),
        "ACP witness binary is absent: {}",
        path.display()
    );
    path
}

fn live(kind: &str, data: Value) -> Value {
    json!({"type":kind,"id":"evt_gap","created":1.0,"data":data})
}
fn durable(kind: &str, seq: u64, version: u8, data: Value) -> Value {
    json!({"type":kind,"id":format!("evt_{seq}"),"created":1.0,
        "durable":{"aggregateID":"ses_1","seq":seq,"version":version},"data":data})
}
fn sse(events: Vec<Value>) -> String {
    events
        .into_iter()
        .map(|event| format!("event: message\ndata: {event}\n\n"))
        .collect()
}

#[tokio::test]
async fn binary_acp_drop_recovers_ordered_parts_and_tools_once() -> TestResult {
    recover(false, false, false, false).await
}
#[tokio::test]
async fn binary_acp_double_drop_holds_display_and_keeps_controls_live() -> TestResult {
    recover(true, false, false, false).await
}
#[tokio::test]
async fn binary_acp_history_prefix_mismatch_is_a_fault() -> TestResult {
    recover(false, true, false, false).await
}

#[tokio::test]
async fn binary_acp_pending_overflow_recovers_before_the_stream_reaches_eof() -> TestResult {
    recover(false, false, true, false).await
}

#[tokio::test]
async fn binary_acp_completed_file_sync_is_not_repeated_after_a_gap() -> TestResult {
    recover(false, false, false, true).await
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn recover(double: bool, corrupt: bool, overflow: bool, completed_file: bool) -> TestResult {
    let root = Scratch::new("acp-gap-host")?;
    let cwd = root.path().to_string_lossy().replace('\\', "/");
    let query = format!("?location%5Bdirectory%5D={}", encode_query_component(&cwd));
    let pid = std::process::id();
    let build = "acp-gap-host";
    let health = json!({"schema":"secode.api.health.v1","pid":pid,"build":build,
        "environment":"test","service_instance":"instance-live"});
    let session = json!({"data":{"id":"ses_1","title":"ACP","location":{"directory":cwd},
        "agent":"build","model":{"providerID":"vendor","id":"model","variant":"high"}}});
    let connected = json!({"type":"server.connected","id":"evt_connected","data":{}});
    std::fs::write(root.path().join("target.txt"), "written once")?;
    let mut first_events = vec![
        connected.clone(),
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
        live(
            "session.reasoning.delta",
            json!({"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"think"}),
        ),
        live(
            "session.text.delta",
            json!({"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"pré"}),
        ),
        durable(
            "session.tool.input.started",
            3,
            1,
            json!({"sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1","name":"read"}),
        ),
    ];
    if completed_file {
        let completion = durable(
            "session.tool.success",
            4,
            2,
            json!({"sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1",
                "content":[{"type":"text","text":"result"}],
                "metadata":{"files":[{"file":"target.txt"}]},"executed":true}),
        );
        // Duplicate live completion and later identical canonical history must
        // retain the one completed update and one client file-write request.
        first_events.extend([completion.clone(), completion]);
    }
    let first = sse(first_events);
    let second = if double {
        sse(vec![
            connected.clone(),
            live(
                "session.text.delta",
                json!({"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"DO NOT DISPLAY"}),
            ),
            durable(
                "session.tool.success",
                4,
                2,
                json!({"sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1","content":[{"type":"text","text":"DO NOT DISPLAY"}],"metadata":{},"executed":true}),
            ),
            durable(
                "permission.asked",
                5,
                2,
                json!({"sessionID":"ses_1","id":"per_1","action":"edit","source":{"callID":"call_2"},"resources":["target.txt"]}),
            ),
            durable(
                "form.created",
                6,
                2,
                json!({"form":{"id":"form_1","sessionID":"ses_1"}}),
            ),
        ])
    } else {
        sse(vec![connected.clone()])
    };
    let mut history = json!({"data":[
        {"type":"assistant","id":"msg_1","finish":"stop","content":[
            {"type":"reasoning","text":"thinking"},
            {"type":"text","text":if corrupt { "different" } else { "préfix" }},
            {"type":"tool","id":"call_1","name":"read","state":{"status":"completed","input":{"path":"target.txt"},"content":[{"type":"text","text":"result"}],"metadata":{}}},
            {"type":"text","text":"after"},
            {"type":"tool","id":"call_2","name":"edit","state":{"status":"error","input":{},"error":{"message":"denied"},"metadata":{}}},
            {"type":"reasoning","text":"done"}
        ]}, {"type":"user","id":"inp_1","text":"hello"}
    ]});
    if completed_file {
        history["data"][0]["content"][2]["state"]["metadata"] =
            json!({"files":[{"file":"target.txt"}]});
    }
    let flood_ready = std::sync::Arc::new(tokio::sync::Notify::new());
    let first_stream = if overflow {
        let (connected_chunk, prefix) = first
            .split_once("\n\n")
            .ok_or("first connected frame missing")?;
        // Compact recognized frames exceed the 64-pending bound within one
        // ordinary receive buffer. The host releases them after its prefix.
        let flood = sse((0..80)
            .map(|_| json!({"type":"server.connected","id":"evt_flood","data":{}}))
            .collect());
        Script::sse_chunks_open(
            "/api/event",
            vec![format!("{connected_chunk}\n\n"), prefix.to_owned()],
        )
        .defer_sse(std::sync::Arc::clone(&flood_ready), flood)
    } else {
        Script::sse("GET", "/api/event", &first)
    };
    let mut scripts = vec![
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
        first_stream,
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
        Script::sse("GET", "/api/event", &second),
        Script::json(
            "GET",
            "/api/session/ses_1/permission",
            200,
            r#"{"data":[]}"#,
        ),
        Script::json(
            "GET",
            "/api/session/active",
            200,
            if double {
                r#"{"data":{"ses_1":{"type":"running"}}}"#
            } else {
                r#"{"data":{}}"#
            },
        ),
    ];
    if double {
        scripts.extend([
        Script::status("POST","/api/session/ses_1/permission/per_1/reply",204).expect_json(json!({"reply":"reject"})),
        Script::status("POST","/api/session/ses_1/form/form_1/cancel",204),
        Script::sse("GET","/api/event",&sse(vec![connected])),
        // Seeing the same permission again must not ask or grant again.
        Script::json("GET","/api/session/ses_1/permission",200,r#"{"data":[{"id":"per_1","status":"pending","action":"edit","source":{"callID":"call_2"},"resources":["target.txt"]}]}"#),
        Script::json("GET","/api/session/active",200,r#"{"data":{}}"#),
    ]);
    }
    scripts.push(Script::json(
        "GET",
        "/api/session/ses_1/message?order=desc&limit=50",
        200,
        &history.to_string(),
    ));
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
            json!({"protocolVersion":1,"clientCapabilities":{"fs":{"writeTextFile":completed_file}}}),
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
    let (response, updates, writes) = read_host(
        &mut output,
        &mut input,
        3,
        overflow.then_some(flood_ready.as_ref()),
    )
    .await?;
    drop(input);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await
            .map_err(|error| format!("ACP child exit after stdin EOF exceeded 10s: {error}"))??
            .success()
    );
    let requests = tokio::time::timeout(std::time::Duration::from_secs(15), server)
        .await
        .map_err(|error| format!("ACP scripted server join exceeded 15s: {error}"))???;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST /api/session/ses_1/prompt"))
            .count(),
        1
    );
    if corrupt {
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|text| text.contains("prefix disagrees")),
            "{response}"
        );
        return Ok(());
    }
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    let observed = updates
        .iter()
        .map(|update| match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk" | "agent_thought_chunk") => format!(
                "{}:{}",
                update["sessionUpdate"].as_str().unwrap_or_default(),
                update["content"]["text"].as_str().unwrap_or_default()
            ),
            _ => format!(
                "{}:{}:{}",
                update["sessionUpdate"].as_str().unwrap_or_default(),
                update["toolCallId"].as_str().unwrap_or_default(),
                update["status"].as_str().unwrap_or_default()
            ),
        })
        .collect::<Vec<_>>();
    let mut expected = vec![
        "agent_thought_chunk:think",
        "agent_message_chunk:pré",
        "tool_call:call_1:pending",
        "agent_thought_chunk:ing",
        "agent_message_chunk:fix",
        "tool_call_update:call_1:completed",
        "agent_message_chunk:after",
        "tool_call:call_2:pending",
        "tool_call_update:call_2:failed",
        "agent_thought_chunk:done",
    ];
    if completed_file {
        let final_update = expected.remove(5);
        expected.insert(3, final_update);
        assert_eq!(writes.len(), 1, "completed file synchronization repeated");
        assert_eq!(writes[0]["content"], "written once");
        let path = writes[0]["path"].as_str().ok_or("file sync path absent")?;
        assert_eq!(
            std::fs::canonicalize(path)?,
            std::fs::canonicalize(root.path().join("target.txt"))?
        );
    } else {
        assert!(writes.is_empty(), "unexpected client file effect");
    }
    assert_eq!(
        observed, expected,
        "lost, duplicated or reordered ACP output"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("permission/per_1/reply"))
            .count(),
        usize::from(double)
    );
    Ok(())
}

async fn read_host(
    output: &mut BufReader<tokio::process::ChildStdout>,
    input: &mut tokio::process::ChildStdin,
    id: i64,
    flood_ready: Option<&tokio::sync::Notify>,
) -> TestResult<(Value, Vec<Value>, Vec<Value>)> {
    tokio::time::timeout(std::time::Duration::from_secs(15),async {
        let mut updates = Vec::new();
        let mut writes = Vec::new();
        for _ in 0..64 {
            let mut line = String::new();
            if output.read_line(&mut line).await? == 0 { return Err("ACP stdout closed".into()); }
            let row: Value = serde_json::from_str(&line)?;
            if row["id"].as_i64() == Some(id) { return Ok((row,updates,writes)); }
            if row["method"] == "session/update" {
                let update = &row["params"]["update"];
                if update["sessionUpdate"] == "tool_call" && update["toolCallId"] == "call_1" {
                    if let Some(signal) = flood_ready { signal.notify_one(); }
                }
                updates.push(update.clone());
            }
            if row["method"] == "fs/write_text_file" {
                writes.push(row["params"].clone());
                write_rpc(input,json!({"jsonrpc":"2.0","id":row["id"],"result":{}})).await?;
            }
            if row["method"] == "session/request_permission" {
                write_rpc(input,json!({"jsonrpc":"2.0","id":row["id"],"result":{"outcome":{"outcome":"selected","optionId":"reject"}}})).await?;
            }
        }
        Err("ACP host frame bound exceeded".into())
    }).await.map_err(|error| format!("ACP response id={id} exceeded 15s: {error}"))?
}
