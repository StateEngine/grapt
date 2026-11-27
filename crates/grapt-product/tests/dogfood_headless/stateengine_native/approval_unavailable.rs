use super::*;

#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn seat01_last_approver_disconnect_cancels_without_execution_and_reconnect_cannot_revive(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("seat01-approver-disconnect");
    prepare(&root, 90_000);
    configure(&root, "ready");
    let path = root.join("grapt.jsonc");
    let calls_file = root.join("mcp-calls.txt");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["mcp"]["stateengine"]["command"]
        .as_array_mut()
        .ok_or("command")?
        .extend([
            serde_json::json!("--calls-file"),
            serde_json::json!(calls_file),
        ]);
    fs::write(path, config.to_string())?;
    let tool = |call| Script::ToolInput {
        call,
        tool: "stateengine_stateengine_msg_read",
        arguments: serde_json::json!({"msg_id":"msg_native_long"}),
    };
    let provider = ProviderFixture::start(vec![
        tool("call_seat01_disconnect"),
        Script::Success,
        tool("call_seat01_reconnected"),
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create_native(address, &root);
    bind(address, &session, true);
    let attachment = approval::Attachment::open(address, &session);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"read fixture mail"}"#,
    );
    let pending = permission_boundaries::pending(address, &session, "call_seat01_disconnect");
    assert!(!fs::read_to_string(&calls_file)?
        .lines()
        .any(|name| name == "stateengine_msg_read"));
    drop(attachment);
    // poll checks the observation before the deadline, so an answer that
    // arrives during the final sleep is still seen. The old loop asked
    // `pending()` first and discarded exactly that answer.
    let answered = PollDeadline::within(Duration::from_secs(6))
        .poll(Duration::from_millis(10), || {
            (provider.calls.load(Ordering::SeqCst) == 2).then_some(())
        })
        .is_some();
    assert!(answered, "disconnect stranded a pending Ask");
    permission_residuals::wait_session_idle(address, &session);
    let output = permission_boundaries::output(&provider, "call_seat01_disconnect");
    assert!(
        output.contains("ApprovalUnavailable") && output.contains("no effect ran"),
        "{output}"
    );
    let _reconnected = approval::Attachment::open(address, &session);
    assert_ne!(
        request(
            address,
            "POST",
            &format!(
                "/api/session/{session}/permission/{}/reply",
                pending["id"].as_str().ok_or("permission")?
            ),
            r#"{"reply":"once"}"#
        )
        .0,
        204,
        "reconnect revived the cancelled request"
    );
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"new mail request"}"#,
    );
    let fresh = permission_boundaries::pending(address, &session, "call_seat01_reconnected");
    permission_boundaries::reply(address, &session, &fresh, "once");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    assert!(
        permission_boundaries::output(&provider, "call_seat01_reconnected")
            .contains("FULL_BODY_END")
    );
    assert_eq!(
        fs::read_to_string(&calls_file)?
            .lines()
            .filter(|name| *name == "stateengine_msg_read")
            .count(),
        1
    );
    child.stop_after_witness(&root);
    drop(child);
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let cancellations: i64 = db.query_row("SELECT COUNT(*) FROM event WHERE type = 'permission.cancelled.1' AND json_extract(data,'$.reason') = 'approval-unavailable'", [], |row| row.get(0))?;
    assert_eq!(
        cancellations, 1,
        "disconnect cancellation reason was not durable"
    );
    drop(db);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn seat01_headless_native_and_codemode_fail_promptly_and_profile_grants_execute(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for (nested, allow) in [(false, false), (true, false), (true, true)] {
        let root = root(&format!("seat01-headless-{nested}-{allow}"));
        prepare(&root, 90_000);
        configure_exposure(&root, "ready", Some(nested));
        let path = root.join("grapt.jsonc");
        let calls_file = root.join("mcp-calls.txt");
        let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        let command = config["mcp"]["stateengine"]["command"]
            .as_array_mut()
            .ok_or("fixture command")?;
        command.push(serde_json::json!("--calls-file"));
        command.push(serde_json::json!(calls_file));
        if allow {
            config["permission"] = serde_json::json!({"stateengine_*":{"stateengine/*":"allow","stateengine/*/*":"ask"}});
        }
        fs::write(&path, config.to_string())?;
        let provider = ProviderFixture::start(vec![
            Script::ToolInput {
                call: "call_seat01_headless",
                tool: if nested {
                    "execute"
                } else {
                    "stateengine_stateengine_msg_read"
                },
                arguments: if nested {
                    serde_json::json!({"code":"return await tools['stateengine_stateengine_msg_read']({msg_id:'msg_native_long'});"})
                } else {
                    serde_json::json!({"msg_id":"msg_native_long"})
                },
            },
            Script::SuccessText("continued after the tool result"),
        ]);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create_native(address, &root);
        bind(address, &session, true);
        // Admission acknowledges execution ownership, before native context
        // preparation and the first provider request. Do not charge that
        // independent startup work to the headless approval no-wait bound.
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"read the fixture mail"}"#,
        );
        let preparation_deadline =
            client_allowance::now() + client_allowance::CLI_REQUEST_ALLOWANCE;
        while provider.calls.load(Ordering::SeqCst) == 0 {
            assert!(
                client_allowance::now() < preparation_deadline,
                "native context preparation never reached the first provider request"
            );
            thread::sleep(Duration::from_millis(10));
        }
        // Keep the original six-second bound after the provider can emit the
        // tool call. A stranded thirty-second CodeMode Ask must still fail.
        // As above: expiry must not be checked before the observation, or an
        // answer inside the bound is thrown away by the last sleep.
        let answered = PollDeadline::within(Duration::from_secs(6))
            .poll(Duration::from_millis(10), || {
                (provider.calls.load(Ordering::SeqCst) == 2).then_some(())
            })
            .is_some();
        assert!(
            answered,
            "headless Ask waited instead of returning a model-visible failure"
        );
        permission_residuals::wait_session_idle(address, &session);
        let output = permission_boundaries::output(&provider, "call_seat01_headless");
        if allow {
            assert!(
                output.contains("FULL_BODY_END"),
                "scoped profile grant failed: {output}"
            );
        } else {
            assert!(
                output.contains("ApprovalUnavailable")
                    && output.contains("approval unavailable (headless)"),
                "headless failure lost its type or explanation: {output}"
            );
            assert!(
                !output.contains("EffectUnknown") && !output.contains("FULL_BODY_END"),
                "headless failure pretended an effect ran: {output}"
            );
        }
        let calls = fs::read_to_string(&calls_file)?;
        assert_eq!(
            calls
                .lines()
                .filter(|name| *name == "stateengine_msg_read")
                .count(),
            usize::from(allow),
            "refused MCP call reached the server: {calls}"
        );
        assert_eq!(
            json(
                address,
                "GET",
                &format!("/api/session/{session}/permission"),
                ""
            )["data"],
            serde_json::json!([])
        );
        child.stop_after_witness(&root);
        drop(child);
        let db = rusqlite::Connection::open_with_flags(
            root.join("database.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let failures: i64 = db.query_row("SELECT COUNT(*) FROM event WHERE json_extract(data,'$.outcome.reason') = 'approval-unavailable'", [], |row| row.get(0))?;
        assert_eq!(
            failures,
            if allow {
                0
            } else if nested {
                2
            } else {
                1
            },
            "headless failure was not retained in native/nested durable terminals"
        );
        let asks: i64 = db.query_row(
            "SELECT COUNT(*) FROM event WHERE type = 'permission.asked.2'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            asks, 0,
            "an initially headless refusal published an orphan Ask"
        );
        drop(db);
        process_fixture::cleanup(&root);
    }
    Ok(())
}
