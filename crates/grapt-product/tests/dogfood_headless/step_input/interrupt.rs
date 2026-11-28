//! Real endpoint responses while the previous executor still holds custody.
use super::*;

#[test]
fn stop_only_callers_leave_queued_input_pending_instead_of_starting_another_execution() -> TestResult
{
    let _journey = process_journey_custody();
    for (label, body) in [
        ("quit-stop-only", "{}"),
        ("quit-stop-false", r#"{"resumePending":false}"#),
    ] {
        let (root, provider, mut child, address, session) = setup(label)?;
        submit(address, &session, "msg_quit_pending", FIRST);
        let (status, _) = request(
            address,
            "POST",
            &format!("/api/session/{session}/interrupt"),
            body,
        );
        assert_eq!(status, 204);
        permission_residuals::wait_session_idle(address, &session);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(users(address, &session), ["run the three step task"]);
        let page = json(
            address,
            "GET",
            &format!("/api/session/{session}/pending"),
            "",
        );
        assert_eq!(page["data"][0]["id"], "msg_quit_pending");
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&workspace_dir(&root));
        process_fixture::cleanup(&root);
    }
    Ok(())
}

fn held_restart(concurrent_stop: bool) -> TestResult {
    let label = if concurrent_stop {
        "esc-concurrent-stop-custody"
    } else {
        "esc-custody-timeout"
    };
    let (root, provider, mut child, address, session) = setup_with_env(
        label,
        false,
        &[("GRAPT_FIXTURE_HOLD_AT", "execution-interrupted")],
    )?;
    submit(address, &session, "msg_esc_still_pending", FIRST);
    let active = json(address, "GET", "/api/session/active", "");
    let old = active["data"][&session]["executionID"].clone();
    let route = format!("/api/session/{session}/interrupt");
    let body = serde_json::json!({"executionID":old,"resumePending":true}).to_string();
    let (sent, received) = std::sync::mpsc::sync_channel(1);
    let endpoint = route.clone();
    let waiting = thread::spawn(move || {
        let result =
            request_with_read_timeout(address, "POST", &endpoint, &body, Duration::from_secs(45));
        let _ = sent.send(result);
    });
    let reached = permission_residuals::wait_until(
        Duration::from_secs(10),
        Duration::from_millis(10),
        || {
            fs::read_to_string(root.join("product-stderr-0.log"))
                .is_ok_and(|log| log.contains("secode-fixture boundary=execution-interrupted"))
        },
    );
    assert!(
        reached.is_some(),
        "the actual executor never reached the held pre-terminal boundary"
    );
    assert!(
        matches!(
            received.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "Esc acknowledged before the old execution reached its terminal"
    );
    if concurrent_stop {
        let (status, bytes) = request(
            address,
            "POST",
            &route,
            r#"{"executionID":"exe_unrelated"}"#,
        );
        assert_eq!(status, 400);
        assert!(String::from_utf8_lossy(&bytes).contains("execution identity changed"));
        let (status, bytes) = request_with_read_timeout(
            address,
            "POST",
            &route,
            &serde_json::json!({"executionID":old,"stopProcesses":true}).to_string(),
            Duration::from_secs(45),
        );
        assert_eq!(
            status, 400,
            "a joined stop cannot confirm cleanup while custody is held"
        );
        assert!(String::from_utf8_lossy(&bytes).contains("execution-owner completion"));
    }
    let (status, bytes) = received.recv_timeout(Duration::from_secs(45))?;
    waiting
        .join()
        .map_err(|_| "interrupt request worker panicked")?;
    assert_eq!(status, 400, "{}", String::from_utf8_lossy(&bytes));
    let error = String::from_utf8_lossy(&bytes);
    assert!(
        error.contains("Session stop incomplete") && error.contains("execution-owner completion")
    );
    let active = json(address, "GET", "/api/session/active", "");
    assert_eq!(
        active["data"][&session]["executionID"], old,
        "the old owner must still hold custody for this witness"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(users(address, &session), ["run the three step task"]);
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/pending"),
        "",
    );
    assert_eq!(page["data"][0]["id"], "msg_esc_still_pending");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&workspace_dir(&root));
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn a_concurrent_stop_cannot_confirm_cleanup_before_old_custody_releases() -> TestResult {
    let _journey = process_journey_custody();
    held_restart(true)
}

#[test]
fn a_restart_deadline_reports_incomplete_stop_without_losing_the_input() -> TestResult {
    let _journey = process_journey_custody();
    held_restart(false)
}

#[test]
fn ordinary_and_destructive_callers_both_confirm_one_released_stop() -> TestResult {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session) = setup_with_spawner(
        "joined-stop-completion",
        false,
        &[("GRAPT_FIXTURE_HOLD_AT", "execution-interrupted")],
        serde_json::json!({"*":"allow"}),
        process_fixture::spawn_held_with,
    )?;
    submit(address, &session, "msg_joined_stop_pending", FIRST);
    let active = json(address, "GET", "/api/session/active", "");
    let old = active["data"][&session]["executionID"]
        .as_str()
        .ok_or("old execution identity absent")?;
    let joined = serde_json::json!({"executionID":old,"stopProcesses":true,"resumePending":true})
        .to_string();
    let route = format!("/api/session/{session}/interrupt");
    let (sent, received) = std::sync::mpsc::sync_channel(2);
    let first_route = route.clone();
    let first_reply = sent.clone();
    let first = thread::spawn(move || {
        let _sent = first_reply.send(request_with_read_timeout(
            address,
            "POST",
            &first_route,
            "{}",
            Duration::from_secs(45),
        ));
    });
    assert!(
        permission_residuals::wait_until(
            Duration::from_secs(10),
            Duration::from_millis(10),
            || {
                fs::read_to_string(root.join("product-stderr-0.log"))
                    .is_ok_and(|log| log.contains("secode-fixture boundary=execution-interrupted"))
            },
        )
        .is_some(),
        "the executor did not reach its interruption boundary"
    );
    let second = thread::spawn(move || {
        let _sent = sent.send(request_with_read_timeout(
            address,
            "POST",
            &route,
            &joined,
            Duration::from_secs(45),
        ));
    });
    assert!(
        matches!(
            received.recv_timeout(Duration::from_millis(250)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "a caller completed before the held owner was released"
    );
    drop(child.stdin.take().ok_or("fixture release pipe is absent")?);
    for _ in 0..2 {
        let (status, bytes) = received.recv_timeout(Duration::from_secs(45))?;
        assert_eq!(status, 204, "{}", String::from_utf8_lossy(&bytes));
    }
    first
        .join()
        .map_err(|_| "first interrupt worker panicked")?;
    second
        .join()
        .map_err(|_| "joined interrupt worker panicked")?;
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert_eq!(users(address, &session), ["run the three step task", FIRST]);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&workspace_dir(&root));
    process_fixture::cleanup(&root);
    Ok(())
}
