use super::*;

fn approved_prompt(address: SocketAddr, session: &str, call: &str) {
    let path = format!("/api/session/{session}/prompt");
    let prompt = thread::spawn(move || {
        let (status, body) = request(
            address,
            "POST",
            &path,
            r#"{"text":"exercise child controls"}"#,
        );
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    });
    let ask = permission_boundaries::pending(address, session, call);
    permission_boundaries::reply(address, session, &ask, "once");
    prompt
        .join()
        .unwrap_or_else(|_| panic!("child control prompt failed"));
    permission_residuals::wait_session_idle(address, session);
}

fn listing(body: &str, call: &str) -> serde_json::Value {
    let body: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("provider request is invalid: {error}"));
    let output = body["input"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["type"] == "function_call_output" && item["call_id"] == call)
        })
        .and_then(|item| item["output"].as_str())
        .unwrap_or_else(|| panic!("provider continuation omitted {call}"));
    serde_json::from_str(output)
        .unwrap_or_else(|error| panic!("listing was not JSON: {error}: {output}"))
}

#[test]
fn list_agents_reports_a_real_completed_child_only_to_its_parent() {
    let _journey = process_journey_custody();
    let root = root("agent-listing");
    prepare(&root, 200_000);
    attended(&root);
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_child",
            task: "summarise the repository",
        },
        Script::Success,
        Script::Success,
        Script::ToolInput {
            call: "call_list_parent",
            tool: "list_agents",
            arguments: serde_json::json!({}),
        },
        Script::Success,
        Script::ToolInput {
            call: "call_list_other",
            tool: "list_agents",
            arguments: serde_json::json!({}),
        },
        Script::Success,
    ]);
    let mut process = spawn(&root, provider.address);
    let address = await_ready(&root, &mut process);
    let _approver = approval::Attachment::open(address, "*");
    let parent = json(address, "POST", "/api/session", "{}")["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("parent id absent"))
        .to_owned();
    approved_prompt(address, &parent, "call_child");
    approved_prompt(address, &parent, "call_list_parent");
    let other = json(address, "POST", "/api/session", "{}")["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("other session id absent"))
        .to_owned();
    approved_prompt(address, &other, "call_list_other");
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 7);
    let own = listing(&bodies[4], "call_list_parent");
    let children = json(address, "GET", "/api/session", "");
    let child = children["data"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["parentID"] == parent))
        .unwrap_or_else(|| panic!("real child absent: {children}"));
    assert_eq!(own["agents"].as_array().map(Vec::len), Some(1));
    assert_eq!(own["agents"][0]["agent_id"], child["id"]);
    assert_eq!(own["agents"][0]["status"], "completed");
    assert!(own["agents"][0].get("output").is_none());
    assert_eq!(own["next_cursor"], serde_json::Value::Null);
    assert_eq!(
        listing(&bodies[6], "call_list_other")["agents"],
        serde_json::json!([])
    );
    process.stop_after_witness(&root);
    drop(process);
    process_fixture::cleanup(&root);
}
