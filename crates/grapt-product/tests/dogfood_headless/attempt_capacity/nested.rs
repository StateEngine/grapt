//! Real root -> child -> grandchild -> child -> root on one shared adapter.
use super::*;

#[test]
fn root_child_and_grandchild_alternate_at_capacity_one_and_all_complete() {
    let _journey = process_journey_custody();
    let root = root("capacity-nested-family");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 200_000);
    attended(&root);
    one_slot(&root);
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_nested_child",
            task: "audit using a grandchild",
        },
        Script::ToolSubagent {
            call: "call_nested_grandchild",
            task: "perform the nested audit",
        },
        Script::SuccessText("grandchild audit complete"),
        Script::SuccessText("child combined the grandchild audit"),
        Script::SuccessText("root combined the child audit"),
    ]);
    let mut process = spawn(&root, provider.address);
    let address = await_ready(&root, &mut process);
    let _approver = approval::Attachment::open(address, "*");
    let parent = create(address);
    prompt(
        address,
        &parent,
        "msg_capacity_nested",
        "delegate the nested audit",
    );
    let ask = permission_boundaries::pending(address, &parent, "call_nested_child");
    permission_boundaries::reply(address, &parent, &ask, "once");
    let wait = PollDeadline::new();
    let child = wait
        .poll(Duration::from_millis(10), || {
            let children = super::children::children(address, &parent);
            if let Some(child) = children.first() {
                return Some(child.clone());
            }
            None
        })
        .unwrap_or_else(|| panic!("nested child never appeared"));
    let ask = permission_boundaries::pending(address, &child, "call_nested_grandchild");
    permission_boundaries::reply(address, &child, &ask, "once");
    manual_compaction::assert_carried(
        &root,
        "msg_capacity_nested",
        "session.execution.succeeded.1",
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 5);
    let grandchildren = super::children::children(address, &child);
    assert_eq!(grandchildren.len(), 1);
    let bodies = locked(&provider.bodies);
    for (index, call) in [(3, "call_nested_grandchild"), (4, "call_nested_child")] {
        let body: serde_json::Value = serde_json::from_str(&bodies[index])
            .unwrap_or_else(|error| panic!("nested provider body: {error}"));
        assert!(
            body["input"].as_array().is_some_and(|input| input
                .iter()
                .any(|item| item["type"] == "function_call_output" && item["call_id"] == call)),
            "the nested result did not reach its own parent"
        );
    }
    drop(bodies);
    for session in [&parent, &child, &grandchildren[0]] {
        permission_residuals::wait_session_idle(address, session);
    }
    process.stop_after_witness(&root);
}
