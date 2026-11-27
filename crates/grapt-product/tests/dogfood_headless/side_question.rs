//! An ephemeral question copies settled context without changing its parent.
use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn side_question_copies_context_and_never_saves_a_turn_or_runs_tools() {
    let _journey = process_journey_custody();
    let root = root("side-question");
    prepare(&root, 120_000);
    prepare_model_inventory(&root, 128_000);
    // Both accounts must exist before the first startup migrates their authority.
    context_quality::configure_anthropic(&root, 200_000);
    fs::write(
        root.join("AGENTS.md"),
        "SIDE_PARENT_INSTRUCTION: answer precisely.",
    )
    .unwrap_or_else(|error| panic!("instructions: {error}"));
    fs::write(root.join("seed.txt"), "SIDE_PARENT_TOOL_RESULT")
        .unwrap_or_else(|error| panic!("seed: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_side_seed",
            path: "seed.txt",
        },
        Script::SuccessText("PARENT_ASSISTANT"),
        Script::SuccessText("EPHEMERAL_SIDE_ANSWER"),
        Script::SuccessText(""),
        Script::ToolInput {
            call: "call_side_forbidden",
            tool: "write",
            arguments: serde_json::json!({"path":"side-created.txt","content":"forbidden"}),
        },
        Script::SuccessText("PARENT_CONTINUES"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let prompt = |id: &str, text: &str| {
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
        )
    };
    prompt("msg_side_parent", "PARENT_USER read seed.txt");
    compaction_continuity::approve(address, &session, "call_side_seed");
    wait_calls(&provider.calls, 2);
    permission_residuals::wait_session_idle(address, &session);
    let history = || {
        json(
            address,
            "GET",
            &format!("/api/session/{session}/message"),
            "",
        )
    };
    let before = history();
    let sessions = json(address, "GET", "/api/session", "");
    let question = "/api/session/".to_owned() + &session + "/side";
    let answer = json(
        address,
        "POST",
        &question,
        r#"{"prompt":"EPHEMERAL_SIDE_QUESTION"}"#,
    );
    assert_eq!(answer["text"], "EPHEMERAL_SIDE_ANSWER");
    assert_eq!(history(), before, "side question changed parent history");
    assert_eq!(
        json(address, "GET", "/api/session", ""),
        sessions,
        "side question created a durable child"
    );
    let bodies = locked(&provider.bodies);
    let side_request: serde_json::Value =
        serde_json::from_str(&bodies[2]).unwrap_or_else(|error| panic!("side request: {error}"));
    for copied in [
        "PARENT_USER",
        "PARENT_ASSISTANT",
        "SIDE_PARENT_TOOL_RESULT",
        "SIDE_PARENT_INSTRUCTION",
    ] {
        assert!(bodies[2].contains(copied), "side snapshot lost {copied}");
    }
    assert!(side_request["tools"].as_array().is_none_or(Vec::is_empty));
    drop(bodies);
    let (empty_status, empty_body) = request(
        address,
        "POST",
        &question,
        r#"{"prompt":"EMPTY_SIDE_QUESTION"}"#,
    );
    assert_eq!(empty_status, 400, "empty side answer was accepted");
    assert!(String::from_utf8_lossy(&empty_body).contains("returned no text"));
    assert_eq!(history(), before, "empty answer changed parent history");
    let (status, body) = request(
        address,
        "POST",
        &question,
        r#"{"prompt":"REFUSED_SIDE_QUESTION"}"#,
    );
    assert_eq!(
        status,
        400,
        "unexpected side refusal: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(!root
        .join("side-created.txt")
        .try_exists()
        .unwrap_or_else(|error| panic!("effect: {error}")));
    assert_eq!(
        history(),
        before,
        "refused side question changed parent history"
    );
    prompt("msg_side_continue", "PARENT_FOLLOWUP");
    wait_calls(&provider.calls, 6);
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies);
    for private in [
        "EPHEMERAL_SIDE_QUESTION",
        "EPHEMERAL_SIDE_ANSWER",
        "REFUSED_SIDE_QUESTION",
        "EMPTY_SIDE_QUESTION",
    ] {
        assert!(
            !bodies[5].contains(private),
            "side turn reached the parent continuation"
        );
    }
    drop(bodies);
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let restored = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )
    .to_string();
    assert!(restored.contains("PARENT_CONTINUES"));
    assert!(!restored.contains("EPHEMERAL_SIDE"));
    child.stop_after_witness(&root);
    drop(child);

    // This route must also serialize with Anthropic's initial System prefix.
    let provider = ProviderFixture::start(vec![
        context_quality::anthropic_text("ANTHROPIC_PARENT", serde_json::json!({"input_tokens":20})),
        context_quality::anthropic_text("ANTHROPIC_SIDE", serde_json::json!({"input_tokens":30})),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    context_quality::select_anthropic(address, &session);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_side_anthropic","text":"Anthropic parent","delivery":"queue","resume":true}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let path = format!("/api/session/{session}/message");
    let before = json(address, "GET", &path, "");
    let answer = json(
        address,
        "POST",
        &format!("/api/session/{session}/side"),
        r#"{"prompt":"Explain this parent"}"#,
    );
    assert_eq!(answer["text"], "ANTHROPIC_SIDE");
    assert_eq!(json(address, "GET", &path, ""), before);
    child.stop_after_witness(&root);
    drop(child);
    let _ = fs::remove_dir_all(root);
}
