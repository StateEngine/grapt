//! Real Product serialization across adaptive, legacy and unknown Claude models.
use super::*;

#[test]
fn native_claude_coding_uses_the_selected_models_supported_thinking_mode(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for (label, model, output, expected) in [
        (
            "adaptive",
            "claude-opus-5-5",
            64000,
            serde_json::json!({"type":"adaptive","display":"summarized"}),
        ),
        (
            "legacy",
            "claude-sonnet-4-5",
            16384,
            serde_json::json!({"type":"enabled","budget_tokens":2048}),
        ),
        ("unknown", "claude-unknown", 4096, serde_json::Value::Null),
        (
            "advertised",
            "new-native",
            128000,
            serde_json::json!({"type":"adaptive","display":"summarized"}),
        ),
        (
            "legacy-tight",
            "claude-sonnet-4-5",
            2048,
            serde_json::json!({"type":"enabled","budget_tokens":2047}),
        ),
    ] {
        let root = root(&format!("anthropic-thinking-{label}"));
        prepare(&root, 120_000);
        context_quality::configure_anthropic(&root, 200_000);
        for name in ["model-catalog.json", "anthropic-account-models.json"] {
            let path = root.join(name);
            let content = fs::read_to_string(&path)?;
            fs::write(path, content.replace("claude-live", model))?;
        }
        let path = root.join("model-catalog.json");
        let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        catalog["anthropic"]["models"][model]["limit"]["output"] = serde_json::json!(output);
        fs::write(path, serde_json::to_vec(&catalog)?)?;
        if label == "advertised" {
            let path = root.join("anthropic-account-models.json");
            let mut listing: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            listing["models"][0]["capabilities"] = serde_json::json!({"thinking":{
                "supported":true,"types":{
                    "adaptive":{"supported":true},"enabled":{"supported":false}
                }
            }});
            fs::write(path, serde_json::to_vec(&listing)?)?;
        }
        let provider = ProviderFixture::start(vec![context_quality::anthropic_text(
            "done",
            serde_json::json!({"input_tokens":20}),
        )]);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        let (status, _) = request(
            address,
            "POST",
            &format!("/api/session/{session}/model"),
            &serde_json::json!({"model":{"providerID":"anthropic","id":model}}).to_string(),
        );
        assert_eq!(status, 204);
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"id":"msg_thinking","text":"explain the answer","delivery":"queue","resume":true}"#,
        );
        permission_residuals::wait_session_idle(address, &session);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let bodies = locked(&provider.bodies).clone();
        let body: serde_json::Value = serde_json::from_str(bodies.first().ok_or("request body")?)?;
        assert_eq!(body["model"], model);
        assert_eq!(body["max_tokens"], output);
        assert_eq!(body["thinking"], expected);
        assert_eq!(body["tool_choice"]["type"], "auto");
        assert!(!body["tools"].as_array().ok_or("tools")?.is_empty());
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&root);
    }
    Ok(())
}
