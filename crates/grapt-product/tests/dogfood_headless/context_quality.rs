//! CONTEXT-QUALITY-01 J01: Anthropic prompt-cache markers on the ordinary
//! product path, captured as the exact bodies the loopback provider received.
//!
//! The markers and the usage below are wire and reporting evidence only. A
//! scripted provider cannot establish a real cache hit or a saving.

use super::*;

const MARKER: &str = "cache_control";

/// A secondary Anthropic Messages account beside the fixture's primary one.
pub(super) fn configure_anthropic(root: &Path, context_window: u64) {
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("config read failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("config decode failed: {error}"));
    config["provider"]["accounts"] = serde_json::json!([{
        "id": "anthropic", "route": "route-anthropic", "account": "account-anthropic",
        "credential_version": "credential-anthropic-v1", "policy_version": "live-policy-v1",
        "protocol_family": "anthropic-messages", "endpoint": "https://api.anthropic.com/v1",
        "credential": "anthropic.credential", "discovery": "anthropic-account-models.json"
    }]);
    let write = |name: &str, value: &serde_json::Value| {
        fs::write(
            root.join(name),
            serde_json::to_vec(value).unwrap_or_else(|error| panic!("{name} encode: {error}")),
        )
        .unwrap_or_else(|error| panic!("{name} write failed: {error}"));
    };
    write("grapt.jsonc", &config);
    fs::write(root.join("anthropic.credential"), "anthropic-secret")
        .unwrap_or_else(|error| panic!("credential write failed: {error}"));
    let provider = |id: &str, model: &str, npm: &str, family: &str| {
        let mut models = serde_json::Map::new();
        models.insert(
            model.to_owned(),
            serde_json::json!({"id": model, "name": model, "family": family, "tool_call": true,
                "modalities": {"input": ["text"], "output": ["text"]},
                "limit": {"output": 32000}}),
        );
        serde_json::json!({"id": id, "name": id, "npm": npm, "models": models})
    };
    write(
        "model-catalog.json",
        &serde_json::json!({
            "openai": provider("openai", "gpt-live", "@ai-sdk/openai", "gpt-5"),
            "anthropic": provider("anthropic", "claude-live", "@ai-sdk/anthropic", "claude"),
        }),
    );
    write(
        "anthropic-account-models.json",
        &serde_json::json!({"models": [{
            "slug": "claude-live", "visibility": "list",
            "context_window": context_window, "supported_reasoning_levels": []
        }]}),
    );
}

pub(super) fn select_anthropic(address: SocketAddr, session: &str) {
    let (status, response) = request(
        address,
        "POST",
        &format!("/api/session/{session}/model"),
        r#"{"model":{"providerID":"anthropic","id":"claude-live"}}"#,
    );
    assert_eq!(
        status,
        204,
        "selection refused: {}",
        String::from_utf8_lossy(&response)
    );
}

/// One Anthropic Messages event stream: the blocks, then the stop reason.
pub(super) fn anthropic_stream(
    blocks: &[serde_json::Value],
    stop: &str,
    usage: serde_json::Value,
) -> String {
    let mut frames = vec![serde_json::json!({"type": "message_start", "message": {
        "id": "msg_cq", "type": "message", "role": "assistant", "model": "claude-live",
        "content": [], "stop_reason": null, "usage": usage
    }})];
    for (index, block) in blocks.iter().enumerate() {
        let (start, delta) = match block["type"].as_str() {
            Some("thinking") => (
                serde_json::json!({"type": "thinking", "thinking": "", "signature": ""}),
                serde_json::json!({"type": "thinking_delta", "thinking": block["thinking"]}),
            ),
            Some("tool_use") => (
                serde_json::json!({"type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}),
                serde_json::json!({"type": "input_json_delta", "partial_json": block["input"].to_string()}),
            ),
            _ => (
                serde_json::json!({"type": "text", "text": ""}),
                serde_json::json!({"type": "text_delta", "text": block["text"]}),
            ),
        };
        frames.push(serde_json::json!({"type": "content_block_start", "index": index, "content_block": start}));
        frames.push(
            serde_json::json!({"type": "content_block_delta", "index": index, "delta": delta}),
        );
        if block["type"] == "thinking" {
            frames.push(
                serde_json::json!({"type": "content_block_delta", "index": index,
                "delta": {"type": "signature_delta", "signature": block["signature"]}}),
            );
        }
        frames.push(serde_json::json!({"type": "content_block_stop", "index": index}));
    }
    frames.push(serde_json::json!({"type": "message_delta",
        "delta": {"stop_reason": stop}, "usage": {"output_tokens": 3}}));
    frames.push(serde_json::json!({"type": "message_stop"}));
    let mut stream = String::new();
    for frame in frames {
        stream.push_str("data: ");
        stream.push_str(&frame.to_string());
        stream.push_str("\n\n");
    }
    stream
}

pub(super) fn anthropic_read(call: &str, path: &str, usage: serde_json::Value) -> Script {
    Script::Raw(anthropic_stream(
        &[
            serde_json::json!({"type": "tool_use", "id": call, "name": "read", "input": {"path": path}}),
        ],
        "tool_use",
        usage,
    ))
}

pub(super) fn anthropic_text(text: &str, usage: serde_json::Value) -> Script {
    Script::Raw(anthropic_stream(
        &[serde_json::json!({"type": "text", "text": text})],
        "end_turn",
        usage,
    ))
}

/// The body with every marker removed: the content a provider prefix covers.
pub(super) fn unmarked(mut value: serde_json::Value) -> serde_json::Value {
    match &mut value {
        serde_json::Value::Object(object) => {
            object.remove("cache_control");
            for child in object.values_mut() {
                *child = unmarked(child.take());
            }
        }
        serde_json::Value::Array(items) => {
            for child in items.iter_mut() {
                *child = unmarked(child.take());
            }
        }
        _ => {}
    }
    value
}

fn find_key<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match value {
        serde_json::Value::Object(object) => object
            .get(key)
            .or_else(|| object.values().find_map(|child| find_key(child, key))),
        serde_json::Value::Array(items) => items.iter().find_map(|child| find_key(child, key)),
        _ => None,
    }
}

fn assistants(messages: &serde_json::Value) -> Vec<serde_json::Value> {
    messages["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["type"] == "assistant")
        .cloned()
        .collect()
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn cq_anthropic_coding_chain_marks_a_stable_growing_prefix_and_reports_cache_usage(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("cq-anthropic-cache");
    prepare(&root, 120_000);
    configure_anthropic(&root, 200_000);
    fs::write(root.join("cq-a.txt"), "alpha contents")?;
    fs::write(root.join("cq-b.txt"), "beta contents")?;
    fs::write(root.join("cq-c.txt"), "gamma contents")?;
    let provider = ProviderFixture::start(vec![
        // Retryable before any visible output: the retry must resend the
        // identical body, so a provider prefix cached by the first send stays valid.
        Script::Raw(
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}\n\n"
                .to_owned(),
        ),
        anthropic_read(
            "toolu_cq_a",
            "cq-a.txt",
            serde_json::json!({"input_tokens": 20,
                "cache_creation_input_tokens": 1500, "cache_read_input_tokens": 0}),
        ),
        anthropic_read(
            "toolu_cq_b",
            "cq-b.txt",
            serde_json::json!({"input_tokens": 10,
                "cache_creation_input_tokens": 60, "cache_read_input_tokens": 1500}),
        ),
        anthropic_read(
            "toolu_cq_c",
            "cq-c.txt",
            serde_json::json!({"input_tokens": 40,
                "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}),
        ),
        anthropic_text("Both files read.", serde_json::json!({"input_tokens": 30})),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    select_anthropic(address, &session);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_cq_cache","text":"read both files","delivery":"queue","resume":true}"#,
    );
    for call in ["toolu_cq_a", "toolu_cq_b", "toolu_cq_c"] {
        super::compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 5);

    let mut heads = locked(&provider.heads).clone();
    let mut raw = locked(&provider.bodies).clone();
    assert_eq!(raw[0], raw[1], "a retry changed the request bytes");
    heads.remove(0);
    raw.remove(0);
    let bodies = raw
        .iter()
        .map(|body| serde_json::from_str::<serde_json::Value>(body))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(bodies.len(), 4);
    for (index, (head, body)) in heads.iter().zip(&bodies).enumerate() {
        assert!(head.starts_with("POST /messages "), "{head}");
        assert_eq!(body["model"], "claude-live");
        assert!(body.get("prompt_cache_key").is_none());
        let text = body.to_string();
        assert_eq!(
            text.matches(MARKER).count(),
            if index == 0 { 3 } else { 4 },
            "{text}"
        );
        let tools = body["tools"].as_array().ok_or("tools")?;
        let tool_marker = &tools.last().ok_or("tool")?["cache_control"]["type"];
        assert_eq!(tool_marker, "ephemeral");
        assert_eq!(tools.last().ok_or("tool")?["cache_control"]["ttl"], "1h");
        assert!(tools[..tools.len() - 1]
            .iter()
            .all(|tool| tool.get("cache_control").is_none()));
        let system = body["system"].as_array().ok_or("system blocks")?;
        assert_eq!(system.len(), 2);
        assert_eq!(
            system[0]["cache_control"],
            serde_json::json!({"type":"ephemeral","ttl":"1h"})
        );
        assert!(system[1].get("cache_control").is_none());
        let messages = body["messages"].as_array().ok_or("messages")?;
        let tail = messages
            .last()
            .and_then(|message| message["content"].as_array())
            .and_then(|content| content.last())
            .ok_or("tail block")?;
        assert_eq!(tail["cache_control"]["type"], "ephemeral");
        if let Some(previous) = index.checked_sub(1).and_then(|index| bodies.get(index)) {
            let previous_messages = previous["messages"].as_array().ok_or("previous messages")?;
            let anchor = messages
                .get(previous_messages.len() - 1)
                .and_then(|message| message["content"].as_array())
                .and_then(|content| content.last())
                .ok_or("previous input anchor")?;
            assert_eq!(anchor["cache_control"]["type"], "ephemeral");
        }
    }
    // Tools and system instructions retain the same content across the chain, and
    // every earlier request's messages are a prefix of the next request's.
    for pair in bodies.windows(2) {
        assert_eq!(
            unmarked(pair[0]["tools"].clone()),
            unmarked(pair[1]["tools"].clone())
        );
        assert_eq!(pair[0]["system"].to_string(), pair[1]["system"].to_string());
        let before = unmarked(pair[0]["messages"].clone());
        let after = unmarked(pair[1]["messages"].clone());
        let before = before.as_array().ok_or("before")?;
        let after = after.as_array().ok_or("after")?;
        assert!(after.len() > before.len());
        assert_eq!(after[..before.len()], before[..]);
    }

    // Provider-reported cache reads/writes reach the public message usage; a
    // reported zero stays zero and an absent cache report stays absent. The
    // retried first Step's usage is its successful attempt's: that attempt
    // only called a tool, and its tool terminal binds it to the Step message.
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc"),
        "",
    );
    let usage = assistants(&messages)
        .iter()
        .map(|row| row["observedUsage"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        usage,
        vec![
            serde_json::json!({"inputTotal": 1520, "inputFresh": 20,
                "cacheRead": 0, "cacheWrite": 1500, "output": 3}),
            serde_json::json!({"inputTotal": 1570, "inputFresh": 10,
                "cacheRead": 1500, "cacheWrite": 60, "output": 3}),
            serde_json::json!({"inputTotal": 40, "inputFresh": 40,
                "cacheRead": 0, "cacheWrite": 0, "output": 3}),
            serde_json::json!({"inputFresh": 30, "output": 3}),
        ],
        "{messages}"
    );
    child.stop_after_witness(&root);
    drop(child);
    let store = EventStore::open(&root.join("database.sqlite3"), "cq-anthropic-usage")?;
    let mut statement = store
        .connection()
        .prepare("SELECT data FROM event WHERE type = 'session.usage.observed.2' ORDER BY seq")?;
    let observed = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(observed.len(), 4, "{observed:?}");
    let retried: serde_json::Value = serde_json::from_str(&observed[0])?;
    for (key, count, provenance) in [
        ("cacheWriteInputTokens", 1500, "provider-reported"),
        ("cacheReadInputTokens", 0, "provider-reported"),
        ("inputFreshTokens", 20, "provider-reported"),
        ("inputTotalTokens", 1520, "provider-derived"),
    ] {
        let field = find_key(&retried, key).ok_or_else(|| format!("{key} missing: {retried}"))?;
        assert_eq!(field["count"], count, "{key}: {retried}");
        assert_eq!(field["provenance"], provenance, "{key}: {retried}");
    }
    drop(statement);
    drop(store);
    process_fixture::cleanup(&root);
    Ok(())
}

// ---------------------------------------------------------------------------
// J03: a repository task across two compactions, a delayed correction and a
// restart. The scripted model cannot show semantic quality; this witnesses
// that the harness, not the summary, carries the user's own words and the
// completed effect into every later request, identically after a restart.
// ---------------------------------------------------------------------------

const LARGE_READ: usize = 36_000;
const FIRST_READS: [(&str, &str); 8] = [
    ("call_cq_first_1", "cq-1.txt"),
    ("call_cq_first_2", "cq-2.txt"),
    ("call_cq_first_3", "cq-3.txt"),
    ("call_cq_first_4", "cq-4.txt"),
    ("call_cq_first_5", "cq-5.txt"),
    ("call_cq_first_6", "cq-6.txt"),
    ("call_cq_first_7", "cq-7.txt"),
    ("call_cq_first_8", "cq-8.txt"),
];
const SECOND_READS: [(&str, &str); 8] = [
    ("call_cq_second_1", "cq-1.txt"),
    ("call_cq_second_2", "cq-2.txt"),
    ("call_cq_second_3", "cq-3.txt"),
    ("call_cq_second_4", "cq-4.txt"),
    ("call_cq_second_5", "cq-5.txt"),
    ("call_cq_second_6", "cq-6.txt"),
    ("call_cq_second_7", "cq-7.txt"),
    ("call_cq_second_8", "cq-8.txt"),
];
const OBJECTIVE: &str =
    "Rename alpha to beta in src/lib.rs and src/util.rs. Never modify NOTES.md.";
const CORRECTION: &str =
    "Correction: rename alpha to gamma instead of beta. NOTES.md stays untouched.";
const EDIT_ARGUMENTS: &str = r#"{"path":"src/lib.rs","old_text":"alpha","new_text":"beta"}"#;

/// One Responses function call with exact argument text.
fn responses_call(call: &str, tool: &str, arguments: &str) -> Script {
    let encoded = serde_json::to_string(arguments)
        .unwrap_or_else(|error| panic!("arguments encoding failed: {error}"));
    Script::Raw(format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"{tool}\"}}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{encoded}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n"
    ))
}

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    let body = serde_json::json!({"id": id, "text": text, "delivery": "queue", "resume": true});
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &body.to_string(),
    );
}

/// The deterministic settled-tool ledger in a captured Responses request.
fn ledger(body: &serde_json::Value) -> Option<String> {
    body["input"].as_array()?.iter().find_map(|item| {
        let content = item["content"].as_str()?;
        (item["role"] == "user" && content.starts_with("[Harness-retained settled tool calls"))
            .then(|| content.to_owned())
    })
}

/// Original user messages precede the labelled continuation summary. The
/// current input and exact tail follow it and are not part of this prefix.
fn originals(body: &serde_json::Value) -> Option<Vec<String>> {
    let items = body["input"].as_array()?;
    let end = items.iter().position(|item| {
        item["role"] == "assistant"
            && item["content"]
                .as_str()
                .is_some_and(|content| content.starts_with("[Model-written continuation summary."))
    })?;
    Some(
        items[..end]
            .iter()
            .filter_map(|item| {
                let content = item["content"].as_str()?;
                (item["role"] == "user" && !content.starts_with("[Harness-retained"))
                    .then(|| content.to_owned())
            })
            .collect(),
    )
}

/// When `CQ_EVALUATION_TRACE` names a file, write this run's exact request
/// bodies, per-message usage provenance and task outcome to it: the
/// deterministic result of the bounded evaluation task in
/// `docs/context-quality/EVALUATION.md`.
fn export_evaluation_trace(
    root: &Path,
    address: SocketAddr,
    session: &str,
    bodies: &[serde_json::Value],
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(target) = std::env::var_os("CQ_EVALUATION_TRACE") else {
        return Ok(());
    };
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc"),
        "",
    );
    let usage = messages["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["type"] == "assistant")
        .map(|row| serde_json::json!({"id": row["id"], "observedUsage": row["observedUsage"]}))
        .collect::<Vec<_>>();
    let trace = serde_json::json!({
        "schema": "context-quality-evaluation-trace/v1",
        "task": "cq-repository-task",
        "provider": "loopback scripted Responses fixture; usage values are scripted, not measured",
        "requests": bodies,
        "usage": usage,
        "outcome": {
            "src/lib.rs": fs::read_to_string(root.join("src/lib.rs"))?,
            "src/util.rs": fs::read_to_string(root.join("src/util.rs"))?,
            "NOTES.md": fs::read_to_string(root.join("NOTES.md"))?,
            "compactions": super::compaction_continuity::compactions(&messages).len(),
        },
    });
    let encoded = serde_json::to_vec_pretty(&trace)?;
    assert!(
        encoded.len() <= 8 * 1024 * 1024,
        "evaluation trace exceeds its bound"
    );
    fs::write(target, encoded)?;
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn cq_repository_task_keeps_user_words_and_effects_across_compactions_and_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("cq-repository-task");
    prepare(&root, 500_000);
    prepare_model_inventory(&root, 128_000);
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n")?;
    fs::write(root.join("src/util.rs"), "pub use crate::alpha;\n")?;
    fs::write(root.join("NOTES.md"), "release notes\n")?;
    for (_, path) in FIRST_READS {
        fs::write(root.join(path), text_lines(b'x', LARGE_READ))?;
    }
    let provider = ProviderFixture::start(vec![
        Script::ToolReads {
            calls: FIRST_READS.to_vec(),
        },
        Script::Output(5_645),
        responses_call("call_cq_edit", "edit", EDIT_ARGUMENTS),
        Script::SuccessText("renamed alpha to beta in src/lib.rs"),
        Script::ToolReads {
            calls: SECOND_READS.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("switching to gamma"),
        Script::SuccessText("status before restart"),
        Script::SuccessText("status after restart"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);

    prompt(address, &session, "msg_cq_objective", OBJECTIVE);
    let calls: Vec<_> = FIRST_READS.iter().map(|(call, _)| *call).collect();
    super::compaction_continuity::approve_batch(address, &session, &calls);
    super::compaction_continuity::approve(address, &session, "call_cq_edit");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        fs::read_to_string(root.join("src/lib.rs"))?,
        "pub fn beta() {}\n"
    );

    prompt(address, &session, "msg_cq_correction", CORRECTION);
    let calls: Vec<_> = SECOND_READS.iter().map(|(call, _)| *call).collect();
    super::compaction_continuity::approve_batch(address, &session, &calls);
    permission_residuals::wait_session_idle(address, &session);
    prompt(address, &session, "msg_cq_before_restart", "Report status.");
    permission_residuals::wait_session_idle(address, &session);
    child.stop_after_witness(&root);
    drop(child);

    let mut restarted = spawn(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    prompt(
        address,
        &session,
        "msg_cq_after_restart",
        "Report status again.",
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 9);

    let bodies = locked(&provider.bodies)
        .iter()
        .map(|body| serde_json::from_str::<serde_json::Value>(body))
        .collect::<Result<Vec<_>, _>>()?;
    let summaries = bodies
        .iter()
        .filter(|body| {
            body.to_string()
                .contains("Output exactly the Markdown structure shown inside <template>")
        })
        .count();
    assert_eq!(summaries, 2, "the task did not compact twice");

    // J01 across compaction and restart: coding requests keep one tool list and
    // one session cache key, so the cacheable tool prefix is not invalidated by
    // either; the one-shot summary requests carry no key.
    let coding = bodies
        .iter()
        .filter(|body| {
            !body
                .to_string()
                .contains("Output exactly the Markdown structure shown inside <template>")
        })
        .collect::<Vec<_>>();
    assert_eq!(coding.len(), 7);
    for body in &coding {
        assert_eq!(body["tools"].to_string(), coding[0]["tools"].to_string());
        assert_eq!(body["prompt_cache_key"], coding[0]["prompt_cache_key"]);
        assert!(body["prompt_cache_key"].is_string());
    }
    for body in bodies.iter().filter(|body| {
        body.to_string()
            .contains("Output exactly the Markdown structure shown inside <template>")
    }) {
        assert!(body.get("prompt_cache_key").is_none());
    }

    // Every coding request after the first compaction carries the facts; the
    // objective's exact words survive both summaries.
    for (index, body) in bodies.iter().enumerate().skip(2) {
        if body
            .to_string()
            .contains("Output exactly the Markdown structure shown inside <template>")
        {
            continue;
        }
        let retained = originals(body)
            .ok_or_else(|| format!("request {index} lost its original-user prefix"))?;
        assert!(
            body["input"].as_array().is_some_and(|items| items
                .iter()
                .any(|item| item["role"] == "user" && item["content"] == OBJECTIVE)),
            "request {index} lost the objective's exact original words: {retained:?}"
        );
    }
    let before = &bodies[7];
    let after = &bodies[8];
    let retained = originals(after).ok_or("the post-restart request lost its originals")?;
    assert_eq!(
        originals(before),
        Some(retained.clone()),
        "restart changed original-user prefix"
    );
    assert!(retained.iter().any(|content| content == OBJECTIVE));
    let effects = ledger(after).ok_or("the post-restart request lost its tool ledger")?;
    assert_eq!(
        ledger(before),
        Some(effects.clone()),
        "restart changed the tool ledger"
    );
    assert!(
        effects.contains(&format!("- edit {EDIT_ARGUMENTS} -> completed")),
        "the completed edit is not recorded: {effects}"
    );
    assert!(
        after["input"].as_array().is_some_and(|items| items
            .iter()
            .any(|item| item["role"] == "user" && item["content"] == CORRECTION)),
        "the delayed correction is not verbatim in the request"
    );
    let text = after.to_string();
    let (Some(objective_at), Some(correction_at)) =
        (text.find(&OBJECTIVE[..24]), text.find(&CORRECTION[..24]))
    else {
        return Err("the objective or its correction is missing".into());
    };
    assert!(
        objective_at < correction_at,
        "the correction does not follow the objective it supersedes"
    );

    // The effect happened once and nothing replayed it.
    assert_eq!(
        fs::read_to_string(root.join("src/lib.rs"))?,
        "pub fn beta() {}\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("NOTES.md"))?,
        "release notes\n"
    );
    export_evaluation_trace(&root, address, &session, &bodies)?;
    restarted.stop_after_witness(&root);
    drop(restarted);
    process_fixture::cleanup(&root);
    Ok(())
}

// ---------------------------------------------------------------------------
// J04: the historic reread loop (context-reliability p0/p1-retention-3). A
// read too large for the working context forces a compaction before the model
// sees it; the model reads the same file again. Before this campaign the
// product compacted again for every repeat until an outer bound. Now a repeat
// is never summarized again: it goes out uncompacted
// while it fits the model window, and stops as a typed, actionable context
// failure, with history kept, only once it no longer does.
// ---------------------------------------------------------------------------

const SUMMARY_REQUEST: &str = "Output exactly the Markdown structure shown inside <template>";

/// The `session.step.failed` reasons the Session recorded, in order.
fn step_failures(root: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("store open failed: {error}"));
    db.prepare(
        "SELECT json_extract(data, '$.reason.failure') FROM event
         WHERE type LIKE 'session.step.failed%' ORDER BY seq",
    )
    .and_then(|mut query| {
        query
            .query_map([], |row| row.get(0))
            .and_then(|rows| rows.collect())
    })
    .unwrap_or_else(|error| panic!("step failures unreadable: {error}"))
}

#[test]
fn cq_identical_reread_that_no_longer_fits_stops_without_another_summary(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("cq-reread-loop");
    prepare(&root, 500_000);
    // 48_000 - 16_384 - 4_096 leaves 27_520 byte-estimated units: one read of
    // the task file cannot fit, exactly as in the historic tight-budget trials,
    // while the rebuilt request, which carries the whole tool catalog, still
    // does (at 45_000 the terminal tools put it over).
    prepare_model_inventory(&root, 48_000);
    fs::write(root.join("task.txt"), text_lines(b't', LARGE_READ))?;
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_cq_loop_1",
            path: "task.txt",
        },
        Script::Output(2_000),
        Script::ToolRead {
            call: "call_cq_loop_2",
            path: "task.txt",
        },
        // Only reachable if the repeat compacts again.
        Script::Output(2_000),
        Script::ToolRead {
            call: "call_cq_loop_3",
            path: "task.txt",
        },
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt(
        address,
        &session,
        "msg_cq_loop",
        "Report what task.txt asks for.",
    );
    for call in ["call_cq_loop_1", "call_cq_loop_2"] {
        super::compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);

    // Two rounds bound the turn at 2 x 2 + 1 calls; a compact loop would
    // show as more.
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "the repeated read was compacted again instead of stopping"
    );
    let bodies = locked(&provider.bodies).clone();
    assert!(bodies[1].contains(SUMMARY_REQUEST));
    assert_eq!(
        bodies
            .iter()
            .filter(|body| body.contains(SUMMARY_REQUEST))
            .count(),
        1,
        "the repeat spent a summary call"
    );
    // The model saw the deterministic record of the oversized result before
    // it repeated the read.
    let second: serde_json::Value = serde_json::from_str(&bodies[2])?;
    let retained = ledger(&second).ok_or("the rebuilt request has no settled-tool ledger")?;
    assert!(
        retained.contains(r#"- read {"path":"task.txt"} -> completed, "#),
        "{retained}"
    );
    let path = format!("/api/session/{session}/message");
    let page = json(address, "GET", &path, "");
    let rendered = page.to_string();
    // The execution failure keeps the standard context-limit class every
    // build reads; beside it, a notice says what happened and what to do.
    assert!(
        rendered.contains("\"executionFailure\":true")
            && rendered.contains("its context could not be prepared or reduced for this model"),
        "{rendered}"
    );
    let rows = page["data"].as_array().ok_or("history page")?;
    let notice = rows
        .iter()
        .find(|row| row["metadata"]["contextRepeated"] == true)
        .ok_or_else(|| format!("no context-repeated notice: {rendered}"))?;
    let text = notice["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("kept re-reading content it had already summarized")
            && text.contains("Start a new session, or narrow the task"),
        "{text}"
    );
    assert_eq!(
        super::compaction_continuity::compactions(&page).len(),
        1,
        "history lost or duplicated the one committed compaction"
    );
    child.stop_after_witness(&root);
    drop(child);
    // Written as the no-progress it is, so an older build still reads the
    // Session; the detail names the repetition.
    assert_eq!(step_failures(&root), ["compaction-no-progress"]);
    process_fixture::cleanup(&root);
    Ok(())
}

/// The other branch: a repeat that still fits the model window goes out
/// uncompacted, with no summary call, and the turn answers.
#[test]
fn cq_identical_reread_that_still_fits_goes_out_uncompacted(
) -> Result<(), Box<dyn std::error::Error>> {
    const READS: [(&str, &str); 2] = [("call_cq_fit_a", "a.txt"), ("call_cq_fit_b", "b.txt")];
    const REREADS: [(&str, &str); 2] = [("call_cq_fit_c", "a.txt"), ("call_cq_fit_d", "b.txt")];
    let _journey = process_journey_custody();
    let root = root("cq-reread-fits");
    prepare(&root, 500_000);
    prepare_model_inventory(&root, 128_000);
    for (_, file) in READS {
        fs::write(root.join(file), text_lines(b'f', LARGE_READ))?;
    }
    let provider = ProviderFixture::start(vec![
        Script::ToolReads {
            calls: READS.to_vec(),
        },
        Script::ToolReads {
            calls: REREADS.to_vec(),
        },
        Script::SuccessText("answered from the repeated reads"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt(
        address,
        &session,
        "msg_cq_fit",
        "Report what a.txt and b.txt say.",
    );
    for window in [&READS, &REREADS] {
        let calls: Vec<_> = window.iter().map(|(call, _)| *call).collect();
        super::compaction_continuity::approve_batch(address, &session, &calls);
    }
    permission_residuals::wait_session_idle(address, &session);

    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 3, "reads, rereads, answer");
    assert_eq!(
        bodies
            .iter()
            .filter(|body| body.contains(SUMMARY_REQUEST))
            .count(),
        0,
        "the repeat spent a summary call"
    );
    // The repeat's results went out as they are.
    assert!(
        bodies[2].contains(&"f".repeat(99)),
        "the repeated reads were not sent uncompacted"
    );
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    let rendered = page.to_string();
    assert!(
        rendered.contains("answered from the repeated reads")
            && !rendered.contains("\"executionFailure\":true"),
        "{rendered}"
    );
    child.stop_after_witness(&root);
    drop(child);
    assert!(step_failures(&root).is_empty(), "a Step failed");
    process_fixture::cleanup(&root);
    Ok(())
}

// ---------------------------------------------------------------------------
// J02 on the product path: with plausible provider usage (four request bytes
// per token), grouped reads that the byte bound would compact fit the
// calibrated count, so the next request goes out uncompacted. The model's
// advertised 80_480 stays the maximum; only the estimate of new bytes changes.
// ---------------------------------------------------------------------------

#[test]
fn cq_confirmed_usage_calibrates_admission_so_fitting_reads_are_not_compacted(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("cq-calibrated-admission");
    prepare(&root, 120_000);
    // Operating threshold 80_480 - 16_384 - 4_096 = 60_000 estimated units.
    prepare_model_inventory(&root, 80_480);
    let reads = vec![
        ("call_cq_fit_1", "cq-fit-1.txt"),
        ("call_cq_fit_2", "cq-fit-2.txt"),
    ];
    for (_, path) in &reads {
        fs::write(root.join(path), text_lines(b'f', LARGE_READ))?;
    }
    let provider = ProviderFixture::start(vec![
        Script::ToolReadsMeasured {
            calls: reads.clone(),
            bytes_per_token: 4,
        },
        Script::SuccessText("both reads fit"),
        Script::SuccessText("the next prompt fit too"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt(
        address,
        &session,
        "msg_cq_fit",
        "Compare the two fit files.",
    );
    let calls: Vec<_> = reads.iter().map(|(call, _)| *call).collect();
    super::compaction_continuity::approve_batch(address, &session, &calls);
    permission_residuals::wait_session_idle(address, &session);
    // A later prompt is a new execution; the Session's calibration carries
    // into its first request, whose history alone exceeds the byte bound.
    prompt(
        address,
        &session,
        "msg_cq_fit_next",
        "Now list what differs.",
    );
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 3, "a calibrated request was compacted");
    for body in &bodies {
        assert!(!body.contains("Output exactly the Markdown structure shown inside <template>"));
    }
    // The byte bound alone would have compacted: each later request's own
    // bytes already exceed the 60_000-unit operating threshold.
    assert!(bodies[1].len() > 72_000, "{}", bodies[1].len());
    assert!(bodies[2].len() > 72_000, "{}", bodies[2].len());
    let rendered = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )
    .to_string();
    assert!(rendered.contains("both reads fit"), "{rendered}");
    assert!(rendered.contains("the next prompt fit too"), "{rendered}");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
