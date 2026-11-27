//! The checklist uses the ordinary provider/tool/history path, including restart.
use super::*;

#[test]
fn checklist_updates_fail_actionably_clear_and_survive_product_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("plan-updates");
    prepare(&root, 200_000);
    let plan = serde_json::json!({"explanation":"Test before fixing", "plan":[
        {"step":"Reproduce","status":"completed"},
        {"step":"Fix","status":"in_progress"},
        {"step":"Verify","status":"pending"}
    ]});
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_plan_first",
            tool: "update_plan",
            arguments: plan.clone(),
        },
        Script::ToolInput {
            call: "call_plan_invalid",
            tool: "update_plan",
            arguments: serde_json::json!({"plan":[
                {"step":"a","status":"in_progress"},{"step":"b","status":"in_progress"}
            ]}),
        },
        Script::ToolInput {
            call: "call_plan_clear",
            tool: "update_plan",
            arguments: serde_json::json!({"plan":[]}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_plan_updates","text":"track the work","delivery":"queue","resume":true}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(
        bodies.len(),
        4,
        "each checklist result must reach the model"
    );
    let requests = bodies
        .iter()
        .map(|body| serde_json::from_str::<serde_json::Value>(body))
        .collect::<Result<Vec<_>, _>>()?;
    let output = |index: usize, call: &str| -> Result<&str, Box<dyn std::error::Error>> {
        requests[index]["input"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == call)
            .and_then(|item| item["output"].as_str())
            .ok_or_else(|| format!("result missing for {call}").into())
    };
    assert_eq!(output(1, "call_plan_first")?, "Plan updated");
    assert!(output(2, "call_plan_invalid")?.contains("at most one plan step may be in_progress"));
    assert_eq!(output(3, "call_plan_clear")?, "Plan updated");
    let messages_path = format!("/api/session/{session}/message?order=asc");
    let before = json(address, "GET", &messages_path, "");
    let tools = before["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 3);
    assert_eq!(tools[0]["state"]["status"], "completed");
    assert_eq!(tools[1]["state"]["status"], "error");
    assert!(tools[1]["state"]
        .to_string()
        .contains("at most one plan step"));
    assert_eq!(tools[2]["state"]["status"], "completed");
    child.stop_after_witness(&root);
    drop(child);
    let mut restarted = spawn(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    let after = json(address, "GET", &messages_path, "");
    assert_eq!(
        after, before,
        "restart must not replace or repeat a checklist update"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    restarted.stop_after_witness(&root);
    drop(restarted);
    process_fixture::cleanup(&root);
    Ok(())
}
