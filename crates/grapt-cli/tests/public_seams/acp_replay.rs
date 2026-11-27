//! A real ACP process loading multiple ordinary API history pages.
use super::*;
use serde_json::{json, Value};

#[tokio::test]
async fn binary_acp_load_replays_all_pages_tools_and_lifecycle_in_order() -> TestResult {
    journey(Scenario::Complete).await
}

#[tokio::test]
async fn binary_acp_load_refuses_a_repeated_cursor_before_duplicate_display() -> TestResult {
    journey(Scenario::Repeat).await
}

#[tokio::test]
async fn binary_acp_load_reports_a_later_page_failure() -> TestResult {
    journey(Scenario::Failure).await
}

#[tokio::test]
async fn binary_acp_load_refuses_a_malformed_cursor_container() -> TestResult {
    journey(Scenario::Malformed).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Complete,
    Repeat,
    Failure,
    Malformed,
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn journey(scenario: Scenario) -> TestResult {
    let repeat = scenario == Scenario::Repeat;
    let fail = scenario == Scenario::Failure;
    let root = Scratch::new("acp-history")?;
    let cwd = root.path().to_string_lossy().replace('\\', "/");
    std::fs::write(root.path().join("target.txt"), "current human contents")?;
    let query = format!("?location%5Bdirectory%5D={}", encode_query_component(&cwd));
    let pid = std::process::id();
    let build = "acp-history";
    let session = json!({"data":{"id":"ses_history","title":"History","location":{"directory":cwd},
        "agent":"build","model":{"providerID":"vendor","id":"model"}}});
    let long = format!("{}{}", "é".repeat(90_000), "\u{1}".repeat(50_000));
    let users: Vec<_> = (0..200)
        .map(|i| {
            json!({"type":"user","id":format!("msg_{i:04}"),
        "text":if i == 180 {long.clone()} else {format!("user-{i:04}")}})
        })
        .collect();
    let mut scripts = vec![Script::json("GET", "/api/health", 200,
        &json!({"schema":"secode.api.health.v1","pid":pid,"build":build,"environment":"test","service_instance":"instance-live"}).to_string())];
    // Initial load and resume both rebuild the attachment's catalog, but only
    // load reads history. No replayed edit is allowed to write the client file.
    for _ in 0..2 {
        scripts.extend([
            Script::json(
                "GET",
                format!("/api/model/default{query}"),
                200,
                r#"{"data":{"providerID":"vendor","id":"model"}}"#,
            ),
            Script::json(
                "GET",
                format!("/api/model{query}"),
                200,
                r#"{"data":[{"providerID":"vendor","id":"model","name":"Model","enabled":true,"variants":[]}]}"#,
            ),
            Script::json(
                "GET",
                format!("/api/agent{query}"),
                200,
                r#"{"data":[{"id":"build","name":"Build","mode":"primary","hidden":false}]}"#,
            ),
            Script::json("GET", format!("/api/command{query}"), 200, r#"{"data":[]}"#),
            Script::json("GET", format!("/api/skill{query}"), 200, r#"{"data":[]}"#),
            Script::json("GET", "/api/session/ses_history", 200, &session.to_string()),
        ]);
    }
    scripts.push(Script::json(
        "GET",
        "/api/session/ses_history/message?order=asc&limit=200",
        200,
        &json!({"data":users,"cursor":{"next":"opaque+/cursor"}}).to_string(),
    ));
    let continuation = "/api/session/ses_history/message?cursor=opaque%2B%2Fcursor";
    if repeat {
        scripts.push(Script::json("GET",continuation,200,
            &json!({"data":[{"type":"user","id":"msg_duplicate","text":"MUST NOT DISPLAY"}],"cursor":{"next":"opaque+/cursor"}}).to_string()));
    } else if fail {
        scripts.push(Script::status("GET", continuation, 503));
    } else if scenario == Scenario::Malformed {
        scripts.push(Script::json(
            "GET",
            continuation,
            200,
            r#"{"data":[],"cursor":"broken"}"#,
        ));
    } else {
        scripts.push(Script::json(
            "GET",
            continuation,
            200,
            &json!({"data":tail(),"cursor":{"next":"empty-tail"}}).to_string(),
        ));
        // The real product can offer a final cursor whose page is empty.
        scripts.push(Script::json(
            "GET",
            "/api/session/ses_history/message?cursor=empty-tail",
            200,
            r#"{"data":[],"cursor":{}}"#,
        ));
    }
    let (endpoint, server) = spawn_server(scripts).await?;
    write_discovery_fixture(root.path(), pid, &endpoint, build)?;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
        .args(["acp", "--service"])
        .arg(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().ok_or("ACP stdin missing")?;
    let mut output = BufReader::new(child.stdout.take().ok_or("ACP stdout missing")?);
    write_rpc(
        &mut input,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":1,"clientCapabilities":{"fs":{"writeTextFile":true}}}}),
    )
    .await?;
    let (initialized, _) = response(&mut output, 1).await?;
    assert!(initialized.get("error").is_none(), "{initialized}");
    write_rpc(
        &mut input,
        json!({"jsonrpc":"2.0","id":2,"method":"session/load",
        "params":{"sessionId":"ses_history","cwd":cwd,"mcpServers":[]}}),
    )
    .await?;
    let (loaded, frames) = response(&mut output, 2).await?;
    let replay: Vec<_> = frames
        .iter()
        .filter(|frame| frame["params"]["_meta"]["isReplay"] == true)
        .collect();
    assert!(
        !replay.is_empty(),
        "history replay did not start: {loaded}; received {} frames",
        frames.len()
    );
    let user_text = |id: &str| {
        replay
            .iter()
            .filter(|frame| frame["params"]["update"]["messageId"] == id)
            .filter_map(|frame| frame["params"]["update"]["content"]["text"].as_str())
            .collect::<String>()
    };
    for i in 0..200 {
        let expected = if i == 180 {
            long.clone()
        } else {
            format!("user-{i:04}")
        };
        assert_eq!(user_text(&format!("msg_{i:04}")), expected);
    }
    if scenario != Scenario::Complete {
        assert!(
            loaded.get("error").is_some(),
            "incomplete history returned successful load: {loaded}"
        );
        assert!(!frames
            .iter()
            .any(|frame| frame.to_string().contains("MUST NOT DISPLAY")));
        if repeat {
            assert!(loaded.to_string().contains("no progress"), "{loaded}");
        }
    } else {
        assert!(loaded.get("error").is_none(), "{loaded}");
        assert_eq!(
            user_text("msg_last"),
            "LAST HISTORY MESSAGE",
            "replay lost the later-page tail"
        );
        let mut user_ids = Vec::new();
        for frame in &replay {
            let update = &frame["params"]["update"];
            if update["sessionUpdate"] == "user_message_chunk" {
                let id = update["messageId"]
                    .as_str()
                    .ok_or("replay user identity missing")?;
                if user_ids.last().copied() != Some(id) {
                    user_ids.push(id);
                }
            }
        }
        let mut expected_ids: Vec<_> = (0..200).map(|i| format!("msg_{i:04}")).collect();
        expected_ids.push("msg_last".into());
        assert_eq!(
            user_ids, expected_ids,
            "history replay changed durable row ordering"
        );
        let updates: Vec<_> = replay
            .iter()
            .map(|frame| &frame["params"]["update"])
            .collect();
        let assistant: Vec<_> = updates
            .iter()
            .filter(|update| {
                update["messageId"] == "msg_assistant" || update.get("toolCallId").is_some()
            })
            .collect();
        assert_eq!(assistant[0]["content"]["text"], "before-tool");
        assert_eq!(assistant[1]["sessionUpdate"], "tool_call");
        assert_eq!(assistant[1]["toolCallId"], "call_done");
        assert_eq!(assistant[2]["status"], "completed");
        assert_eq!(
            assistant[2]["content"][0]["content"]["text"],
            "historical result"
        );
        assert_eq!(assistant[3]["content"]["text"], "after-tool");
        for (call, status) in [
            ("call_stream", "pending"),
            ("call_running", "in_progress"),
            ("call_error", "failed"),
        ] {
            assert!(updates
                .iter()
                .any(|update| update["toolCallId"] == call && update["status"] == status));
        }
        let lifecycle: Vec<_> = updates
            .iter()
            .filter_map(|update| update["content"]["text"].as_str())
            .map(str::trim)
            .filter(|text| text.starts_with("[system]"))
            .collect();
        assert!(
            lifecycle.windows(3).any(|rows| rows
                == [
                    "[system] Execution stopped before completing.",
                    "[system] Compaction running.",
                    "[system] Compaction completed.\nSummary: preserved context"
                ]),
            "{lifecycle:?}"
        );
        assert!(lifecycle.contains(&"[system] Compaction failed: refused summary"));
        for notice in [
            "[system] Agent changed to review.",
            "[system] Model changed to vendor/next.",
            "[system] Skill audit: audit instructions",
            "[system] historical context",
            "[system] Shell killed: echo historical",
            "[system] Shell output is truncated in recorded history.",
            "[system] Assistant failed: interrupted answer",
        ] {
            assert!(
                lifecycle.contains(&notice),
                "history lifecycle missing: {notice}"
            );
        }
        assert_eq!(user_text("msg_shell"),"\n[system] Shell killed: echo historical\nrecorded output\n[system] Shell output is truncated in recorded history.\n");
        assert!(
            updates
                .iter()
                .any(|update| update["messageId"] == "msg_shell"
                    && update["content"]["text"] == "recorded output"),
            "recorded output bytes were changed"
        );
        assert_eq!(
            user_text("msg_failed"),
            "unfinished answer\n[system] Assistant failed: interrupted answer\n",
            "assistant failure ran into its unfinished text"
        );
        assert!(updates
            .iter()
            .any(|update| update["messageId"] == "msg_assistant"
                && update["_meta"]["grapt/finish"] == "tool-calls"));
    }
    assert!(
        frames
            .iter()
            .all(|frame| frame["method"] == "session/update"),
        "load requested a historical effect"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("target.txt"))?,
        "current human contents"
    );
    write_rpc(
        &mut input,
        json!({"jsonrpc":"2.0","id":3,"method":"session/resume",
        "params":{"sessionId":"ses_history","cwd":cwd,"mcpServers":[]}}),
    )
    .await?;
    let (resumed, resume_frames) = response(&mut output, 3).await?;
    assert!(resumed.get("error").is_none(), "{resumed}");
    assert!(
        !resume_frames
            .iter()
            .any(|frame| frame["params"]["_meta"]["isReplay"] == true),
        "resume replayed history"
    );
    drop(input);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await??
            .success()
    );
    let requests = tokio::time::timeout(std::time::Duration::from_secs(15), server).await???;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("/message?"))
            .count(),
        if scenario != Scenario::Complete { 2 } else { 3 }
    );
    Ok(())
}

fn tail() -> Vec<Value> {
    let tool = |id: &str, status: &str| {
        json!({"type":"tool","id":id,"name":"edit","state":{
        "status":status,"input":if status == "streaming" {json!("{\"path\":")} else {json!({})},"content":[{"type":"text","text":"historical result"}],
        "metadata":{"files":[{"file":"target.txt"}]},"error":{"type":"ToolFailed","message":"historical refusal"}}})
    };
    vec![
        json!({"id":"msg_assistant","type":"assistant","finish":"tool-calls","content":[
            {"type":"text","text":"before-tool"},tool("call_done","completed"),{"type":"text","text":"after-tool"},
            tool("call_stream","streaming"),tool("call_running","running"),tool("call_error","error")]}),
        json!({"id":"msg_system","type":"system","text":"Execution stopped before completing."}),
        json!({"id":"msg_compact_running","type":"compaction","status":"running"}),
        json!({"id":"msg_compact_done","type":"compaction","status":"completed","summary":"preserved context"}),
        json!({"id":"msg_compact_error","type":"compaction","status":"failed","error":{"message":"refused summary"}}),
        json!({"id":"msg_agent_change","type":"agent-switched","agent":"review"}),
        json!({"id":"msg_model_change","type":"model-switched","model":{"providerID":"vendor","id":"next"}}),
        json!({"id":"msg_skill","type":"skill","name":"audit","text":"audit instructions"}),
        json!({"id":"msg_synthetic","type":"synthetic","text":"historical context"}),
        json!({"id":"msg_shell","type":"shell","command":"echo historical","status":"killed","output":{"output":"recorded output","truncated":true}}),
        json!({"id":"msg_failed","type":"assistant","finish":"error","error":{"type":"aborted","message":"interrupted answer"},"content":[{"type":"text","text":"unfinished answer"}]}),
        json!({"id":"msg_last","type":"user","text":"LAST HISTORY MESSAGE"}),
    ]
}

async fn response(
    output: &mut BufReader<tokio::process::ChildStdout>,
    id: i64,
) -> TestResult<(Value, Vec<Value>)> {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let mut frames = Vec::new();
        for _ in 0..1000 {
            let mut line = String::new();
            if output.read_line(&mut line).await? == 0 {
                return Err("ACP stdout closed before response".into());
            }
            assert!(
                line.len() <= 262_144,
                "replay frame exceeded transport bound"
            );
            let frame: Value = serde_json::from_str(&line)?;
            if frame["id"].as_i64() == Some(id) {
                return Ok((frame, frames));
            }
            assert!(
                frame.get("id").is_none(),
                "historical load requested a client effect: {frame}"
            );
            frames.push(frame);
        }
        Err("ACP replay exceeded witness frame bound".into())
    })
    .await?
}
