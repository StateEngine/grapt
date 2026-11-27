use super::*;

#[test]
fn completed_and_failed_calls_close_the_window_but_an_unrun_call_does_not() {
    let _journey = process_journey_custody();
    let root = root("approval-terminal-window");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    fs::write(root.join("present.txt"), b"terminal evidence")
        .unwrap_or_else(|error| panic!("approval fixture write failed: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_approval_complete",
            path: "present.txt",
        },
        Script::ToolRead {
            call: "call_approval_error",
            path: "absent.txt",
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
        r#"{"id":"msg_approval_terminals","text":"read both named files","delivery":"queue","resume":true}"#,
    );
    approve_batch(
        address,
        &session,
        &["call_approval_complete", "call_approval_error"],
    );
    permission_residuals::wait_session_idle(address, &session);
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    for (call, expected) in [
        ("call_approval_complete", "completed"),
        ("call_approval_error", "error"),
    ] {
        let part = page["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .find(|part| part["id"] == call)
            .unwrap_or_else(|| panic!("public history omitted named terminal {call}: {page}"));
        assert_eq!(part["state"]["status"], expected, "{page}");
    }
    let missing = std::panic::catch_unwind(|| {
        approve_batch(
            address,
            &session,
            &["call_approval_complete", "call_approval_never_ran"],
        );
    });
    let reason = missing
        .err()
        .unwrap_or_else(|| panic!("idle Session accepted an unrun named call"));
    let reason = reason
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| reason.downcast_ref::<&str>().copied())
        .unwrap_or_else(|| panic!("missing-call refusal had no readable reason"));
    assert!(
        reason.contains("went idle without running call_approval_never_ran"),
        "unexpected missing-call refusal: {reason}"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
fn single_and_batch_windows_observe_terminals_while_the_next_response_is_held_even_at_expiry() {
    let _journey = process_journey_custody();
    // Nine separate responses place the first named terminal beyond page one.
    const CALLS: [&str; 9] = [
        "call_approval_live_one",
        "call_approval_live_two",
        "call_approval_live_three",
        "call_approval_live_four",
        "call_approval_live_five",
        "call_approval_live_six",
        "call_approval_live_seven",
        "call_approval_live_eight",
        "call_approval_live_nine",
    ];
    for count in [1, CALLS.len()] {
        let root = root(&format!("approval-held-window-{count}"));
        let _cleanup = terminal_coordination::RootCleanup(root.clone());
        prepare(&root, 120_000);
        fs::write(root.join("present.txt"), b"held-response terminal evidence")
            .unwrap_or_else(|error| panic!("approval fixture write failed: {error}"));
        let entered = Arc::new(terminal_coordination::Phase::new());
        let release = Arc::new(terminal_coordination::Phase::new());
        let mut scripts: Vec<Script> = CALLS[..count]
            .iter()
            .map(|call| Script::ToolRead {
                call,
                path: "present.txt",
            })
            .collect();
        scripts.push(Script::HeldTerminalOutput {
            bytes: 1,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        let provider = ProviderFixture::start(scripts);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"id":"msg_approval_held","text":"read and retain the named facts","delivery":"queue","resume":true}"#,
        );
        entered
            .wait("approval next response held", &provider.stop)
            .unwrap_or_else(|error| panic!("{error}"));
        approve_batch(address, &session, &CALLS[..count]);
        // No poll remains, but the committed terminals must still be read.
        approve_batch_until(
            address,
            &session,
            &CALLS[..count],
            PollDeadline::within(Duration::ZERO),
        );
        let missing = std::panic::catch_unwind(|| {
            approve_batch_until(
                address,
                &session,
                &["call_approval_never_ran"],
                PollDeadline::within(Duration::ZERO),
            );
        });
        let reason = missing
            .err()
            .and_then(|reason| reason.downcast::<String>().ok())
            .unwrap_or_else(|| panic!("expired approval omitted its named missing-call refusal"));
        assert!(reason.contains("the chain stopped before call_approval_never_ran"));
        assert!(
            json(address, "GET", "/api/session/active", "")["data"]
                .as_object()
                .is_some_and(|active| active.contains_key(&session)),
            "approval waited for the held provider response instead of named terminals"
        );
        release.release();
        permission_residuals::wait_session_idle(address, &session);
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&root);
    }
}
