//! CODING-QUALITY-01 prompt caching on the production openai-compatible
//! route (OpenRouter): an Anthropic model's requests carry the oracle's four
//! cache breakpoints through tool history; any other model's carry none.
//! A scripted provider proves the bytes, never a real cache hit.
use super::*;

const MARKER: &str = r#""cache_control":{"type":"ephemeral"}"#;

/// One compatible stream that calls `read` on `path`.
fn compatible_read(call: &str, path: &str) -> Script {
    let arguments = serde_json::json!({"path": path}).to_string();
    let delta = serde_json::json!({"choices": [{"index": 0, "delta": {"tool_calls": [{
        "index": 0, "id": call, "type": "function",
        "function": {"name": "read", "arguments": arguments}}]}, "finish_reason": null}]});
    Script::Raw(format!(
        "data: {delta}\n\n\
         data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n\
         data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":12,\"completion_tokens\":4,\"total_tokens\":16}}}}\n\n\
         data: [DONE]\n\n"
    ))
}

/// An OpenRouter account offering `model`, as the benchmark's is configured.
pub(super) fn configure_openrouter(root: &Path, model: &str) {
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("config read failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("config decode failed: {error}"));
    config["provider"]["accounts"] = serde_json::json!([{
        "id": "openrouter", "route": "route-openrouter", "account": "account-openrouter",
        "credential_version": "credential-openrouter-v1", "policy_version": "live-policy-v1",
        "protocol_family": "openai-compatible", "endpoint": "https://openrouter.ai/api/v1",
        "credential": "openrouter.credential", "discovery": "openrouter-account-models.json"
    }]);
    let write = |name: &str, value: &serde_json::Value| {
        fs::write(
            root.join(name),
            serde_json::to_vec(value).unwrap_or_else(|error| panic!("{name} encode: {error}")),
        )
        .unwrap_or_else(|error| panic!("{name} write failed: {error}"));
    };
    write("grapt.jsonc", &config);
    fs::write(root.join("openrouter.credential"), "openrouter-secret")
        .unwrap_or_else(|error| panic!("credential write failed: {error}"));
    let provider = |id: &str, model: &str| {
        let mut models = serde_json::Map::new();
        models.insert(
            model.to_owned(),
            serde_json::json!({"id": model, "name": model, "family": "gpt-5", "tool_call": true,
                "modalities": {"input": ["text"], "output": ["text"]}, "limit": {"output": 32000}}),
        );
        serde_json::json!({"id": id, "name": id, "npm": "@ai-sdk/openai", "models": models})
    };
    write(
        "model-catalog.json",
        &serde_json::json!({"openai": provider("openai", "gpt-live"),
            "openrouter": provider("openrouter", model)}),
    );
    write(
        "openrouter-account-models.json",
        &serde_json::json!({"models": [{"slug": model, "visibility": "list",
            "context_window": 128000, "supported_reasoning_levels": []}]}),
    );
}

/// Two user turns on `model`, the first reading a file through a tool.
fn bodies_for(label: &str, model: &str) -> Vec<serde_json::Value> {
    let _journey = process_journey_custody();
    let root = root(label);
    prepare(&root, 120_000);
    configure_openrouter(&root, model);
    fs::write(root.join("a.txt"), "contents of a\n")
        .unwrap_or_else(|error| panic!("read fixture write failed: {error}"));
    let provider = ProviderFixture::start(vec![
        compatible_read("call_compat_a", "a.txt"),
        Script::CompatibleSuccess,
        Script::CompatibleSuccess,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let (status, response) = request(
        address,
        "POST",
        &format!("/api/session/{session}/model"),
        &serde_json::json!({"model": {"providerID": "openrouter", "id": model}}).to_string(),
    );
    assert_eq!(
        status,
        204,
        "selection refused: {}",
        String::from_utf8_lossy(&response)
    );
    for (id, text, call) in [
        ("msg_compat_one", "read a.txt", Some("call_compat_a")),
        ("msg_compat_two", "now answer", None),
    ] {
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id": id, "text": text, "delivery": "queue", "resume": true})
                .to_string(),
        );
        if let Some(call) = call {
            compaction_continuity::approve(address, &session, call);
        }
        permission_residuals::wait_session_idle(address, &session);
    }
    child.stop_after_witness(&root);
    drop(child);
    let bodies = locked(&provider.bodies).clone();
    process_fixture::cleanup(&root);
    assert_eq!(
        bodies.len(),
        3,
        "two turns with one tool call make three requests"
    );
    bodies
        .iter()
        .map(|body| {
            serde_json::from_str(body).unwrap_or_else(|error| panic!("body is not JSON: {error}"))
        })
        .collect()
}

fn marked(message: &serde_json::Value) -> bool {
    message["content"]
        .as_array()
        .is_some_and(|parts| parts.iter().any(|part| part.get("cache_control").is_some()))
}

fn has_text(message: &serde_json::Value) -> bool {
    message["content"]
        .as_str()
        .is_some_and(|text| !text.is_empty())
        || message["content"]
            .as_array()
            .is_some_and(|parts| !parts.is_empty())
}

#[test]
fn an_anthropic_model_on_openrouter_carries_four_breakpoints_through_tool_history() {
    let bodies = bodies_for("compat-cache-anthropic", "anthropic/claude-sonnet-5.5");
    for (index, body) in bodies.iter().enumerate() {
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let system: Vec<usize> = (0..messages.len())
            .filter(|&at| messages[at]["role"] == "system" && has_text(&messages[at]))
            .collect();
        let others: Vec<usize> = (0..messages.len())
            .filter(|&at| messages[at]["role"] != "system" && has_text(&messages[at]))
            .collect();
        let expected: Vec<usize> = system
            .iter()
            .take(2)
            .chain(others.iter().rev().take(2))
            .copied()
            .collect();
        // Four at most; fewer only while the conversation has fewer
        // text-bearing messages (the first request has one user message).
        assert_eq!(
            body.to_string().matches(MARKER).count(),
            expected.len(),
            "request {index} carries the wrong number of markers: {body}"
        );
        assert!(expected.len() <= 4);
        if index > 0 {
            assert_eq!(
                expected.len(),
                4,
                "request {index} has history for all four"
            );
        }
        for (at, message) in messages.iter().enumerate() {
            assert_eq!(
                marked(message),
                expected.contains(&at),
                "request {index} message {at} marking is wrong: {message}"
            );
        }
        assert!(
            messages
                .iter()
                .filter(|message| message.get("tool_calls").is_some())
                .all(|message| !marked(message)),
            "a tool-call message carries a marker in request {index}"
        );
    }
    assert!(
        bodies[1]["messages"]
            .as_array()
            .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool")),
        "the tool output reached the continued request"
    );
}

#[test]
fn a_non_anthropic_model_on_openrouter_carries_no_breakpoint() {
    for body in bodies_for("compat-cache-off", "openai/gpt-6-luna") {
        assert!(!body.to_string().contains("cache_control"), "{body}");
    }
}

/// One text turn on the compatible route whose final usage chunk carries
/// `usage` verbatim.
fn compatible_text(usage: &str) -> Script {
    Script::Raw(format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"ok\"}},\"finish_reason\":null}}]}}\n\n\
         data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
         data: {{\"choices\":[],{usage}}}\n\n\
         data: [DONE]\n\n"
    ))
}

/// One usage leaf of a stored `session.usage.observed.2` row.
fn usage_leaf<'a>(row: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    row.get("usage").and_then(|usage| usage.get(key))
}

/// The assistant rows of a session message listing.
fn assistants(messages: &serde_json::Value) -> Vec<serde_json::Value> {
    messages["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["type"] == "assistant")
        .cloned()
        .collect()
}

/// The OpenRouter chat route reports cache creation in
/// `prompt_tokens_details.cache_write_tokens` (recorded live shape of an
/// anthropic/claude-haiku-5.5 final chunk). A reported write is stored
/// provider-reported and subtracted from fresh input; a usage without the
/// field keeps cache write absent in both the public usage and the durable
/// observation, never an observed zero.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn an_openrouter_usage_reports_cache_writes_and_absence_stays_absent(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("compat-usage-writes");
    prepare(&root, 120_000);
    configure_openrouter(&root, "anthropic/claude-haiku-5.5");
    let with_write = r#""usage":{"prompt_tokens":6315,"prompt_tokens_details":{"cached_tokens":0,"cache_write_tokens":6311,"audio_tokens":0,"video_tokens":0},"completion_tokens":1,"completion_tokens_details":{"reasoning_tokens":0,"image_tokens":0,"audio_tokens":0},"total_tokens":6316}"#;
    let without_write = r#""usage":{"prompt_tokens":4998,"prompt_tokens_details":{"cached_tokens":4488},"completion_tokens":157,"total_tokens":5155}"#;
    let provider = ProviderFixture::start(vec![
        compatible_text(with_write),
        compatible_text(without_write),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let (status, response) = request(
        address,
        "POST",
        &format!("/api/session/{session}/model"),
        &serde_json::json!({"model": {"providerID": "openrouter",
            "id": "anthropic/claude-haiku-5.5"}})
        .to_string(),
    );
    assert_eq!(
        status,
        204,
        "selection refused: {}",
        String::from_utf8_lossy(&response)
    );
    for id in ["msg_compat_usage_one", "msg_compat_usage_two"] {
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id": id, "text": "answer", "delivery": "queue", "resume": true})
                .to_string(),
        );
        permission_residuals::wait_session_idle(address, &session);
    }
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
            serde_json::json!({"inputTotal": 6315, "inputFresh": 4,
                "cacheRead": 0, "cacheWrite": 6311, "reasoning": 0,
                "output": 1, "total": 6316}),
            serde_json::json!({"inputTotal": 4998, "inputFresh": 510,
                "cacheRead": 4488, "output": 157, "total": 5155}),
        ],
        "{messages}"
    );
    child.stop_after_witness(&root);
    drop(child);
    let store = EventStore::open(&root.join("database.sqlite3"), "compat-usage-writes")?;
    let mut statement = store
        .connection()
        .prepare("SELECT data FROM event WHERE type = 'session.usage.observed.2' ORDER BY seq")?;
    let observed = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    drop(store);
    assert_eq!(observed.len(), 2, "{observed:?}");
    let first: serde_json::Value = serde_json::from_str(&observed[0])?;
    let second: serde_json::Value = serde_json::from_str(&observed[1])?;
    assert_eq!(
        usage_leaf(&first, "cacheWriteInputTokens"),
        Some(&serde_json::json!({"count": 6311, "provenance": "provider-reported"}))
    );
    assert_eq!(
        usage_leaf(&first, "inputFreshTokens"),
        Some(
            &serde_json::json!({"count": 4, "provenance": "provider-derived", "rule": "fresh-from-inclusive-input"})
        )
    );
    assert_eq!(
        usage_leaf(&first, "cacheReadInputTokens"),
        Some(&serde_json::json!({"count": 0, "provenance": "provider-reported"}))
    );
    assert_eq!(
        usage_leaf(&second, "cacheWriteInputTokens"),
        None,
        "an absent cache write must stay absent: {second}"
    );
    assert_eq!(
        usage_leaf(&second, "inputFreshTokens"),
        Some(
            &serde_json::json!({"count": 510, "provenance": "provider-derived", "rule": "fresh-from-inclusive-input"})
        )
    );
    assert_eq!(
        usage_leaf(&second, "cacheReadInputTokens"),
        Some(&serde_json::json!({"count": 4488, "provenance": "provider-reported"}))
    );
    process_fixture::cleanup(&root);
    Ok(())
}
