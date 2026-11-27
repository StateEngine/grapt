//! A steer admitted while a tool awaits approval is the current input of
//! the chain's next eligible request — exactly once, after the tool
//! exchange — on both request paths (SC-REL-STEER-01).
use super::*;

/// The maintainer preset allows ordinary shell work and asks only for the
/// destructive: a forced recursive delete is the ask this witness needs.
const COMMAND: &str = "rm -rf steer_probe_dir";
const STEER: &str = "STEER_APPROVAL_BOUNDARY_7QX";
const ANSWER: &str = "STEER_APPROVAL_ANSWERED";

#[test]
fn steer_admitted_during_tool_approval_is_current_in_the_next_request() {
    steer_during_approval("chatgpt", false);
    steer_during_approval("anthropic", true);
}

fn message_text(value: &serde_json::Value) -> String {
    match &value["content"] {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn steer_during_approval(label: &str, anthropic: bool) {
    let _journey = process_journey_custody();
    let root = root(&format!("steer-approval-{label}"));
    prepare(&root, 150_000);
    if anthropic {
        context_quality::configure_anthropic(&root, 200_000);
    }
    fs::create_dir_all(root.join("steer_probe_dir"))
        .unwrap_or_else(|error| panic!("probe dir creation failed: {error}"));
    fs::write(root.join("steer_probe_dir/seed.txt"), "probe payload")
        .unwrap_or_else(|error| panic!("probe seed write failed: {error}"));
    let call = if anthropic {
        "toolu_steer_probe"
    } else {
        "call_steer_probe"
    };
    let provider = ProviderFixture::start(if anthropic {
        vec![
            Script::Raw(context_quality::anthropic_stream(
                &[serde_json::json!({
                    "type": "tool_use",
                    "id": call,
                    "name": "shell",
                    "input": {"command": COMMAND},
                })],
                "tool_use",
                serde_json::json!({"input_tokens": 10}),
            )),
            context_quality::anthropic_text(ANSWER, serde_json::json!({"input_tokens": 20})),
        ]
    } else {
        vec![
            Script::ToolShell {
                call,
                command: COMMAND,
            },
            Script::SuccessText(ANSWER),
        ]
    });
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    if anthropic {
        context_quality::select_anthropic(address, &session);
    }
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_steer_probe","text":"run the probe command","delivery":"queue","resume":true}).to_string(),
    );
    // The steer joins only while the tool's ask is genuinely pending: a
    // steer sent after approval would be a later boundary, not this one.
    let asked = permission_residuals::wait_until(
        Duration::from_secs(10),
        permission_residuals::SETTLE_POLL,
        || {
            let page = json(
                address,
                "GET",
                &format!("/api/session/{session}/permission"),
                "",
            );
            page["data"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["source"]["callID"] == call))
        },
    );
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/permission"),
        "",
    );
    let active = json(address, "GET", "/api/session/active", "");
    let seen = locked(&provider.bodies).len();
    assert!(
        asked.is_some(),
        "the shell call must await approval before the steer joins; permission={page} active={active} bodies={seen}"
    );
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_steer_mid_approval","text":STEER,"delivery":"steer","resume":true}).to_string(),
    );
    compaction_continuity::approve(address, &session, call);
    permission_residuals::wait_session_idle(address, &session);
    manual_compaction::assert_carried(&root, "msg_steer_probe", "session.execution.succeeded.1");

    let bodies = locked(&provider.bodies).clone();
    assert_eq!(
        bodies.len(),
        2,
        "the steer continues in the chain's own next Step"
    );
    assert!(
        !bodies[0].contains(STEER),
        "the steer was not sent when the first request went out"
    );
    assert_eq!(
        bodies[1].matches(STEER).count(),
        1,
        "the steer appears exactly once in the request it was admitted for"
    );
    let next = serde_json::from_str::<serde_json::Value>(&bodies[1])
        .unwrap_or_else(|error| panic!("next request decode: {error}"));
    if anthropic {
        let messages = next["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("anthropic request lost its messages: {next}"));
        // Consecutive user rows merge into one message: the steer may ride
        // the tool result's own user message, after the result block.
        let mut tool_result = None;
        let mut steer = None;
        for (message_index, message) in messages.iter().enumerate() {
            let Some(blocks) = message["content"].as_array() else {
                continue;
            };
            for (block_index, block) in blocks.iter().enumerate() {
                if block["type"] == "tool_result" && block["tool_use_id"] == call {
                    tool_result = Some((message_index, block_index));
                }
                if block
                    .get("text")
                    .is_some_and(|text| text.as_str().is_some_and(|text| text.contains(STEER)))
                {
                    steer = Some((message_index, block_index));
                }
            }
        }
        let (Some(tool_result), Some(steer)) = (tool_result, steer) else {
            panic!("steer request lost its tool result or steer text: {next}")
        };
        assert!(
            steer > tool_result,
            "the steer must sit after the tool exchange: {next}"
        );
    } else {
        let items = next["input"]
            .as_array()
            .unwrap_or_else(|| panic!("chatgpt request lost its input: {next}"));
        let tool_output = items
            .iter()
            .position(|item| item["type"] == "function_call_output" && item["call_id"] == call);
        let steer = items
            .iter()
            .position(|item| item["role"] == "user" && message_text(item).contains(STEER));
        let (Some(tool_output), Some(steer)) = (tool_output, steer) else {
            panic!("steer request lost its tool output or steer message: {next}")
        };
        assert!(
            steer > tool_output,
            "the steer must sit after the tool exchange: {next}"
        );
    }

    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("steer boundary store: {error}"));
    let promoted: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.input.promoted.1'
               AND json_extract(data, '$.inputID') = ?2",
            (session.as_str(), "msg_steer_mid_approval"),
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("steer promotion count: {error}"));
    assert_eq!(promoted, 1, "the steer promotes exactly once");
    let pending: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM session_pending WHERE session_id = ?1 AND id = ?2",
            (session.as_str(), "msg_steer_mid_approval"),
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("steer pending count: {error}"));
    assert_eq!(pending, 0, "the admitted steer leaves no pending input");

    drop(child);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    process_fixture::cleanup(&root);
}
