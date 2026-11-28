//! Ordinary composer input joins the next model Step through the existing inbox.
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const FIRST: &str = "STEP_BOUNDARY_USER_FIRST";
const SECOND: &str = "STEP_BOUNDARY_USER_SECOND";
/// The input admitted while the tool still runs: a parked owner waits for
/// the running allowance instead of spending it.
const QUEUED: &str = "a queued input runs after the others";
const FINISHED: &str = "STEP_ONE_TOOL_FINISHED";

#[cfg(feature = "handover-crash-fixture")]
#[path = "step_input/interrupt.rs"]
mod interrupt;

#[path = "step_input/permission.rs"]
mod permission;

fn submit(address: SocketAddr, session: &str, id: &str, text: &str) -> serde_json::Value {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":text}).to_string(),
    )
}

fn users(address: SocketAddr, session: &str) -> Vec<String> {
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    page["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history missing: {page}"))
        .iter()
        .filter(|row| row["type"] == "user")
        .map(|row| {
            row["text"]
                .as_str()
                .unwrap_or_else(|| panic!("user text missing"))
                .to_owned()
        })
        .rev()
        .collect()
}

type Fixture = (
    PathBuf,
    ProviderFixture,
    process_fixture::ManagedChild,
    SocketAddr,
    String,
);

fn setup(label: &str) -> Result<Fixture, Box<dyn std::error::Error>> {
    setup_second(label, false)
}

fn setup_second(label: &str, hold_second: bool) -> Result<Fixture, Box<dyn std::error::Error>> {
    setup_with_env(label, hold_second, &[])
}

fn setup_with_env(
    label: &str,
    hold_second: bool,
    env: &[(&str, &str)],
) -> Result<Fixture, Box<dyn std::error::Error>> {
    setup_with_permission(label, hold_second, env, serde_json::json!({"*":"allow"}))
}

fn setup_with_permission(
    label: &str,
    hold_second: bool,
    env: &[(&str, &str)],
    permission: serde_json::Value,
) -> Result<Fixture, Box<dyn std::error::Error>> {
    setup_with_spawner(
        label,
        hold_second,
        env,
        permission,
        process_fixture::spawn_controlled_with,
    )
}

fn workspace_dir(root: &Path) -> PathBuf {
    root.with_extension("workspace")
}

fn setup_with_spawner(
    label: &str,
    hold_second: bool,
    env: &[(&str, &str)],
    permission: serde_json::Value,
    spawn: impl FnOnce(&Path, SocketAddr, &[(&str, &str)]) -> process_fixture::ManagedChild,
) -> Result<Fixture, Box<dyn std::error::Error>> {
    let root = root(label);
    let workspace = workspace_dir(&root);
    fs::create_dir_all(&workspace)?;
    prepare(&root, 90_000);
    unattended(&root);
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["permission"] = permission;
    fs::write(path, serde_json::to_vec(&config)?)?;
    fs::write(workspace.join("step-two.txt"), "second tool completed")?;
    let command = format!(
        "printf ready > '{}/step-ready'; while [ ! -f '{}/step-release' ]; do sleep 0.02; done; printf {FINISHED}",
        workspace.display(), workspace.display());
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_first_step",
            tool: "exec_command",
            arguments: serde_json::json!({
                "cmd":command, "workdir":workspace, "yield_time_ms":10000, "timeout_ms":20000, "max_output_tokens":200
            }),
        },
        if hold_second {
            Script::ToolInput {
                call: "call_second_step",
                tool: "exec_command",
                arguments: serde_json::json!({
                    "cmd": format!("printf ready > '{}/second-ready'; while [ ! -f '{}/second-release' ]; do sleep 0.02; done; printf SECOND_TOOL_FINISHED",workspace.display(),workspace.display()),
                    "workdir":workspace,"yield_time_ms":10000,"timeout_ms":20000,"max_output_tokens":200
                }),
            }
        } else {
            Script::ToolRead {
                call: "call_second_step",
                path: "step-two.txt",
            }
        },
        Script::SuccessText("THREE_STEP_EXECUTION_DONE"),
        Script::SuccessText("LEGACY_QUEUE_DONE"),
    ]);
    let mut child = spawn(&root, provider.address, env);
    let address = await_ready(&root, &mut child);
    let session = create_in(address, &workspace);
    submit(
        address,
        &session,
        "msg_step_initial",
        "run the three step task",
    );
    let ready = permission_residuals::wait_until(
        Duration::from_secs(10),
        Duration::from_millis(10),
        || workspace.join("step-ready").exists(),
    );
    assert!(
        ready.is_some(),
        "first real tool did not reach its hold point"
    );
    Ok((root, provider, child, address, session))
}

#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn ordinary_inputs_during_a_tool_reach_the_second_request_in_order_once() -> TestResult {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session) = setup("ordinary-step-input")?;
    let (events, connected) = event_stream(address);
    let watcher = thread::spawn(move || {
        collect(
            events,
            connected,
            "\"type\":\"session.execution.succeeded\"",
        )
    });
    submit(address, &session, "msg_step_first", FIRST);
    submit(address, &session, "msg_step_second", SECOND);
    submit(address, &session, "msg_step_first", FIRST);
    assert_eq!(users(address, &session), ["run the three step task"]);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "tool batch still owns step one"
    );
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_step_first","text":"changed original content"}).to_string(),
    );
    assert_eq!(status, 409, "stable identities cannot change content");
    // A parked owner spends no running allowance, so a fifth queued input is
    // admitted and runs after the others. Running work stays bounded; the
    // queue is what waits.
    let (queued, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_step_queued","text":QUEUED}).to_string(),
    );
    assert_eq!(
        queued, 200,
        "a parked owner must not spend running capacity"
    );
    fs::write(workspace_dir(&root).join("step-release"), "release")?;
    let sent = permission_residuals::wait_until(
        Duration::from_secs(10),
        Duration::from_millis(10),
        || locked(&provider.bodies).len() >= 2,
    );
    assert!(sent.is_some(), "the second model request never started");
    let bodies = locked(&provider.bodies).clone();
    let next = bodies.get(1).ok_or("second provider request absent")?;
    let first = next
        .find(FIRST)
        .ok_or("first ordinary input missing from step two")?;
    let second = next
        .find(SECOND)
        .ok_or("second ordinary input missing from step two")?;
    let queued = next
        .find(QUEUED)
        .ok_or("queued ordinary input missing from step two")?;
    assert!(
        first < second && second < queued,
        "ordinary input order changed"
    );
    assert!(
        next.contains(FINISHED),
        "submission cancelled or bypassed the in-flight tool"
    );
    assert_eq!(next.matches(FIRST).count(), 1);
    assert_eq!(next.matches(SECOND).count(), 1);
    assert_eq!(next.matches(QUEUED).count(), 1);
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(
        bodies.len(),
        3,
        "inputs must stay in the same three-Step execution"
    );
    assert_eq!(
        users(address, &session),
        ["run the three step task", FIRST, SECOND, QUEUED]
    );
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/pending"),
        "",
    );
    assert_eq!(page["data"], serde_json::json!([]));
    let observed = watcher
        .join()
        .map_err(|_| "promotion event watcher panicked")?;
    let promoted: Vec<serde_json::Value> = observed
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["type"] == "session.input.promoted")
        .collect();
    for id in ["msg_step_first", "msg_step_second", "msg_step_queued"] {
        assert_eq!(
            promoted
                .iter()
                .filter(|event| event["data"]["inputID"] == id)
                .count(),
            1,
            "mid-chain promotion must publish its committed advisory once: {observed}"
        );
    }
    let response = submit(address, &session, "msg_step_first", FIRST);
    assert_eq!(response["data"]["delivery"], "steer");
    for conflicting in [
        serde_json::json!({"id":"msg_step_first","text":"changed original content"}),
        serde_json::json!({"id":"msg_step_first","text":FIRST,"delivery":"queue"}),
    ] {
        let (status, _) = request(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &conflicting.to_string(),
        );
        assert_eq!(status, 409, "settled stable input identity changed");
    }
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "promoted retry started another execution"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&workspace_dir(&root));
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn esc_restarts_queued_inputs_once_after_terminal_and_a_stale_stop_cannot_hit_the_successor(
) -> TestResult {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session) =
        setup_second("ordinary-step-interrupt", true)?;
    let active = json(address, "GET", "/api/session/active", "");
    let old = active["data"][&session]["executionID"]
        .as_str()
        .ok_or("old execution identity absent")?
        .to_owned();
    submit(address, &session, "msg_step_interrupt_first", FIRST);
    submit(address, &session, "msg_step_interrupt_second", SECOND);
    let body = serde_json::json!({"executionID":old,"resumePending":true}).to_string();
    let (status, bytes) = request(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        &body,
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&bytes));
    let ready = permission_residuals::wait_until(
        Duration::from_secs(10),
        Duration::from_millis(10),
        || workspace_dir(&root).join("second-ready").exists(),
    );
    assert!(
        ready.is_some(),
        "queued input never reached the new execution"
    );
    let current = json(address, "GET", "/api/session/active", "");
    assert_ne!(current["data"][&session]["executionID"], old);
    let (stale, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        &body,
    );
    assert_eq!(
        stale, 400,
        "a stale stop must be refused before touching the successor"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        users(address, &session),
        ["run the three step task", FIRST, SECOND]
    );
    let bodies = locked(&provider.bodies).clone();
    let next = bodies.get(1).ok_or("resumed request absent")?;
    assert!(
        next.find(FIRST).ok_or("first input absent")?
            < next.find(SECOND).ok_or("second input absent")?
    );
    assert_eq!(next.matches(FIRST).count(), 1);
    assert_eq!(next.matches(SECOND).count(), 1);
    fs::write(workspace_dir(&root).join("second-release"), "release")?;
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "queued messages caused duplicate executions"
    );
    manual_compaction::assert_carried(&root, "msg_step_initial", "session.execution.interrupted.1");
    manual_compaction::assert_carried(
        &root,
        "msg_step_interrupt_first",
        "session.execution.succeeded.1",
    );
    let (stale, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        &body,
    );
    assert_eq!(stale, 400);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&workspace_dir(&root));
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn an_input_promoted_before_esc_is_not_resubmitted() -> TestResult {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session) =
        setup_second("ordinary-promoted-before-esc", true)?;
    submit(address, &session, "msg_step_promoted", FIRST);
    fs::write(workspace_dir(&root).join("step-release"), "release")?;
    let ready = permission_residuals::wait_until(
        Duration::from_secs(10),
        Duration::from_millis(10),
        || workspace_dir(&root).join("second-ready").exists(),
    );
    assert!(ready.is_some(), "second tool never started");
    assert_eq!(users(address, &session), ["run the three step task", FIRST]);
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        r#"{"resumePending":true}"#,
    );
    assert_eq!(status, 204);
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "already-promoted input was replayed"
    );
    assert_eq!(
        users(address, &session)
            .iter()
            .filter(|text| text.as_str() == FIRST)
            .count(),
        1
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&workspace_dir(&root));
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn omitted_mode_retry_retains_an_existing_queue_until_idle() -> TestResult {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session) = setup("ordinary-step-legacy-queue")?;
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_step_legacy_queue","text":FIRST,"delivery":"queue"})
            .to_string(),
    );
    let retried = submit(address, &session, "msg_step_legacy_queue", FIRST);
    assert_eq!(
        retried["data"]["delivery"], "queue",
        "retry changed stored delivery"
    );
    assert_eq!(users(address, &session), ["run the three step task"]);
    fs::write(workspace_dir(&root).join("step-release"), "release")?;
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert!(!bodies.get(1).ok_or("second step absent")?.contains(FIRST));
    assert!(bodies
        .get(3)
        .ok_or("queued request absent")?
        .contains(FIRST));
    let retried = submit(address, &session, "msg_step_legacy_queue", FIRST);
    assert_eq!(retried["data"]["delivery"], "queue");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&workspace_dir(&root));
    process_fixture::cleanup(&root);
    Ok(())
}
