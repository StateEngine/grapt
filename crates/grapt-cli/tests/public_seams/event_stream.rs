//! The advisory event stream outlives the request allowance, and ACP survives
//! the server ending it mid-prompt (benchmark rows B01..R01, 2026-10-07).
use super::*;

use std::time::Duration;

use tokio::net::TcpListener;

fn frame(value: &serde_json::Value) -> String {
    format!("event: message\r\ndata: {value}\r\n\r\n")
}

fn connected() -> String {
    frame(&serde_json::json!({"type":"server.connected","id":"evt_connected","data":{}}))
}

fn durable(kind: &str, seq: u64, version: u8, data: serde_json::Value) -> String {
    frame(&serde_json::json!({
        "type":kind, "id":format!("evt_{seq}"), "created":1.0,
        "durable":{"aggregateID":"ses_1","seq":seq,"version":version},
        "data":data
    }))
}

/// Catalog and Session creation for `ses_1` at `cwd`.
fn session_scripts(cwd: &str) -> Vec<Script> {
    let query = format!("?location%5Bdirectory%5D={}", encode_query_component(cwd));
    let session = format!(
        r#"{{"data":{{"id":"ses_1","title":"ACP","location":{{"directory":"{cwd}"}},"agent":"build","model":{{"providerID":"vendor","id":"model"}}}}}}"#
    );
    vec![
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
        Script::json("POST", "/api/session", 200, &session),
    ]
}

const PENDING_PERMISSION: &str = r#"{"data":[{"id":"per_1","sessionID":"ses_1","action":"glob","resources":["*"],"status":"pending","source":{"callID":"call_1"}}],"hasMore":false}"#;
/// Real-time bound on a stalled fixture, so a regression fails, not hangs.
const STALL: Duration = Duration::from_secs(10);
const RUNNING: &str = r#"{"data":{"ses_1":{"type":"running"}}}"#;

/// Drives one ACP prompt to its response, allowing every permission once.
async fn prompt(
    scripts: Vec<Script>,
    cwd: &str,
) -> TestResult<(serde_json::Value, Vec<serde_json::Value>, Vec<String>)> {
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
    }))
    .await?;
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
    assert_eq!(created["result"]["sessionId"], "ses_1", "{created}");
    write_rpc(
        &mut client_input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":3, "method":"session/prompt",
            "params":{"sessionId":"ses_1","prompt":[{"type":"text","text":"hello"}]}
        }),
    )
    .await?;
    let mut frames = Vec::new();
    let response = loop {
        let mut line = String::new();
        let read = tokio::time::timeout(STALL, output.read_line(&mut line))
            .await
            .map_err(|_| format!("ACP prompt stalled after {frames:?}"))??;
        if read == 0 {
            return Err("ACP output closed before the prompt response".into());
        }
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if value.get("method").and_then(serde_json::Value::as_str)
            == Some("session/request_permission")
        {
            let id = value.get("id").cloned().ok_or("permission id absent")?;
            write_rpc(
                &mut client_input,
                serde_json::json!({
                    "jsonrpc":"2.0","id":id,
                    "result":{"outcome":{"outcome":"selected","optionId":"once"}}
                }),
            )
            .await?;
        }
        if value.get("id").and_then(serde_json::Value::as_i64) == Some(3) {
            break value;
        }
        frames.push(value);
        if frames.len() > 64 {
            return Err("ACP prompt exceeded the fixture frame bound".into());
        }
    };
    drop(client_input);
    task.await??;
    let requests = tokio::time::timeout(STALL, server)
        .await
        .map_err(|_| format!("scripted requests were left unserved after {response}"))???;
    Ok((response, frames, requests))
}

/// C01 in miniature: the server ends the stream after the step opens, again
/// after the tool settles, and the terminal lands in neither stream. The
/// permission asked in the gap reaches the client exactly once and the
/// outcome is read from durable history.
#[tokio::test]
async fn acp_prompt_survives_the_server_ending_its_event_stream() -> TestResult {
    let root = Scratch::new("acp-stream-rebind")?;
    let cwd = root.path().to_string_lossy().replace('\\', "/");
    let first = connected()
        + &durable(
            "session.input.promoted",
            1,
            1,
            serde_json::json!({"sessionID":"ses_1","inputID":"inp_1"}),
        )
        + &durable(
            "session.step.started",
            2,
            1,
            serde_json::json!({"sessionID":"ses_1","assistantMessageID":"msg_1"}),
        )
        + &frame(&serde_json::json!({
            "type":"session.text.delta","id":"evt_text","created":1.0,
            "data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"partial"}
        }));
    let second = connected()
        + &durable(
            "session.tool.input.started",
            4,
            1,
            serde_json::json!({"sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1","name":"glob"}),
        )
        + &durable(
            "session.tool.success",
            5,
            2,
            serde_json::json!({
                "sessionID":"ses_1","assistantMessageID":"msg_1","callID":"call_1",
                "content":[{"type":"text","text":"app.py"}],"executed":true
            }),
        );
    let history = r#"{"data":[
        {"type":"assistant","id":"msg_1","finish":"stop","content":[
            {"type":"text","text":"partial"},
            {"type":"tool","id":"call_1","name":"glob","state":{"status":"completed","input":{},"content":[{"type":"text","text":"app.py"}],"metadata":{}}}
        ]},
        {"type":"user","id":"inp_1","text":"hello"}
    ]}"#;
    let mut scripts = session_scripts(&cwd);
    scripts.extend([
        // Promotion reads history for MCP notices before the terminal lookup.
        Script::json(
            "GET",
            "/api/session/ses_1/message?order=desc&limit=50",
            200,
            r#"{"data":[]}"#,
        ),
        Script::sse("GET", "/api/event", &first),
        Script::json(
            "POST",
            "/api/session/ses_1/prompt",
            200,
            r#"{"data":{"id":"inp_1"}}"#,
        ),
        Script::sse("GET", "/api/event", &second),
        Script::json(
            "GET",
            "/api/session/ses_1/permission",
            200,
            PENDING_PERMISSION,
        ),
        Script::status("POST", "/api/session/ses_1/permission/per_1/reply", 204)
            .expect_json(serde_json::json!({"reply":"once"})),
        Script::json("GET", "/api/session/active", 200, RUNNING),
        Script::sse("GET", "/api/event", &connected()),
        // Still listed: answered once, never asked again.
        Script::json(
            "GET",
            "/api/session/ses_1/permission",
            200,
            PENDING_PERMISSION,
        ),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json(
            "GET",
            "/api/session/ses_1/message?order=desc&limit=50",
            200,
            history,
        ),
    ]);
    let (response, frames, requests) = prompt(scripts, &cwd).await?;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    let rendered = serde_json::to_string(&frames)?;
    assert!(rendered.contains("partial"), "text before the end was lost");
    assert!(rendered.contains("\"completed\""), "tool after rebind lost");
    let asked = frames
        .iter()
        .filter(|frame| frame["method"] == "session/request_permission")
        .count();
    assert_eq!(asked, 1, "a dropped permission must be asked exactly once");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.as_str() == "GET /api/event")
            .count(),
        3
    );
    Ok(())
}

/// Live text really lost, as when an integrity refusal
/// closes the stream at once or a subscriber overflows. The first stream
/// carries "Hello " and ends mid-block; the turn finishes in the gap. The
/// client still receives the whole answer, the rest backfilled from durable
/// history, and nothing twice.
#[tokio::test]
async fn acp_prompt_backfills_the_answer_a_dropped_stream_lost() -> TestResult {
    let root = Scratch::new("acp-stream-backfill")?;
    let cwd = root.path().to_string_lossy().replace('\\', "/");
    let first = connected()
        + &durable(
            "session.input.promoted",
            1,
            1,
            serde_json::json!({"sessionID":"ses_1","inputID":"inp_1"}),
        )
        + &durable(
            "session.step.started",
            2,
            1,
            serde_json::json!({"sessionID":"ses_1","assistantMessageID":"msg_1"}),
        )
        + &frame(&serde_json::json!({
            "type":"session.text.delta","id":"evt_text","created":1.0,
            "data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"Hello "}
        }));
    let history = r#"{"data":[
        {"type":"assistant","id":"msg_1","finish":"stop",
         "content":[{"type":"text","text":"Hello world."}]},
        {"type":"user","id":"inp_1","text":"hello"}
    ]}"#;
    let mut scripts = session_scripts(&cwd);
    scripts.extend([
        // Promotion reads history for MCP notices before the terminal lookup.
        Script::json(
            "GET",
            "/api/session/ses_1/message?order=desc&limit=50",
            200,
            r#"{"data":[]}"#,
        ),
        Script::sse("GET", "/api/event", &first),
        Script::json(
            "POST",
            "/api/session/ses_1/prompt",
            200,
            r#"{"data":{"id":"inp_1"}}"#,
        ),
        Script::sse("GET", "/api/event", &connected()),
        Script::json(
            "GET",
            "/api/session/ses_1/permission",
            200,
            r#"{"data":[],"hasMore":false}"#,
        ),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json(
            "GET",
            "/api/session/ses_1/message?order=desc&limit=50",
            200,
            history,
        ),
    ]);
    let (response, frames, _requests) = prompt(scripts, &cwd).await?;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    let received: String = frames
        .iter()
        .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .filter_map(|frame| frame["params"]["update"]["content"]["text"].as_str())
        .collect();
    assert_eq!(
        received, "Hello world.",
        "the client did not receive the whole answer"
    );
    Ok(())
}

/// The first stream of a turn whose answer the stream ends after "Hello ".
fn hello_then_end() -> String {
    connected()
        + &durable(
            "session.input.promoted",
            1,
            1,
            serde_json::json!({"sessionID":"ses_1","inputID":"inp_1"}),
        )
        + &durable(
            "session.step.started",
            2,
            1,
            serde_json::json!({"sessionID":"ses_1","assistantMessageID":"msg_1"}),
        )
        + &frame(&serde_json::json!({
            "type":"session.text.delta","id":"evt_text","created":1.0,
            "data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"Hello "}
        }))
}

/// The scripts of a turn that loses its stream after "Hello ", settles idle,
/// and then reads `pages` of newest-first history (path, body).
fn backfill_scripts(cwd: &str, pages: &[(&str, String)]) -> Vec<Script> {
    let mut scripts = session_scripts(cwd);
    scripts.extend([
        Script::json(
            "GET",
            "/api/session/ses_1/message?order=desc&limit=50",
            200,
            r#"{"data":[]}"#,
        ),
        Script::sse("GET", "/api/event", &hello_then_end()),
        Script::json(
            "POST",
            "/api/session/ses_1/prompt",
            200,
            r#"{"data":{"id":"inp_1"}}"#,
        ),
        Script::sse("GET", "/api/event", &connected()),
        Script::json(
            "GET",
            "/api/session/ses_1/permission",
            200,
            r#"{"data":[],"hasMore":false}"#,
        ),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
    ]);
    for (path, body) in pages {
        scripts.push(Script::json("GET", *path, 200, body));
    }
    scripts
}

fn received_text(frames: &[serde_json::Value]) -> String {
    frames
        .iter()
        .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .filter_map(|frame| frame["params"]["update"]["content"]["text"].as_str())
        .collect()
}

/// Review AC-H2: a later prompt ran before this one settled.
/// Its answer and its failed terminal, on the first history page, belong to
/// it; this prompt's own turn is on the next page. The client receives only
/// this prompt's answer, and its result is this prompt's, not the later
/// failure.
#[tokio::test]
async fn acp_backfill_and_result_stay_with_their_own_prompt_across_pages() -> TestResult {
    let root = Scratch::new("acp-backfill-own-turn")?;
    let cwd = root.path().to_string_lossy().replace('\\', "/");
    let first_page = r#"{"data":[
        {"type":"system","id":"msg_later_failure","metadata":{"executionFailure":true},
         "time":{"created":5},"text":"Execution failed: Step budget exhausted"},
        {"type":"assistant","id":"msg_later","finish":"stop",
         "content":[{"type":"text","text":"UNRELATED_NEXT_PROMPT"}]}
    ],"cursor":{"next":"page_2"}}"#;
    let second_page = r#"{"data":[
        {"type":"user","id":"inp_later","text":"next"},
        {"type":"assistant","id":"msg_1","finish":"stop",
         "content":[{"type":"text","text":"Hello world."}]},
        {"type":"user","id":"inp_1","text":"hello"}
    ]}"#;
    let scripts = backfill_scripts(
        &cwd,
        &[
            (
                "/api/session/ses_1/message?order=desc&limit=50",
                first_page.to_owned(),
            ),
            (
                "/api/session/ses_1/message?cursor=page_2",
                second_page.to_owned(),
            ),
        ],
    );
    let (response, frames, _requests) = prompt(scripts, &cwd).await?;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    assert_eq!(received_text(&frames), "Hello world.");
    Ok(())
}

/// Review AC-H1: a legal at-bound answer with only its
/// first six bytes delivered live. The rest goes out in frames the ACP
/// transport accepts, and the client receives every byte.
#[tokio::test]
async fn acp_backfill_of_a_mostly_lost_at_bound_answer_fits_its_frames() -> TestResult {
    let root = Scratch::new("acp-backfill-at-bound")?;
    let cwd = root.path().to_string_lossy().replace('\\', "/");
    let answer = format!("Hello {}", "x".repeat(262_144 - 6));
    let history = serde_json::json!({"data":[
        {"type":"assistant","id":"msg_1","finish":"stop",
         "content":[{"type":"text","text":answer}]},
        {"type":"user","id":"inp_1","text":"hello"}
    ]})
    .to_string();
    let scripts = backfill_scripts(
        &cwd,
        &[("/api/session/ses_1/message?order=desc&limit=50", history)],
    );
    let (response, frames, _requests) = prompt(scripts, &cwd).await?;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    assert!(
        received_text(&frames) == answer,
        "the client did not receive every byte"
    );
    Ok(())
}

/// A failed execution after a good tool Step settles as the failure the live
/// `session.execution.failed` path reports, never as `end_turn` (review
/// HOLD on 8c7550e3).
#[tokio::test]
async fn acp_prompt_settles_a_missed_execution_failure_as_a_failure() -> TestResult {
    let root = Scratch::new("acp-stream-failure")?;
    let cwd = root.path().to_string_lossy().replace('\\', "/");
    let history = r#"{"data":[
        {"type":"system","id":"msg_failure","metadata":{"executionFailure":true},
         "time":{"created":3},"text":"Execution failed: Step budget exhausted"},
        {"type":"assistant","id":"msg_tool_step","finish":"tool-calls","content":[]},
        {"type":"user","id":"inp_1","text":"hello"}
    ]}"#;
    let mut scripts = session_scripts(&cwd);
    scripts.extend([
        Script::sse("GET", "/api/event", &connected()),
        Script::json(
            "POST",
            "/api/session/ses_1/prompt",
            200,
            r#"{"data":{"id":"inp_1"}}"#,
        ),
        Script::sse("GET", "/api/event", &connected()),
        Script::json(
            "GET",
            "/api/session/ses_1/permission",
            200,
            r#"{"data":[],"hasMore":false}"#,
        ),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json(
            "GET",
            "/api/session/ses_1/message?order=desc&limit=50",
            200,
            history,
        ),
    ]);
    let (response, _frames, _requests) = prompt(scripts, &cwd).await?;
    assert!(response.get("result").is_none(), "{response}");
    assert_eq!(
        response["error"]["message"], "Step budget exhausted",
        "{response}"
    );
    Ok(())
}

#[tokio::test]
async fn acp_prompt_refuses_a_stream_that_keeps_ending_without_progress() -> TestResult {
    let root = Scratch::new("acp-stream-quiet")?;
    let cwd = root.path().to_string_lossy().replace('\\', "/");
    let mut scripts = session_scripts(&cwd);
    scripts.extend([
        Script::sse("GET", "/api/event", &connected()),
        Script::json(
            "POST",
            "/api/session/ses_1/prompt",
            200,
            r#"{"data":{"id":"inp_1"}}"#,
        ),
    ]);
    for _rebind in 0..32 {
        scripts.extend([
            Script::sse("GET", "/api/event", &connected()),
            Script::json(
                "GET",
                "/api/session/ses_1/permission",
                200,
                r#"{"data":[],"hasMore":false}"#,
            ),
            Script::json("GET", "/api/session/active", 200, RUNNING),
        ]);
    }
    let (response, _frames, _requests) = prompt(scripts, &cwd).await?;
    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("without progress"),
        "a stream ending without progress must be refused by its bound: {response}"
    );
    Ok(())
}

/// Serves one event stream: `connected`, then each frame after its delay,
/// then holds the connection open for `hold`.
async fn spawn_paced_stream(
    paced: Vec<(Duration, String)>,
    hold: Duration,
) -> TestResult<(String, tokio::task::JoinHandle<TestResult>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        support::read_request(&mut stream).await?;
        let head =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(connected().as_bytes()).await?;
        for (delay, frame) in paced {
            tokio::time::sleep(delay).await;
            stream.write_all(frame.as_bytes()).await?;
        }
        tokio::time::sleep(hold).await;
        Ok(())
    });
    Ok((format!("http://{address}"), server))
}

/// Rows B02, M02, D01 and D02 died at the 30 s whole-request timeout. A
/// stream that keeps delivering stays open as long as the turn does.
#[tokio::test(start_paused = true)]
async fn an_event_stream_that_keeps_delivering_outlives_the_request_allowance() -> TestResult {
    let beat = frame(&serde_json::json!({
        "type":"session.text.delta","id":"evt_beat","created":1.0,
        "data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"."}
    }));
    let paced = (0..6)
        .map(|_| (Duration::from_secs(25), beat.clone()))
        .collect();
    let (endpoint, server) = spawn_paced_stream(paced, Duration::ZERO).await?;
    let mut events = client(&endpoint)?.events().await?;
    let began = tokio::time::Instant::now();
    assert_eq!(events.next().await?.kind(), "server.connected");
    for _beat in 0..6 {
        assert_eq!(events.next().await?.kind(), "session.text.delta");
    }
    assert!(began.elapsed() >= Duration::from_secs(150));
    server.await??;
    Ok(())
}

/// A stream that goes silent ends at the idle allowance, not never.
#[tokio::test(start_paused = true)]
async fn a_silent_event_stream_ends_at_the_idle_allowance() -> TestResult {
    let (endpoint, server) = spawn_paced_stream(Vec::new(), Duration::from_secs(600)).await?;
    let mut events = client(&endpoint)?.events().await?;
    assert_eq!(events.next().await?.kind(), "server.connected");
    let began = tokio::time::Instant::now();
    let ended = events.next().await;
    let silent = began.elapsed();
    assert!(
        matches!(&ended, Err(grapt_cli::ApiClientError::Transport(error)) if error.is_timeout()),
        "a silent stream must end on its idle bound: {ended:?}"
    );
    assert!(
        silent >= Duration::from_secs(60) && silent < Duration::from_secs(90),
        "idle bound fired after {silent:?}"
    );
    server.abort();
    Ok(())
}

/// A kind this client does not know arrives mid-turn (a
/// newer server's vocabulary), then the real global web-search notice. The
/// stream skips the first, admits the second (no Session, so ACP ignores
/// it), delivers the following delta, and the turn completes, where the
/// closed closure used to end it with -32603.
#[tokio::test]
async fn acp_prompt_skips_an_unknown_event_kind_and_completes() -> TestResult {
    let root = Scratch::new("acp-unknown-kind")?;
    let cwd = root.path().to_string_lossy().replace('\\', "/");
    let first = connected()
        + &durable(
            "session.input.promoted",
            1,
            1,
            serde_json::json!({"sessionID":"ses_1","inputID":"inp_1"}),
        )
        + &durable(
            "session.step.started",
            2,
            1,
            serde_json::json!({"sessionID":"ses_1","assistantMessageID":"msg_1"}),
        )
        + &frame(&serde_json::json!({
            "type":"session.future.notice","id":"evt_future","created":1.0,
            "data":{"sessionID":"ses_1"}
        }))
        // The real global web-search notice: empty data, no Session.
        + &frame(&serde_json::json!({
            "type":"websearch.updated","id":"evt_search","created":1.0,"data":{}
        }))
        + &frame(&serde_json::json!({
            "type":"session.text.delta","id":"evt_text","created":1.0,
            "data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"after-unknown"}
        }));
    let history = r#"{"data":[
        {"type":"assistant","id":"msg_1","finish":"stop","content":[{"type":"text","text":"after-unknown"}]},
        {"type":"user","id":"inp_1","text":"hello"}
    ]}"#;
    let mut scripts = session_scripts(&cwd);
    scripts.extend([
        // Promotion reads history for MCP notices before the terminal lookup.
        Script::json(
            "GET",
            "/api/session/ses_1/message?order=desc&limit=50",
            200,
            r#"{"data":[]}"#,
        ),
        Script::sse("GET", "/api/event", &first),
        Script::json(
            "POST",
            "/api/session/ses_1/prompt",
            200,
            r#"{"data":{"id":"inp_1"}}"#,
        ),
        Script::sse("GET", "/api/event", &connected()),
        Script::json(
            "GET",
            "/api/session/ses_1/permission",
            200,
            r#"{"data":[],"hasMore":false}"#,
        ),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json(
            "GET",
            "/api/session/ses_1/message?order=desc&limit=50",
            200,
            history,
        ),
    ]);
    let (response, frames, _requests) = prompt(scripts, &cwd).await?;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    assert!(
        serde_json::to_string(&frames)?.contains("after-unknown"),
        "the frame after the unknown kind was lost"
    );
    Ok(())
}

#[tokio::test]
async fn a_scoped_skill_hint_does_not_end_the_session_stream() -> TestResult {
    let hint = frame(&serde_json::json!({
        "type":"skills.changed","id":"evt_skills","created":1.0,"data":{},
        "location":{"directory":"/repo","workspaceID":"workspace"}
    }));
    let delta = frame(&serde_json::json!({
        "type":"session.text.delta","id":"evt_text","created":1.0,
        "data":{"sessionID":"ses_1","assistantMessageID":"msg_1","ordinal":0,"delta":"after-hint"}
    }));
    let (endpoint, server) = spawn_paced_stream(
        vec![(Duration::ZERO, hint), (Duration::ZERO, delta)],
        Duration::from_secs(1),
    )
    .await?;
    let mut events = client(&endpoint)?.events().await?;
    assert_eq!(events.next().await?.kind(), "server.connected");
    assert_eq!(events.next().await?.kind(), "session.text.delta");
    assert_eq!(events.skipped_kinds(), 1);
    server.abort();

    for location in [
        serde_json::Value::Null,
        serde_json::json!({}),
        serde_json::json!({"directory":""}),
        serde_json::json!({"directory":"relative"}),
        serde_json::json!({"directory":"x".repeat(65_537)}),
        serde_json::json!({"directory":"/repo","workspaceID":null}),
        serde_json::json!({"directory":"/repo","extra":true}),
    ] {
        let malformed = frame(&serde_json::json!({
            "type":"websearch.updated","id":"evt_known","created":1.0,"data":{},
            "location":location
        }));
        assert!(grapt_cli::AdvisoryEvent::decode_sse(
            malformed.as_bytes(),
            CliLimits::production()
        )
        .is_err());
    }
    Ok(())
}

/// Review of d0871ae6: a stream of nothing this client knows must
/// not hold a turn. Each skipped frame refreshes the idle allowance, so the
/// stream ends after a bounded run of unknown frames, at once, rather than
/// waiting out its idle allowance (or forever, while they keep coming).
#[tokio::test(start_paused = true)]
async fn a_stream_of_only_unknown_kinds_ends_after_a_bounded_run() -> TestResult {
    let unknown = frame(&serde_json::json!({
        "type":"session.future.notice","id":"evt_future","created":1.0,
        "data":{"sessionID":"ses_1"}
    }));
    let paced = (0..400)
        .map(|_| (Duration::ZERO, unknown.clone()))
        .collect();
    let (endpoint, server) = spawn_paced_stream(paced, Duration::from_secs(600)).await?;
    let mut events = client(&endpoint)?.events().await?;
    assert_eq!(events.next().await?.kind(), "server.connected");
    let began = tokio::time::Instant::now();
    let ended = events.next().await;
    assert!(
        matches!(ended, Err(grapt_cli::ApiClientError::EventClosed)),
        "unknown frames held the stream: {ended:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "the stream waited out its idle allowance instead: {:?}",
        began.elapsed()
    );
    assert!(events.skipped_kinds() >= 256);
    server.abort();
    Ok(())
}
