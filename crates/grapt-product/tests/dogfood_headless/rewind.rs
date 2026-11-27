//! RW: conversation cuts survive restart without resurrecting compacted input.
use super::*;

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
    );
}

fn history(address: SocketAddr, session: &str) -> String {
    json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )
    .to_string()
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn rw_restart_and_compaction_cannot_resurrect_discarded_context() {
    let _journey = process_journey_custody();
    let root = root("rewind-restart");
    prepare(&root, 120_000);
    fs::write(root.join("seed.txt"), vec![b's'; 7_524])
        .unwrap_or_else(|error| panic!("seed write: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_rewind_seed",
            path: "seed.txt",
        },
        Script::SuccessText("KEPT_ASSISTANT_MARKER"),
        Script::SuccessText("DISCARDED_ASSISTANT_MARKER"),
        Script::SuccessText("## Objective\n- TAINTED_SUMMARY_MARKER"),
        Script::SuccessText("CARRYING_ANSWER_MARKER"),
        Script::SuccessText("NEW_REPLY_MARKER"),
        Script::SuccessText("## Objective\n- clean retained history"),
        Script::SuccessText("FINAL_REPLY_MARKER"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt(
        address,
        &session,
        "msg_rewind_kept",
        "read seed.txt; KEPT_USER_MARKER",
    );
    compaction_continuity::approve(address, &session, "call_rewind_seed");
    wait_calls(&provider.calls, 2);
    permission_residuals::wait_session_idle(address, &session);
    prompt(
        address,
        &session,
        "msg_rewind_discarded",
        &format!("DISCARDED_USER_MARKER {}", "detail ".repeat(300)),
    );
    wait_calls(&provider.calls, 3);
    permission_residuals::wait_session_idle(address, &session);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    prompt(
        address,
        &session,
        "msg_rewind_carrying",
        "CARRYING_USER_MARKER",
    );
    wait_calls(&provider.calls, 5);
    permission_residuals::wait_session_idle(address, &session);
    assert!(history(address, &session).contains("TAINTED_SUMMARY_MARKER"));

    fs::write(root.join("seed.txt"), "human edit")
        .unwrap_or_else(|error| panic!("human edit: {error}"));
    let preview = json(
        address,
        "POST",
        &format!("/api/session/{session}/rewind/preview"),
        r#"{"prompts":2}"#,
    );
    assert_eq!(preview["data"]["messageID"], "msg_rewind_discarded");
    let request =
        serde_json::json!({"messageID":preview["data"]["messageID"],"tip":preview["data"]["tip"]})
            .to_string();
    let committed = json(
        address,
        "POST",
        &format!("/api/session/{session}/rewind"),
        &request,
    );
    let after = history(address, &session);
    assert!(after.contains("KEPT_USER_MARKER"));
    for removed in [
        "DISCARDED_USER_MARKER",
        "DISCARDED_ASSISTANT_MARKER",
        "TAINTED_SUMMARY_MARKER",
        "CARRYING_USER_MARKER",
        "CARRYING_ANSWER_MARKER",
    ] {
        assert!(!after.contains(removed), "history resurrected {removed}");
    }
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    assert_eq!(history(address, &session), after);
    let retry = json(
        address,
        "POST",
        &format!("/api/session/{session}/rewind"),
        &request,
    );
    assert_eq!(retry, committed, "lost-response retry appended another cut");
    prompt(
        address,
        &session,
        "msg_rewind_new",
        "continue with retained context",
    );
    wait_calls(&provider.calls, 6);
    permission_residuals::wait_session_idle(address, &session);
    {
        let bodies = locked(&provider.bodies);
        assert!(bodies[5].contains("KEPT_USER_MARKER"));
        for removed in [
            "DISCARDED_USER_MARKER",
            "DISCARDED_ASSISTANT_MARKER",
            "TAINTED_SUMMARY_MARKER",
            "CARRYING_USER_MARKER",
            "CARRYING_ANSWER_MARKER",
        ] {
            assert!(
                !bodies[5].contains(removed),
                "provider context resurrected {removed}"
            );
        }
    }
    json(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    prompt(
        address,
        &session,
        "msg_rewind_final",
        "continue after clean compaction",
    );
    wait_calls(&provider.calls, 8);
    permission_residuals::wait_session_idle(address, &session);
    assert!(history(address, &session).contains("FINAL_REPLY_MARKER"));
    assert_eq!(
        fs::read_to_string(root.join("seed.txt"))
            .unwrap_or_else(|error| panic!("read edit: {error}")),
        "human edit"
    );
    child.stop_after_witness(&root);
    drop(child);
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("read ledger: {error}"));
    let cuts: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM event WHERE type = 'session.revert.committed.1'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("count cuts: {error}"));
    assert_eq!(cuts, 1);
    let epochs: Vec<i64> = db.prepare("SELECT json_extract(data, '$.targetEpoch') FROM event WHERE type = 'session.compaction.ended.2' ORDER BY seq")
        .unwrap_or_else(|error| panic!("epoch query: {error}"))
        .query_map([], |row| row.get(0)).unwrap_or_else(|error| panic!("epoch read: {error}"))
        .map(|row| row.unwrap_or_else(|error| panic!("epoch row: {error}"))).collect();
    assert_eq!(
        epochs,
        [1, 2],
        "rewind must preserve physical compaction generations"
    );
    drop(db);
    process_fixture::cleanup(&root);
}
