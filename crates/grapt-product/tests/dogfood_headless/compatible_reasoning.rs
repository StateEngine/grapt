//! Declared compatible reasoning contracts through durable continuation and restart.
use super::*;

const FIRST: &str = "first π thought";
const SECOND: &str = "second thought without a call";
const AFTER: &str = "thought after compaction";

fn stream(reason: &str, text: &str, calls: &[(&str, &str)]) -> Script {
    let mut body = String::new();
    let split = reason.char_indices().nth(3).map_or(0, |(index, _)| index);
    for part in [&reason[..split], &reason[split..]] {
        let delta = serde_json::json!({"choices":[{"index":0,"delta":{"reasoning_content":part},"finish_reason":null}]});
        body.push_str(&format!("data: {delta}\n\n"));
    }
    let tools: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(index, (call, path))| {
            serde_json::json!({"index":index,"id":call,"type":"function","function":{
            "name":"read","arguments":serde_json::json!({"path":path}).to_string()}})
        })
        .collect();
    let delta = serde_json::json!({"choices":[{"index":0,"delta":{"content":text,"tool_calls":tools},"finish_reason":null}]});
    let finish = if calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    body.push_str(&format!("data: {delta}\n\n"));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":finish}]})
    ));
    body.push_str("data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":4,\"total_tokens\":16}}\n\ndata: [DONE]\n\n");
    Script::Raw(body)
}

fn configured(root: &Path, catalog: &str) -> Result<(), Box<dyn std::error::Error>> {
    prepare(root, 120_000);
    compatible_cache::configure_openrouter(root, "compat-model");
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["provider"]["accounts"][0]["catalog_provider"] = serde_json::json!(catalog);
    // The address is identical in the declared integration and custom control.
    config["provider"]["accounts"][0]["endpoint"] =
        serde_json::json!("https://api.deepseek.com/v1");
    fs::write(path, serde_json::to_vec(&config)?)?;
    let path = root.join("model-catalog.json");
    let mut inventory: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    inventory[catalog] = inventory["openrouter"].clone();
    inventory[catalog]["id"] = serde_json::json!(catalog);
    fs::write(path, serde_json::to_vec(&inventory)?)?;
    fs::write(root.join("a.txt"), "one")?;
    fs::write(root.join("b.txt"), "two")?;
    Ok(())
}

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
    );
}

fn select(address: SocketAddr, session: &str) {
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/model"),
            r#"{"model":{"providerID":"openrouter","id":"compat-model"}}"#
        )
        .0,
        204
    );
}

fn valid(body: &str, expected: &[(&str, &str)]) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    let Some(messages) = value["messages"].as_array() else {
        return false;
    };
    let complete = messages
        .iter()
        .filter(|message| message["role"] == "assistant")
        .all(|message| {
            [
                ("reading both files", FIRST),
                ("finished first request", SECOND),
                ("finished restarted request", "third thought"),
                ("reading after compaction", AFTER),
            ]
            .iter()
            .all(|(text, reason)| {
                message["content"].as_str() != Some(*text)
                    || message["reasoning_content"].as_str() == Some(*reason)
            })
        });
    complete
        && expected.iter().all(|(marker, reasoning)| {
            let paired = !marker.starts_with("call_")
                || messages.iter().any(|message| {
                    message["role"] == "tool" && message["tool_call_id"].as_str() == Some(*marker)
                });
            paired
                && messages.iter().any(|message| {
                    message["role"] == "assistant"
                        && message.to_string().contains(marker)
                        && message["reasoning_content"].as_str() == Some(*reasoning)
                        && message["tool_calls"].as_array().is_none_or(|calls| {
                            calls.iter().all(|call| {
                                let path = if call["id"] == "call_reason_b" {
                                    "b.txt"
                                } else {
                                    "a.txt"
                                };
                                let arguments = serde_json::json!({"path":path}).to_string();
                                call["function"]["name"] == "read"
                                    && call["function"]["arguments"].as_str()
                                        == Some(arguments.as_str())
                            })
                        })
                })
        })
}

fn rejected() -> Script {
    Script::Refused {
        status: 400,
        body: "missing or changed required reasoning_content",
    }
}

fn after_calls(body: &str) -> Script {
    if valid(body, &[("call_reason_a", FIRST), ("call_reason_b", FIRST)]) {
        stream(SECOND, "finished first request", &[])
    } else {
        rejected()
    }
}

fn after_restart(body: &str) -> Script {
    if valid(
        body,
        &[("call_reason_a", FIRST), ("finished first request", SECOND)],
    ) {
        stream("third thought", "finished restarted request", &[])
    } else {
        rejected()
    }
}

fn after_compaction(body: &str) -> Script {
    if body.contains("COMPACTED_PURPOSE") && valid(body, &[("call_after_compact", AFTER)]) {
        stream("final thought", "finished compacted request", &[])
    } else {
        rejected()
    }
}

#[test]
fn declared_reasoning_survives_no_call_turn_restart_and_compaction(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("compatible-reasoning-retained");
    configured(&root, "deepseek")?;
    let provider = ProviderFixture::start(vec![
        stream(
            FIRST,
            "reading both files",
            &[("call_reason_a", "a.txt"), ("call_reason_b", "b.txt")],
        ),
        Script::ForRequest(after_calls),
        Script::ForRequest(after_restart),
        stream("", "## Objective\n- COMPACTED_PURPOSE", &[]),
        stream(
            AFTER,
            "reading after compaction",
            &[("call_after_compact", "a.txt")],
        ),
        Script::ForRequest(after_compaction),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    select(address, &session);
    prompt(address, &session, "msg_reason_first", "read both files");
    compaction_continuity::approve_batch(address, &session, &["call_reason_a", "call_reason_b"]);
    permission_residuals::wait_session_idle(address, &session);
    manual_compaction::assert_carried(&root, "msg_reason_first", "session.execution.succeeded.1");
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    prompt(
        address,
        &session,
        "msg_reason_restart",
        "answer after restart",
    );
    permission_residuals::wait_session_idle(address, &session);
    manual_compaction::assert_carried(&root, "msg_reason_restart", "session.execution.succeeded.1");
    assert!((200..300).contains(
        &request(
            address,
            "POST",
            &format!("/api/session/{session}/compact"),
            "{}"
        )
        .0
    ));
    prompt(
        address,
        &session,
        "msg_reason_compact",
        "read again after compaction",
    );
    compaction_continuity::approve(address, &session, "call_after_compact");
    permission_residuals::wait_session_idle(address, &session);
    manual_compaction::assert_carried(&root, "msg_reason_compact", "session.execution.succeeded.1");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 6);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn other_integration_and_same_url_custom_do_not_emit_reasoning(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for catalog in ["openrouter", "custom"] {
        let root = root(&format!("compatible-reasoning-{catalog}"));
        configured(&root, catalog)?;
        let provider = ProviderFixture::start(vec![
            stream(FIRST, "reading", &[("call_reason_control", "a.txt")]),
            stream(SECOND, "answered", &[]),
        ]);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        select(address, &session);
        prompt(address, &session, "msg_reason_control", "read the file");
        compaction_continuity::approve(address, &session, "call_reason_control");
        permission_residuals::wait_session_idle(address, &session);
        manual_compaction::assert_carried(
            &root,
            "msg_reason_control",
            "session.execution.succeeded.1",
        );
        let bodies = locked(&provider.bodies);
        assert_eq!(bodies.len(), 2);
        assert!(bodies
            .iter()
            .all(|body| !body.contains("reasoning_content") && !body.contains(FIRST)));
        drop(bodies);
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&root);
    }
    Ok(())
}
