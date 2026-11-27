//! A text patch reaches the existing permissions, effects and model continuation.
use super::*;

#[test]
fn native_text_patch_edits_files_and_inspect_cannot_run_it(
) -> Result<(), Box<dyn std::error::Error>> {
    offered_text_patch_and_undo("openai-responses")
}

#[test]
fn codex_offered_text_patch_edits_and_undo_restores_files() -> Result<(), Box<dyn std::error::Error>>
{
    offered_text_patch_and_undo("chatgpt-codex")
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn offered_text_patch_and_undo(family: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root(&format!("apply-patch-{family}"));
    prepare(&root, 200_000);
    attended(&root);
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["provider"]["protocol_family"] = serde_json::json!(family);
    if family == "openai-responses" {
        config["catalog"]["source"] = serde_json::json!("models.dev-openai");
        let inventory_path = root.join("model-catalog.json");
        let mut inventory: serde_json::Value = serde_json::from_slice(&fs::read(&inventory_path)?)?;
        inventory["openai"]["models"]["gpt-live"]["limit"]["context"] = serde_json::json!(128_000);
        fs::write(inventory_path, serde_json::to_vec(&inventory)?)?;
        fs::write(
            root.join("account-models.json"),
            serde_json::to_vec(&serde_json::json!({"data":[{"id":"gpt-live"}]}))?,
        )?;
    }
    fs::write(path, serde_json::to_vec(&config)?)?;
    // Outside the service root, so its journal stays out of the undo capture.
    let workspace = super::root(&format!("apply-patch-{family}-workspace"));
    process_fixture::cleanup(&workspace);
    fs::create_dir_all(&workspace)?;
    let create_at = |address| {
        json(
            address,
            "POST",
            "/api/session",
            &serde_json::json!({"location":{"directory":workspace}}).to_string(),
        )["data"]["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or("created workspace Session has no identity")
    };
    fs::write(workspace.join("source.txt"), "old\n")?;
    let patch="*** Begin Patch\n*** Update File: source.txt\n@@\n-old\n+new\n*** Update File: source.txt\n@@\n-new\n+newer\n*** Add File: added.txt\n+added\n*** Add File: deep/new.txt\n+new parent\n*** End Patch";
    let frames = [
        serde_json::json!({"type":"response.output_item.added","item":{"type":"custom_tool_call","id":"patch_item","call_id":"call_apply_patch_native","name":"apply_patch"}}),
        serde_json::json!({"type":"response.custom_tool_call_input.delta","item_id":"patch_item","delta":patch}),
        serde_json::json!({"type":"response.custom_tool_call_input.done","item_id":"patch_item","input":patch}),
        serde_json::json!({"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
    ];
    let body = frames
        .iter()
        .map(|value| format!("data: {value}\n\n"))
        .collect::<String>();
    fs::write(workspace.join("added.txt"), "previous content\n")?;
    let provider = ProviderFixture::start(vec![
        Script::Raw(body),
        Script::Success,
        Script::Success,
        Script::ToolInput {
            call: "call_apply_patch_denied",
            tool: "apply_patch",
            arguments: serde_json::json!({"input":
            "*** Begin Patch\n*** Add File: denied.txt\n+must not happen\n*** End Patch"}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create_at(address)?;
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"apply the change"}"#,
    );
    let overwrite = write_overwrite::asked(address, &session, "call_apply_patch_native");
    assert!(
        overwrite.to_string().contains("fs.overwrite"),
        "{overwrite}"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("added.txt"))?,
        "previous content\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("source.txt"))?,
        "old\n",
        "no earlier effect may run while overwrite approval is pending"
    );
    write_overwrite::reply(address, &session, &overwrite, "once");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(fs::read_to_string(workspace.join("source.txt"))?, "newer\n");
    assert_eq!(fs::read_to_string(workspace.join("added.txt"))?, "added\n");
    assert_eq!(
        fs::read_to_string(workspace.join("deep/new.txt"))?,
        "new parent\n"
    );
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    assert!(messages.to_string().contains("apply_patch"));
    // The offered text patch is attributable: undo restores the overwritten
    // file and the twice-updated source, and removes the added file.
    let preview = json(
        address,
        "POST",
        &format!("/api/session/{session}/undo/preview"),
        "{}",
    );
    assert_eq!(preview["data"]["paths"].as_array().map(Vec::len), Some(3));
    assert_eq!(preview["data"]["writes"], 2);
    assert_eq!(preview["data"]["deletes"], 1);
    let undo = json(
        address,
        "POST",
        &format!("/api/session/{session}/undo"),
        &serde_json::json!({"inputID":preview["data"]["inputID"],"target":preview["data"]["target"]})
            .to_string(),
    );
    assert_eq!(undo["data"]["status"], "applied", "{undo}");
    assert_eq!(fs::read_to_string(workspace.join("source.txt"))?, "old\n");
    assert_eq!(
        fs::read_to_string(workspace.join("added.txt"))?,
        "previous content\n"
    );
    assert!(!workspace.join("deep/new.txt").exists());
    assert!(
        workspace.join("deep").is_dir(),
        "undo leaves the patch-created directory"
    );
    assert_eq!(
        json(
            address,
            "GET",
            &format!("/api/session/{session}/message"),
            "",
        ),
        messages,
        "file undo changed conversation history"
    );
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"continue after reopening"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(fs::read_to_string(workspace.join("source.txt"))?, "old\n");
    let inspect = create_at(address)?;
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{inspect}/preset"),
        r#"{"preset":"inspect"}"#,
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
    json(
        address,
        "POST",
        &format!("/api/session/{inspect}/prompt"),
        r#"{"text":"inspect only"}"#,
    );
    permission_residuals::wait_session_idle(address, &inspect);
    assert!(!workspace.join("denied.txt").exists());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 5);
    let bodies = locked(&provider.bodies).clone();
    for body in bodies.iter().take(3) {
        let body: serde_json::Value = serde_json::from_str(body)?;
        let tools = body["tools"].as_array().ok_or("offered tools are absent")?;
        assert_eq!(
            tools
                .iter()
                .filter(|tool| tool["name"] == "apply_patch")
                .count(),
            1
        );
        assert!(
            !tools.iter().any(|tool| tool["name"] == "patch"),
            "the restored editing path still advertises duplicate patch interfaces"
        );
    }
    let allowed: serde_json::Value = serde_json::from_str(&bodies[0])?;
    let denied: serde_json::Value = serde_json::from_str(&bodies[3])?;
    let offered = |value: &serde_json::Value| {
        value["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|tool| tool["name"] == "apply_patch")
    };
    assert!(offered(&allowed) && !offered(&denied));
    assert_eq!(
        allowed["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|tool| tool["name"] == "apply_patch")
            .map(|tool| &tool["type"]),
        Some(&serde_json::json!("custom"))
    );
    let reopened: serde_json::Value = serde_json::from_str(&bodies[2])?;
    assert!(reopened["input"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|item| item["type"] == "custom_tool_call" && item["input"] == patch));
    let continued: serde_json::Value = serde_json::from_str(&bodies[1])?;
    let output = continued["input"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| {
            item["type"] == "custom_tool_call_output"
                && item["call_id"] == "call_apply_patch_native"
        })
        .and_then(|item| item["output"].as_str())
        .ok_or("patch result did not reach the model")?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(output)?["complete"],
        true
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(output)?
            .get("undoUnavailable")
            .is_none(),
        "{output}"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    process_fixture::cleanup(&workspace);
    Ok(())
}

#[test]
fn malformed_native_text_patch_stays_text_in_continuation_and_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("apply-patch-invalid-text");
    prepare(&root, 200_000);
    fs::write(root.join("source.txt"), "unchanged\n")?;
    let patch = "invalid patch syntax\nλ";
    let frames = [
        serde_json::json!({"type":"response.output_item.added","item":{"type":"custom_tool_call","id":"invalid_item","call_id":"call_invalid_patch","name":"apply_patch"}}),
        serde_json::json!({"type":"response.custom_tool_call_input.done","item_id":"invalid_item","input":patch}),
        serde_json::json!({"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
    ];
    let body = frames
        .iter()
        .map(|value| format!("data: {value}\n\n"))
        .collect::<String>();
    let provider =
        ProviderFixture::start(vec![Script::Raw(body), Script::Success, Script::Success]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"try an invalid patch"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"continue after restart"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    for body in locked(&provider.bodies).iter().skip(1) {
        let request: serde_json::Value = serde_json::from_str(body)?;
        let input = request["input"].as_array().ok_or("provider input")?;
        let call = input
            .iter()
            .find(|item| {
                item["call_id"] == "call_invalid_patch" && item["type"] == "custom_tool_call"
            })
            .ok_or("invalid builtin patch lost text syntax")?;
        assert_eq!(call["name"], "apply_patch");
        assert_eq!(call["input"], patch);
        assert!(
            !input
                .iter()
                .any(|item| item["call_id"] == "call_invalid_patch"
                    && item["type"] == "function_call")
        );
        assert!(input
            .iter()
            .any(|item| item["call_id"] == "call_invalid_patch"
                && item["type"] == "custom_tool_call_output"
                && item["output"]
                    .as_str()
                    .is_some_and(|text| text.contains("Tool failed"))));
    }
    assert_eq!(fs::read_to_string(root.join("source.txt"))?, "unchanged\n");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
