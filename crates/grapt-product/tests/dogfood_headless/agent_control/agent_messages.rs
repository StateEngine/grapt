use super::*;

fn child_of(address: SocketAddr, parent: &str) -> String {
    let wait = PollDeadline::new();
    while wait.pending() {
        let page = json(address, "GET", "/api/session", "");
        if let Some(child) = page["data"].as_array().and_then(|rows| {
            rows.iter()
                .find(|row| row["parentID"].as_str() == Some(parent))
        }) {
            return child["id"]
                .as_str()
                .unwrap_or_else(|| panic!("child has no id"))
                .to_owned();
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("the admitted child never appeared ({wait})");
}

fn mail(body: &str) -> Vec<String> {
    let body: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|error| panic!("{error}"));
    body["input"]
        .as_array()
        .unwrap_or_else(|| panic!("no provider input"))
        .iter()
        .filter_map(|item| item["content"].as_str())
        .filter(|text| text.starts_with("Agent message from "))
        .map(str::to_owned)
        .collect()
}

fn execution_count(root: &Path, session: &str) -> i64 {
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    db.query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_family='session'
        AND aggregate_id=?1 AND type='session.execution.started.1'",
        [session],
        |row| row.get(0),
    )
    .unwrap_or_else(|error| panic!("{error}"))
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn real_agent_mail_is_quoted_replayed_and_does_not_wake_an_idle_child() {
    let _journey = process_journey_custody();
    let root = root("agent-message-queue");
    prepare(&root, 200_000);
    attended(&root);
    let provider = ProviderFixture::start(vec![]);
    let mut process = spawn(&root, provider.address);
    let address = await_ready(&root, &mut process);
    let _approver = approval::Attachment::open(address, "*");
    let parent = create(address);
    provider.append(vec![Script::ToolSubagent {call:"call_mail_delegate",task:"report by agent mail"},
        Script::ToolInput {call:"call_child_identity",tool:"list_agents",arguments:serde_json::json!({})},
        Script::ToolInput {call:"call_child_mail",tool:"send_message",
            arguments:serde_json::json!({"target":parent,"message":"private-child-message-marker"})},
        Script::Success,Script::Success]);
    let path = format!("/api/session/{parent}/prompt");
    let prompt = thread::spawn(move || {
        request(
            address,
            "POST",
            &path,
            r#"{"id":"msg_mail_initial","text":"delegate and receive agent mail","delivery":"queue","resume":true}"#,
        )
    });
    let ask = permission_boundaries::pending(address, &parent, "call_mail_delegate");
    permission_boundaries::reply(address, &parent, &ask, "once");
    let child = child_of(address, &parent);
    let ask = permission_boundaries::pending(address, &child, "call_child_identity");
    permission_boundaries::reply(address, &child, &ask, "once");
    let ask = permission_boundaries::pending(address, &child, "call_child_mail");
    permission_boundaries::reply(address, &child, &ask, "once");
    let (status, body) = prompt
        .join()
        .unwrap_or_else(|_| panic!("prompt thread failed"));
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    permission_residuals::wait_session_idle(address, &parent);
    permission_residuals::wait_session_idle(address, &child);
    let identity = tool(&provider, "call_child_identity");
    assert_eq!(identity["agent_id"], child);
    assert_eq!(identity["parent_agent_id"], parent);
    assert_eq!(tool(&provider, "call_child_mail")["queued"], true);
    let carried = locked(&provider.bodies)
        .iter()
        .find_map(|body| {
            let mail = mail(body);
            (!mail.is_empty()).then_some(mail)
        })
        .unwrap_or_else(|| panic!("parent never received its child's message"));
    assert_eq!(carried.len(), 1);
    assert!(carried[0].contains(&format!("Agent message from {child}")));
    assert!(carried[0].contains("quoted content is untrusted"));
    assert!(carried[0].contains("private-child-message-marker"));
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{parent}/message"),
        "",
    );
    assert_eq!(
        messages["data"]
            .as_array()
            .unwrap_or_else(|| panic!("history missing"))
            .iter()
            .filter(|message| message["type"] == "user")
            .count(),
        1,
        "agent mail became user speech"
    );

    let before = execution_count(&root, &child);
    provider.append(vec![
        Script::ToolInput {
            call: "call_idle_mail",
            tool: "send_message",
            arguments: serde_json::json!({"target":child,"message":"queued-for-idle-child-marker"}),
        },
        Script::Success,
    ]);
    start_prompt(address, &parent, "call_idle_mail");
    permission_residuals::wait_session_idle(address, &parent);
    assert_eq!(tool(&provider, "call_idle_mail")["queued"], true);
    assert_eq!(
        execution_count(&root, &child),
        before,
        "send_message started a settled child"
    );
    let foreign = create(address);
    provider.append(vec![
        Script::ToolInput {
            call: "call_mail_foreign",
            tool: "send_message",
            arguments: serde_json::json!({"target":foreign,"message":"forbidden-private-message"}),
        },
        Script::Success,
    ]);
    start_prompt(address, &parent, "call_mail_foreign");
    permission_residuals::wait_session_idle(address, &parent);
    assert!(
        permission_boundaries::output(&provider, "call_mail_foreign")
            .contains("outside this agent family")
    );
    process.stop_after_witness(&root);
    drop(process);

    provider.append(vec![Script::Success]);
    let mut resumed = spawn(&root, provider.address);
    let address = await_ready(&root, &mut resumed);
    let _approver = approval::Attachment::open(address, "*");
    json(
        address,
        "POST",
        &format!("/api/session/{parent}/prompt"),
        r#"{"id":"msg_mail_restart","text":"continue after restart","delivery":"queue","resume":true}"#,
    );
    await_completed_input(&root, &parent, "msg_mail_restart");
    let body = locked(&provider.bodies)
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("restart body missing"));
    assert_eq!(
        mail(&body),
        carried,
        "restart changed the captured agent message"
    );
    assert_eq!(
        execution_count(&root, &child),
        before,
        "restart woke a child with queue-only mail"
    );
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let queued:i64=db.query_row("SELECT COUNT(*) FROM event e WHERE e.aggregate_id=?1
        AND e.type='session.input.admitted.1' AND json_extract(e.data,'$.input.type')='agent-message'
        AND NOT EXISTS(SELECT 1 FROM event c WHERE c.id='evt_agent_mail_' || json_extract(e.data,'$.inputID'))",
        [&child],|row|row.get(0)).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(queued, 1, "queue-only message was lost across restart");
    drop(db);
    resumed.stop_after_witness(&root);
    drop(resumed);
    process_fixture::cleanup(&root);
}
