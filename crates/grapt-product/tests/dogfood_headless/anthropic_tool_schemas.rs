//! Real built-ins and the public StateEngine core catalog over Anthropic HTTP.
use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn default_anthropic_tools_with_stateengine_have_flat_wire_schemas(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("anthropic-tool-schemas");
    prepare(&root, 120_000);
    context_quality::configure_anthropic(&root, 200_000);
    let catalog_path = root.join("model-catalog.json");
    let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&catalog_path)?)?;
    catalog["anthropic"]["models"]["claude-live"]["modalities"]["input"] =
        serde_json::json!(["text", "image"]);
    fs::write(catalog_path, serde_json::to_vec(&catalog)?)?;
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/anthropic-schema-peer.py");
    config["mcp"] = serde_json::json!({"stateengine": {
        "type":"local", "command":["python3",script,root.join("mcp-call-marker")], "codemode":false
    }});
    config["permission"] = serde_json::json!({"stateengine_*":"allow"});
    fs::write(path, config.to_string())?;
    let provider = ProviderFixture::start(vec![
        Script::Raw(context_quality::anthropic_stream(
            &[
                serde_json::json!({"type":"tool_use", "id":"call_schema_invalid",
                "name":"stateengine_stateengine_atlas_query",
                "input":{"project_id":"fixture","query":"decision",
                    "queries":[{"id":"one","query":"decision"}]}}),
            ],
            "tool_use",
            serde_json::json!({"input_tokens":20}),
        )),
        context_quality::anthropic_text("ready", serde_json::json!({"input_tokens":20})),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":root}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("session missing")?
        .to_owned();
    context_quality::select_anthropic(address, &session);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"hello"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    child.stop_after_witness(&root);
    drop(child);
    let bodies = locked(&provider.bodies);
    assert_eq!(
        bodies.len(),
        2,
        "ordinary failed tool input must return to the model"
    );
    assert!(
        !root.join("mcp-call-marker").exists(),
        "invalid Atlas arguments reached tools/call instead of failing locally"
    );
    let continuation: serde_json::Value = serde_json::from_str(&bodies[1])?;
    let results: Vec<_> = continuation["messages"]
        .as_array()
        .ok_or("messages")?
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|block| block["type"] == "tool_result")
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["is_error"], true);
    assert!(results[0]["content"]
        .as_str()
        .ok_or("tool failure detail")?
        .contains("tool input does not match its published schema"));
    let body: serde_json::Value = serde_json::from_str(&bodies[0])?;
    let tools = body["tools"].as_array().ok_or("tools missing")?;
    let atlas = tools
        .iter()
        .find(|tool| tool["name"] == "stateengine_stateengine_atlas_query")
        .ok_or("Atlas tool missing")?;
    let original: serde_json::Value =
        serde_json::from_str(include_str!("../support/stateengine-core-tools.json"))?;
    for definition in original["tools"].as_array().ok_or("fixture catalog")? {
        let name = definition["name"].as_str().ok_or("fixture tool name")?;
        let qualified = format!("stateengine_{name}");
        // Automatic bootstrap and explicit model resume share the governed tool.
        assert!(
            tools.iter().any(|tool| tool["name"] == qualified),
            "model publication missing for {name}"
        );
    }
    for tool in tools {
        assert_eq!(tool["input_schema"]["type"], "object", "{}", tool["name"]);
        for keyword in ["oneOf", "anyOf", "allOf"] {
            assert!(
                tool["input_schema"].get(keyword).is_none(),
                "{} has {keyword}",
                tool["name"]
            );
        }
    }
    assert!(atlas["description"]
        .as_str()
        .ok_or("Atlas description")?
        .contains("oneOf"));
    // Optional diagnostic export is only the public tool definitions, never
    // request headers, credentials, prompts or Session data.
    if let Ok(destination) = std::env::var("GRAPT_TEST_ANTHROPIC_TOOLS_EXPORT") {
        fs::write(destination, serde_json::to_vec(&body["tools"])?)?;
    }
    process_fixture::cleanup(&root);
    Ok(())
}
