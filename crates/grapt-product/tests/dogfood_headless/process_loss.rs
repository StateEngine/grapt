use super::*;

#[test]
fn request_sent_process_loss_recovers_on_restart_without_a_second_provider_call() {
    let _journey = process_journey_custody();
    let root = root("d1-process-loss");
    prepare(&root, 3_000);
    let provider = ProviderFixture::start(vec![Script::StallBeforeFirstFragment]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let prompt_path = format!("/api/session/{session}/prompt");
    let messages_path = format!("/api/session/{session}/message?order=asc");
    let prompt =
        r#"{"id":"msg_d1_process_loss","text":"survive restart","delivery":"queue","resume":true}"#;
    let admitted = json(address, "POST", &prompt_path, prompt);
    assert_eq!(admitted["data"]["id"], "msg_d1_process_loss");
    wait_calls(&provider.calls, 1);
    kill_for_restart(&root, &mut child);

    let mut restarted = spawn(&root, provider.address);
    let restarted_address = await_ready(&root, &mut restarted);
    let messages = json(restarted_address, "GET", &messages_path, "");
    assert_eq!(messages["data"][0]["type"], "user");
    assert_eq!(messages["data"][0]["id"], "msg_d1_process_loss");
    assert_eq!(messages["data"][0]["text"], "survive restart");
    assert_eq!(
        messages["data"].as_array().map(Vec::len),
        Some(2),
        "process loss invented messages beyond the durable user and opened Step"
    );
    assert_eq!(messages["data"][1]["type"], "assistant");
    assert_eq!(
        messages["data"][1]["content"],
        serde_json::json!([]),
        "process loss invented assistant output"
    );
    assert!(
        json(restarted_address, "GET", "/api/session/active", "")["data"][&session].is_null(),
        "restarted process projected a dead execution as active"
    );
    let (retry_status, _) = request(restarted_address, "POST", &prompt_path, prompt);
    assert_eq!(
        retry_status, 200,
        "stable retry did not reconcile after process loss"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "restart or stable retry repeated the provider call"
    );
    let status = restarted
        .wait()
        .unwrap_or_else(|error| panic!("process-loss restart wait failed: {error}"));
    assert!(status.success());

    let store = EventStore::open(&root.join("database.sqlite3"), "d1-process-loss-inspection")
        .unwrap_or_else(|error| panic!("process-loss inspection store failed: {error}"));
    let terminal_count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.step.failed.2'",
            [&session],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("process-loss terminal count failed: {error}"));
    let receipt_count: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM attempt_receipt", [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("process-loss receipt count failed: {error}"));
    assert_eq!((terminal_count, receipt_count), (1, 0));
    drop(store);
    fs::remove_dir_all(&root)
        .unwrap_or_else(|error| panic!("{} cleanup failed: {error}", root.display()));
}

/// CLAIM-2: text the user already saw stream survives a killed server, as a
/// partial answer marked incomplete.
#[test]
fn text_streamed_before_a_process_loss_comes_back_marked_incomplete() {
    let _journey = process_journey_custody();
    let root = root("claim2-partial-text");
    // Room for the streamed pause and the bounded wait below before the
    // serve window closes.
    prepare(&root, 10_000);
    let provider = ProviderFixture::start(vec![Script::TextThenStall]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let prompt = r#"{"id":"msg_claim2_partial","text":"answer slowly","delivery":"queue"}"#;
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        prompt,
    );
    // Kill once the newest text is durable, as the cadence promises, or after
    // a bound, so a missing checkpoint fails on what the user sees below.
    let database = root.join("database.sqlite3");
    let wait = PollDeadline::within(Duration::from_secs(5));
    while checkpoints(&database) < 1 && wait.pending() {
        thread::sleep(Duration::from_millis(20));
    }
    kill_for_restart(&root, &mut child);

    let mut restarted = spawn(&root, provider.address);
    let restarted_address = await_ready(&root, &mut restarted);
    let messages = json(
        restarted_address,
        "GET",
        &format!("/api/session/{session}/message?order=asc"),
        "",
    );
    let assistant = &messages["data"][1];
    assert_eq!(assistant["type"], "assistant", "{messages}");
    assert_eq!(
        assistant["content"][0]["text"], "Streamed before the crash.",
        "{assistant}"
    );
    assert_eq!(assistant["finish"], "error", "{assistant}");
    assert_eq!(
        assistant["error"]["message"],
        "The server stopped before this answer finished; the text above is incomplete."
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    restarted
        .wait()
        .unwrap_or_else(|error| panic!("claim-2 restart wait failed: {error}"));
    drop(provider);
    fs::remove_dir_all(&root)
        .unwrap_or_else(|error| panic!("{} cleanup failed: {error}", root.display()));
}

/// Committed text checkpoints, read without opening the store as a writer.
fn checkpoints(database: &std::path::Path) -> i64 {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
    rusqlite::Connection::open_with_flags(database, flags)
        .and_then(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM event WHERE type = 'session.content.checkpointed.1'",
                [],
                |row| row.get(0),
            )
        })
        .unwrap_or(0)
}

/// Kill the server and clear what its supervisor clears before a relaunch.
fn kill_for_restart(root: &std::path::Path, child: &mut std::process::Child) {
    child
        .kill()
        .unwrap_or_else(|error| panic!("process-loss kill failed: {error}"));
    child
        .wait()
        .unwrap_or_else(|error| panic!("process-loss wait failed: {error}"));
    // The production service manager owns stale registration cleanup before
    // relaunch. This witness invokes the binary directly, so model that exact
    // supervisor act without touching the durable database under test.
    for registration in ["registration", "registration.elected", "registration.lock"] {
        let path = root.join(registration);
        if path.exists() {
            fs::remove_file(&path)
                .unwrap_or_else(|error| panic!("stale {registration} removal failed: {error}"));
        }
    }
    let ready = root.join("serve-ready.json");
    if ready.exists() {
        fs::remove_file(&ready)
            .unwrap_or_else(|error| panic!("stale readiness removal failed: {error}"));
    }
}
