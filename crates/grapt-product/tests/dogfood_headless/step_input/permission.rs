//! Steering preserves permission provenance from its original execution.
use super::*;

#[test]
fn a_rejected_permission_after_steering_cannot_poison_the_chain_or_next_input() -> TestResult {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session) = setup_with_permission(
        "step-permission-steer",
        false,
        &[],
        serde_json::json!({"*":"allow", "read":"ask"}),
    )?;
    let _approver = approval::Attachment::open(address, "*");
    submit(address, &session, "msg_step_permission_steer", FIRST);
    fs::write(workspace_dir(&root).join("step-release"), "release")?;
    let path = format!("/api/session/{session}/permission");
    let ask =
        permission_residuals::wait_tool_ask("call_second_step", || json(address, "GET", &path, ""));
    let id = ask["id"].as_str().ok_or("permission identity missing")?;
    let (status, body) = request(
        address,
        "POST",
        &format!("{path}/{id}/reply"),
        r#"{"reply":"reject"}"#,
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "a rejected ask after steering must leave the next model Step usable"
    );
    submit(
        address,
        &session,
        "msg_after_permission",
        "continue after that refusal",
    );
    // Idleness can precede this admitted input's provider request.
    let sent = permission_residuals::wait_until(
        client_allowance::CLI_REQUEST_ALLOWANCE,
        permission_residuals::SETTLE_POLL,
        || provider.calls.load(Ordering::SeqCst) >= 4,
    );
    assert!(sent.is_some(), "the next input never reached the provider");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    assert_eq!(ask["source"]["messageID"], "msg_step_initial");
    provider.append(vec![Script::SuccessText("FOREIGN_SOURCE_WAS_ACCEPTED")]);
    // Neither a busy Steer nor a later execution owns the earlier ask.
    for (forged, next) in [
        ("msg_step_permission_steer", "msg_forged_steer"),
        ("msg_after_permission", "msg_foreign_permission"),
    ] {
        let diagnostic_path = root.join("product-stderr-0.log");
        let diagnostic_start = fs::metadata(&diagnostic_path)?.len();
        // Corrupt only this disposable fixture's exact permission event.
        {
            let connection = rusqlite::Connection::open(root.join("database.sqlite3"))?;
            connection.busy_timeout(Duration::from_secs(5))?;
            assert_eq!(
                connection.execute(
                    "UPDATE event SET data=json_set(data, '$.source.messageID', ?1)
                     WHERE aggregate_family='session' AND aggregate_id=?2
                       AND type='permission.asked.2'
                       AND json_extract(data, '$.source.callID')='call_second_step'",
                    rusqlite::params![forged, session],
                )?,
                1,
                "the foreign-input control must change its exact permission"
            );
        }
        submit(
            address,
            &session,
            next,
            "continue with the forged permission fixture",
        );
        use std::io::{Seek, SeekFrom};
        // Idleness does not wait for this input's refusal diagnostic.
        let wait = PollDeadline::within(client_allowance::CLI_REQUEST_ALLOWANCE);
        let diagnostic = loop {
            let mut diagnostic = String::new();
            let mut file = fs::File::open(&diagnostic_path)?;
            file.seek(SeekFrom::Start(diagnostic_start))?;
            file.take(65_536).read_to_string(&mut diagnostic)?;
            if diagnostic.contains("tool permission names a different originating input")
                || !wait.pending()
            {
                break diagnostic;
            }
            thread::sleep(permission_residuals::SETTLE_POLL);
        };
        assert!(diagnostic.contains("tool permission names a different originating input"));
        permission_residuals::wait_session_idle(address, &session);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            4,
            "the forged permission source {forged} must still be refused"
        );
    }
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
