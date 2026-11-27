use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn committed_output_bounds_and_two_session_publication_are_advisory() {
    let _journey = process_journey_custody();
    const PUBLIC_BYTES: usize = 262_144;
    const RUNTIME_BYTES: usize = 1_048_576;

    let root = root("publication-bounds");
    prepare(&root, 20_000);
    let a_entered = Arc::new(Barrier::new(2));
    let release_a = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::Output(PUBLIC_BYTES),
        Script::Output(PUBLIC_BYTES + 1),
        Script::Output(RUNTIME_BYTES),
        Script::HeldOutput {
            bytes: PUBLIC_BYTES + 1,
            entered: Arc::clone(&a_entered),
            release: Arc::clone(&release_a),
        },
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);

    for (index, bytes) in [PUBLIC_BYTES, PUBLIC_BYTES + 1, RUNTIME_BYTES]
        .into_iter()
        .enumerate()
    {
        let session = create(address);
        let prompt_path = format!("/api/session/{session}/prompt");
        let messages_path = format!("/api/session/{session}/message?order=asc");
        let body = format!(
            r#"{{"id":"msg_bound_{index}","text":"bound {bytes}","delivery":"queue","resume":true}}"#
        );
        let (events, connected) = event_stream(address);
        let collector = thread::spawn(move || collect_until_terminal_or_gap(events, connected, 1));
        let response = json(address, "POST", &prompt_path, &body);
        assert_eq!(response["data"]["id"], format!("msg_bound_{index}"));
        let observed = collector
            .join()
            .unwrap_or_else(|_| panic!("threshold event collector panicked"));
        permission_residuals::wait_session_idle(address, &session);
        assert!(
            observed.matches("execution.succeeded").count() <= 1,
            "one execution published duplicate terminals"
        );
        // High-volume advisory frames may be lost before the first content frame.
        // Completion and exact captured bytes below remain mandatory.

        let messages = json(address, "GET", &messages_path, "");
        assert_eq!(messages["data"][0]["type"], "user");
        assert_eq!(messages["data"][1]["type"], "assistant");
        let content = &messages["data"][1]["content"][0];
        assert_eq!(content["state"]["durableBytes"], bytes);
        assert_eq!(
            content["text"].as_str().map(str::len).unwrap_or_default(),
            bytes.min(PUBLIC_BYTES)
        );
        assert_eq!(
            content["state"]["publicProjection"],
            if bytes > PUBLIC_BYTES {
                "truncated"
            } else {
                "complete"
            }
        );
        assert_eq!(content["state"]["durableCapture"], "complete");
    }

    let session_a = create(address);
    let session_b = create(address);
    let (events, connected) = event_stream(address);
    let collector = thread::spawn(move || collect_until_terminal_or_gap(events, connected, 2));
    let prompt_a = format!("/api/session/{session_a}/prompt");
    let a_address = address;
    let a = thread::spawn(move || {
        request(
            a_address,
            "POST",
            &prompt_a,
            r#"{"id":"msg_bound_a","text":"A","delivery":"queue","resume":true}"#,
        )
    });
    let _ = a_entered.wait();
    let active = json(address, "GET", "/api/session/active", "");
    assert!(
        active["data"].get(&session_a).is_some(),
        "coordinator active projection omitted barrier-held Session A"
    );
    let prompt_b = format!("/api/session/{session_b}/prompt");
    let (b_status, b_body) = request(
        address,
        "POST",
        &prompt_b,
        r#"{"id":"msg_bound_b","text":"B","delivery":"queue","resume":true}"#,
    );
    assert_eq!(
        b_status,
        200,
        "Session B lost its success while A was held: {}",
        String::from_utf8_lossy(&b_body)
    );
    permission_residuals::wait_session_idle(address, &session_b);
    let _ = release_a.wait();
    let (a_status, a_body) = a
        .join()
        .unwrap_or_else(|_| panic!("Session A prompt panicked"));
    assert_eq!(
        a_status,
        200,
        "Session A lost its success after B completed first: {}",
        String::from_utf8_lossy(&a_body)
    );
    let observed = collector
        .join()
        .unwrap_or_else(|_| panic!("two-session event collector panicked"));
    assert!(observed.matches("execution.succeeded").count() <= 2);
    permission_residuals::wait_session_idle(address, &session_a);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 5);

    let status = child
        .wait()
        .unwrap_or_else(|error| panic!("bounded publication product wait failed: {error}"));
    assert!(status.success());
    let store = EventStore::open(&root.join("database.sqlite3"), "publication-observations")
        .unwrap_or_else(|error| panic!("publication observations unreadable: {error}"));
    let succeeded: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM event WHERE type = 'session.execution.succeeded.1'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("execution outcomes unreadable: {error}"));
    let failed: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM event WHERE type = 'session.execution.failed.1'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("execution failures unreadable: {error}"));
    assert_eq!(
        (succeeded, failed),
        (5, 0),
        "publication loss rewrote canonical runtime outcomes"
    );
    drop(store);
    fs::remove_dir_all(&root)
        .unwrap_or_else(|error| panic!("{} cleanup failed: {error}", root.display()));
}

// An invalidated SSE stream closes so clients hydrate durable truth. It is
// not a provider failure. Bound both the network reads and retained bytes.
fn collect_until_terminal_or_gap(
    mut stream: TcpStream,
    mut observed: Vec<u8>,
    wanted: usize,
) -> String {
    let mut chunk = [0u8; 8192];
    for _ in 0..4096 {
        assert!(
            observed.len() <= 4 * 1024 * 1024,
            "publication witness byte bound exceeded"
        );
        if String::from_utf8_lossy(&observed)
            .matches("execution.succeeded")
            .count()
            >= wanted
        {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => observed.extend_from_slice(&chunk[..count]),
            Err(error) => panic!("publication read failed: {error}"),
        }
    }
    String::from_utf8_lossy(&observed).into_owned()
}
