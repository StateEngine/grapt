//! Run the guide's line-input and checklist examples against the real service.
use super::*;

#[test]
fn guide_line_input_and_plan_examples_use_the_real_product(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("terminal-guide-examples");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut service = spawn(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let session = create(address);
    #[cfg(windows)]
    let command = "set /p GRAPT_REPLY=ready^> & set GRAPT_REPLY";
    #[cfg(unix)]
    let command = "printf 'ready> '; IFS= read -r word; printf '\\nreply=%s\\n' \"$word\"";
    let raw = tool(
        &root,
        &provider,
        address,
        &session,
        "call_guide_start",
        "exec_command",
        serde_json::json!({"cmd":command,"login":false,"tty":true,"yield_time_ms":250,"timeout_ms":60000,"max_output_tokens":256}),
    )?;
    let start: serde_json::Value = serde_json::from_str(&raw)?;
    let operation = start["session_id"]
        .as_str()
        .ok_or("guide process handle absent")?;
    assert_eq!(start["done"], false);
    let raw = tool(
        &root,
        &provider,
        address,
        &session,
        "call_guide_line",
        "write_stdin",
        serde_json::json!({"session_id":operation,"chars":"hello\n","yield_time_ms":5000}),
    )?;
    let mut result: serde_json::Value = serde_json::from_str(&raw)?;
    let mut output = result["output"]
        .as_str()
        .ok_or("guide output absent")?
        .to_owned();
    for call in ["call_guide_poll_1", "call_guide_poll_2"] {
        if result["done"] == true {
            break;
        }
        let raw = tool(
            &root,
            &provider,
            address,
            &session,
            call,
            "write_stdin",
            stdin(operation, "", None),
        )?;
        result = serde_json::from_str(&raw)?;
        output.push_str(
            result["output"]
                .as_str()
                .ok_or("guide poll output absent")?,
        );
    }
    assert_eq!(result["done"], true);
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["cleanup"], "complete");
    #[cfg(windows)]
    assert!(output.contains("GRAPT_REPLY=hello"), "{output}");
    #[cfg(unix)]
    assert!(output.contains("reply=hello"), "{output}");
    let plan = serde_json::json!({"plan":[{"step":"Reproduce","status":"completed"},{"step":"Fix","status":"in_progress"},{"step":"Verify","status":"pending"}]});
    let acknowledgement = tool(
        &root,
        &provider,
        address,
        &session,
        "call_guide_plan",
        "update_plan",
        plan.clone(),
    )?;
    assert_eq!(acknowledgement, "Plan updated");
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=200"),
        "",
    );
    let plans = history["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "tool" && part["name"] == "update_plan")
        .collect::<Vec<_>>();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0]["state"]["status"], "completed");
    assert_eq!(plans[0]["state"]["input"], plan);
    service.stop_after_witness(&root);
    drop(service);
    process_fixture::cleanup(&root);
    Ok(())
}
