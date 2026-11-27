//! A compacted parent can fork and execute without inheriting foreign cursors.
use super::*;

fn instruction_position(root: &Path, session: &str, first: bool) -> i64 {
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("instruction evidence is unreadable: {error}"));
    let order = if first { "ASC" } else { "DESC" };
    db.query_row(
        &format!(
            "SELECT seq FROM event WHERE aggregate_family='session' AND aggregate_id=?1
         AND type IN ('session.instructions.updated.2','session.compaction.ended.2')
         ORDER BY seq {order} LIMIT 1"
        ),
        [session],
        |row| row.get(0),
    )
    .unwrap_or_else(|error| panic!("instruction boundary absent: {error}"))
}

#[test]
fn rel15_a_fork_of_a_compacted_parent_executes_its_first_step_and_a_nested_fork() {
    let _journey = process_journey_custody();
    let root = root("rel15-compacted-fork");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 180_000);
    fs::write(root.join("AGENTS.md"), "REL15-stale-rule")
        .unwrap_or_else(|error| panic!("instruction fixture: {error}"));
    fs::write(root.join("seed.txt"), vec![b's'; 7_524])
        .unwrap_or_else(|error| panic!("seed fixture: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_rel15_seed",
            path: "seed.txt",
        },
        Script::SuccessText("parent seeded"),
        Script::SuccessText("parent instructions updated"),
        Script::SuccessText(
            "## Objective\n- The seed was read and the current rules govern further work.",
        ),
        Script::SuccessText("parent compacted"),
        Script::SuccessText("child completed"),
        Script::SuccessText("grandchild completed"),
    ]);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let parent = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{parent}/prompt"),
        r#"{"id":"msg_rel15_seed","text":"read the seed","delivery":"queue","resume":true}"#,
    );
    compaction_continuity::approve(address, &parent, "call_rel15_seed");
    wait_session_idle(address, &parent);
    manual_compaction::assert_carried(&root, "msg_rel15_seed", "session.execution.succeeded.1");
    fs::write(root.join("AGENTS.md"), "REL15-current-rule")
        .unwrap_or_else(|error| panic!("updated instruction fixture: {error}"));
    prompt(
        address,
        &parent,
        "msg_rel15_updated",
        "apply the current instructions",
    );
    manual_compaction::assert_carried(&root, "msg_rel15_updated", "session.execution.succeeded.1");
    assert!(request_body(&provider, 2).contains("REL15-current-rule"));
    product.stop_after_witness(&root);
    drop(product);

    manual_compaction::admit_offline(&root, &parent);
    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    prompt(
        address,
        &parent,
        "msg_rel15_compact",
        "continue after compacting",
    );
    manual_compaction::assert_carried(&root, "msg_rel15_compact", "session.execution.succeeded.1");
    assert!(request_body(&provider, 3)
        .contains("Output exactly the Markdown structure shown inside <template>"));
    let inherited_through = instruction_position(&root, &parent, false);
    let parent_before = turns(address, &parent);
    let child = fork(address, &parent);
    prompt(address, &child, "msg_rel15_child", "continue in the fork");
    manual_compaction::assert_carried(&root, "msg_rel15_child", "session.execution.succeeded.1");
    assert!(
        instruction_position(&root, &child, true) < inherited_through,
        "fixture did not restart numbering below inherited instruction history"
    );
    let child_request = request_body(&provider, 5);
    assert!(
        child_request.contains("REL15-current-rule") && !child_request.contains("REL15-stale-rule")
    );
    let grandchild = fork(address, &child);
    prompt(
        address,
        &grandchild,
        "msg_rel15_grandchild",
        "continue in the nested fork",
    );
    manual_compaction::assert_carried(
        &root,
        "msg_rel15_grandchild",
        "session.execution.succeeded.1",
    );
    let request = request_body(&provider, 6);
    assert!(request.contains("REL15-current-rule") && request.contains("child completed"));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        7,
        "a fork replayed or failed before reaching the provider"
    );
    assert_eq!(
        turns(address, &parent),
        parent_before,
        "a child changed its parent"
    );
    product.stop_after_witness(&root);
}
