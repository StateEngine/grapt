//! Exercise the production composition caller, not only its opener helper.
use super::*;

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn selected_secondary_account_crosses_production_composition_and_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("secondary-account");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("grapt.jsonc"))?)?;
    config["catalog"] = serde_json::json!({"source":"models.dev-chatgpt"});
    config["provider"]["accounts"] = serde_json::json!([{
        "id":"openrouter", "route":"route-secondary", "account":"account-b",
        "credential_version":"credential-b1", "policy_version":"live-policy-v1",
        "protocol_family":"openai-compatible", "endpoint":"https://openrouter.ai/api/v1",
        "credential":"openrouter.credential", "discovery":"openrouter-account-models.json"
    }]);
    fs::write(root.join("grapt.jsonc"), serde_json::to_vec(&config)?)?;
    fs::write(root.join("openrouter.credential"), "secondary-secret")?;
    let mut catalog = serde_json::Map::new();
    for (provider, model) in [("openai", "gpt-live"), ("openrouter", "vendor-model-a")] {
        catalog.insert(
            provider.to_owned(),
            serde_json::json!({
                "id":provider,"name":provider,"npm":"@ai-sdk/openai",
                "models":{model:{"id":model,"name":model,"family":"gpt-5",
                    "tool_call":true,"modalities":{"input":["text"],"output":["text"]},
                    "limit":{"output":32000}}}
            }),
        );
        let discovery = if provider == "openai" {
            "account-models.json"
        } else {
            "openrouter-account-models.json"
        };
        fs::write(
            root.join(discovery),
            serde_json::to_vec(&serde_json::json!({
                "models":[{"slug":model,"visibility":"list","context_window":128000,
                    "supported_reasoning_levels":[]}]
            }))?,
        )?;
    }
    fs::write(
        root.join("model-catalog.json"),
        serde_json::to_vec(&catalog)?,
    )?;
    let provider =
        ProviderFixture::start(vec![Script::CompatibleSuccess, Script::CompatibleSuccess]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let (events, connected) = event_stream(address);
    let (status, response) = request(
        address,
        "POST",
        &format!("/api/session/{session}/model"),
        r#"{"model":{"providerID":"openrouter","id":"vendor-model-a"}}"#,
    );
    assert_eq!(
        status,
        204,
        "selection refused: {}",
        String::from_utf8_lossy(&response)
    );
    let selected = collect(events, connected, "session.model.selected");
    assert!(selected.contains("vendor-model-a"));
    let selected_seq = durable_seq(&selected, "session.model.selected");
    assert!(selected_seq > 0);
    let (events, connected) = event_stream(address);
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/model"),
            r#"{"model":{"providerID":"openrouter","id":"vendor-model-a"}}"#
        )
        .0,
        204
    );
    assert_eq!(
        request(
            address,
            "POST",
            &format!("/api/session/{session}/agent"),
            r#"{"agent":"agt_dogfood_allow"}"#
        )
        .0,
        204
    );
    let agent = collect(events, connected, "session.agent.selected");
    assert!(
        !agent.contains("session.model.selected"),
        "unchanged model published a duplicate"
    );
    assert_eq!(
        durable_seq(&agent, "session.agent.selected"),
        selected_seq + 1
    );
    let path = format!("/api/session/{session}/prompt");
    let prompt =
        r#"{"id":"msg_secondary_first","text":"hello secondary","delivery":"queue","resume":true}"#;
    json(address, "POST", &path, prompt);
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "selected account never dispatched"
    );
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc"),
        "",
    );
    assert!(
        messages.to_string().contains("Secondary account reached."),
        "compatible result lost: {messages}"
    );
    let assistant = &messages["data"][1];
    assert_eq!(assistant["model"]["providerID"], "openrouter");
    assert_eq!(
        assistant["observedUsage"],
        serde_json::json!({
            "inputTotal":12,"output":4,"total":16
        })
    );
    assert!(assistant.get("tokens").is_none());
    let message_id = assistant["id"]
        .as_str()
        .ok_or("assistant identity missing")?;
    let message_path = format!("/api/session/{session}/message/{message_id}");
    let fetched = json(address, "GET", &message_path, "");
    assert_eq!(fetched["data"], *assistant);
    let user = json(
        address,
        "GET",
        &format!("/api/session/{session}/message/msg_secondary_first"),
        "",
    );
    assert_eq!(user["data"], messages["data"][0]);
    let foreign = create(address);
    for (owner, id, tag) in [
        (foreign.as_str(), message_id, "MessageNotFoundError"),
        (session.as_str(), "msg_missing", "MessageNotFoundError"),
        ("ses_missing", message_id, "SessionNotFoundError"),
    ] {
        let (status, bytes) = request(
            address,
            "GET",
            &format!("/api/session/{owner}/message/{id}"),
            "",
        );
        assert_eq!(status, 404);
        let error: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(error["_tag"], tag);
        assert_eq!(error["sessionID"], owner);
        if tag == "MessageNotFoundError" {
            assert_eq!(error["messageID"], id);
        }
    }
    let heads = locked(&provider.heads);
    assert!(heads[0]
        .to_ascii_lowercase()
        .contains("authorization: bearer secondary-secret"));
    assert!(!heads[0].contains("provider-secret"));
    drop(heads);
    let bodies = locked(&provider.bodies);
    let body: serde_json::Value = serde_json::from_str(&bodies[0])?;
    assert_eq!(body["model"], "vendor-model-a");
    assert!(
        body["messages"].is_array(),
        "primary protocol used for secondary selection"
    );
    assert!(body.get("input").is_none());
    drop(bodies);

    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    assert_eq!(json(address, "GET", &message_path, ""), fetched);
    json(address, "POST", &path, prompt);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "completed input resurrected on restart"
    );
    json(
        address,
        "POST",
        &path,
        r#"{"id":"msg_secondary_second","text":"continue secondary","delivery":"queue","resume":true}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "persisted selection lost on restart"
    );
    let body: serde_json::Value = serde_json::from_str(&locked(&provider.bodies)[1])?;
    assert_eq!(body["model"], "vendor-model-a");
    assert!(
        body["messages"]
            .to_string()
            .contains("Secondary account reached."),
        "durable compatible history lost"
    );
    Ok(())
}

#[test]
fn legacy_openai_launch_selection_is_stored_and_issued_through_chatgpt(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("chatgpt-launch-alias");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("grapt.jsonc"))?)?;
    config["provider"]["id"] = serde_json::json!("chatgpt");
    config["provider"]["catalog_provider"] = serde_json::json!("openai");
    fs::write(root.join("grapt.jsonc"), serde_json::to_vec(&config)?)?;
    let provider = ProviderFixture::start(vec![Script::Success]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let (status, bytes) = request(
        address,
        "POST",
        "/api/session",
        r#"{"id":"ses_chatgpt_alias","model":{"providerID":"openai","id":"gpt-live"}}"#,
    );
    assert_eq!(
        status,
        200,
        "launch refused: {}",
        String::from_utf8_lossy(&bytes)
    );
    let created: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(created["data"]["model"]["providerID"], "chatgpt");
    let session = "ses_chatgpt_alias";
    for (method, route) in [("POST", "model"), ("PUT", "subagent-model")] {
        let (status, bytes) = request(
            address,
            method,
            &format!("/api/session/{session}/{route}"),
            r#"{"model":{"providerID":"openai","id":"gpt-live"}}"#,
        );
        assert_eq!(
            status,
            204,
            "{route} refused: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    assert_eq!(
        json(
            address,
            "GET",
            &format!("/api/session/{session}/subagent-model"),
            ""
        )["data"]["providerID"],
        "chatgpt"
    );
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_chatgpt_alias","text":"use the selected subscription","delivery":"queue"}"#,
    );
    permission_residuals::wait_session_idle(address, session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc"),
        "",
    );
    assert_eq!(messages["data"][1]["model"]["providerID"], "chatgpt");
    child.stop_after_witness(&root);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    assert_eq!(
        json(address, "GET", &format!("/api/session/{session}"), "")["data"]["model"]["providerID"],
        "chatgpt"
    );
    Ok(())
}
