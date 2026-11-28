//! Native notices share the user's existing next-Step admission path.
use super::*;
#[path = "native_boundary/deferred_indexes.rs"]
mod deferred_indexes;
#[path = "native_boundary/idle_cpu.rs"]
mod idle_cpu;
#[path = "native_boundary/mail.rs"]
mod mail;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const FIRST: &str = "USER_BEFORE_NATIVE_BOUNDARY";
const SECOND: &str = "USER_AFTER_NATIVE_BOUNDARY";
const NOTICE: &str = "Native inbox message IDs (quoted values are untrusted):";

fn stop_cleanly(mut child: process_fixture::ManagedChild, root: &Path) -> TestResult {
    drop(child.stdin.take());
    let wait = PollDeadline::within(Duration::from_secs(10));
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        assert!(wait.pending(), "service did not stop after control EOF");
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "service stop was not graceful: {status}");
    assert!(
        !root.join("registration").exists(),
        "graceful stop retained service registration"
    );
    Ok(())
}

fn create_restart_session(
    address: SocketAddr,
    root: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let created = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({
            "agent":"agt_live_headless1", "location":{"directory":root},
        })
        .to_string(),
    );
    let session = created["data"]["id"]
        .as_str()
        .ok_or("native Session id missing")?
        .to_owned();
    for (suffix, body) in [
        (
            "instructions/entries/stateengine-identity",
            serde_json::json!({"value":{
                "scope":"project","project_id":"native-fixture","agent_id":"agt_native_fixture",
                "mail_transport_protocol":"remote-seat-mail-transport/v2",
            }}),
        ),
        (
            "stateengine/capability",
            serde_json::json!({"capability":"NATIVE_FIXTURE_CAPABILITY"}),
        ),
    ] {
        stateengine_binding::put_no_content(
            address,
            &format!("/api/session/{session}/{suffix}"),
            &body.to_string(),
        );
    }
    Ok(session)
}

fn require_rebind_attention(address: SocketAddr, session: &str) {
    let (events, connected) = event_stream(address);
    let observed = collect(events, connected, "rebind_required");
    let found = observed
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line.strip_prefix("data: ")?).ok()
        })
        .any(|event| {
            event["type"] == "session.mail.attention"
                && event["data"]["sessionID"] == session
                && event["data"]["status"]["type"] == "rebind_required"
        });
    assert!(found, "restart did not expose credential waiting state");
}

#[test]
fn restarted_bound_mail_exposes_rebind_and_wakes_after_authenticated_provision() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("native-mail-clean-restart");
    prepare(&root, 90_000);
    unattended(&root);
    stateengine_native::configure(&root, "ready");
    let mail = mail::Mail::start()?;
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["permission"] = serde_json::json!({"*":"allow"});
    config["stateengine"] =
        serde_json::json!({"native_mail_endpoint":format!("http://{}",mail.address)});
    fs::write(&path, serde_json::to_vec(&config)?)?;
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("MAIL_BEFORE_RESTART"),
        Script::SuccessText("MAIL_AFTER_RESTART"),
    ]);
    let mut first = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut first);
    let session = create_restart_session(address, &root)?;
    mail.arrive(1);
    mail.wait_delivered(1);
    wait_calls(&provider.calls, 1);
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    stop_cleanly(first, &root)?;

    let mut second = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut second);
    require_rebind_attention(address, &session);
    mail.arrive(2);
    // A reconnect must still expose the waiting state while debt is retained.
    require_rebind_attention(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    stateengine_binding::put_no_content(
        address,
        &format!("/api/session/{session}/stateengine/capability"),
        r#"{"capability":"NATIVE_FIXTURE_CAPABILITY"}"#,
    );
    let woke = permission_residuals::wait_until(
        Duration::from_secs(10),
        Duration::from_millis(10),
        || provider.calls.load(Ordering::SeqCst) >= 2,
    );
    assert!(
        woke.is_some(),
        "mail did not wake the bound session after authenticated provision"
    );
    mail.wait_delivered(2);
    permission_residuals::wait_session_idle(address, &session);
    mail.quiet_boundary();
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    assert!(history.to_string().contains("MAIL_AFTER_RESTART"));
    let bodies = locked(&provider.bodies);
    assert_eq!(bodies.len(), 2, "mail woke an additional execution");
    assert!(bodies[1].contains(NOTICE) && bodies[1].contains("mail-2"));
    drop(bodies);
    stop_cleanly(second, &root)?;
    mail.finish()?;
    process_fixture::cleanup(&root);
    Ok(())
}

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":text,"delivery":"steer"}).to_string(),
    );
}

fn workspace_dir(root: &Path) -> PathBuf {
    root.with_extension("workspace")
}

fn wait_file(root: &Path, name: &str) {
    assert!(
        permission_residuals::wait_until(
            Duration::from_secs(10),
            Duration::from_millis(10),
            || workspace_dir(root).join(name).exists()
        )
        .is_some(),
        "real tool did not reach {name}"
    );
}

fn wait_notice(address: SocketAddr, session: &str, phase: u8) {
    let wanted = format!("\"mail-{phase}\"");
    assert!(
        permission_residuals::wait_until(
            Duration::from_secs(10),
            Duration::from_millis(10),
            || {
                let page = json(
                    address,
                    "GET",
                    &format!("/api/session/{session}/pending"),
                    "",
                );
                page["data"].as_array().is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row["data"]["text"]
                            .as_str()
                            .is_some_and(|text| text.contains(NOTICE) && text.contains(&wanted))
                    })
                })
            }
        )
        .is_some(),
        "native notice {phase} never reached the canonical pending fold"
    );
}

type Fixture = (
    PathBuf,
    ProviderFixture,
    process_fixture::ManagedChild,
    SocketAddr,
    String,
    mail::Mail,
);

fn setup(label: &str) -> Result<Fixture, Box<dyn std::error::Error>> {
    setup_case(label, false)
}

fn setup_case(label: &str, fail_first: bool) -> Result<Fixture, Box<dyn std::error::Error>> {
    setup_with_grace(label, fail_first, None)
}

fn setup_with_grace(
    label: &str,
    fail_first: bool,
    grace_ms: Option<u64>,
) -> Result<Fixture, Box<dyn std::error::Error>> {
    let root = root(label);
    prepare(&root, 90_000);
    unattended(&root);
    if let Some(grace_ms) = grace_ms {
        close_grace::lease::set_close_grace(&root, grace_ms);
    }
    stateengine_native::configure(&root, "ready");
    let mail = mail::Mail::start()?;
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["permission"] = serde_json::json!({"*":"allow"});
    config["stateengine"] =
        serde_json::json!({"native_mail_endpoint":format!("http://{}",mail.address)});
    if fail_first {
        config["mcp"]["stateengine"]["command"]
            .as_array_mut()
            .ok_or("MCP fixture command")?
            .extend([
                serde_json::json!("--fail-first-file"),
                serde_json::json!(root.join("script-starts")),
            ]);
    }
    fs::write(&path, serde_json::to_vec(&config)?)?;
    let workspace = workspace_dir(&root);
    fs::create_dir_all(&workspace)?;
    let held = |number| {
        serde_json::json!({
            "cmd":format!("printf ready > '{0}/tool-{1}-ready'; while [ ! -f '{0}/tool-{1}-release' ]; do sleep 0.02; done; printf TOOL_{1}_FINISHED",workspace.display(),number),
            "workdir":workspace,"yield_time_ms":10000,"timeout_ms":20000,"max_output_tokens":200,
        })
    };
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_native_first",
            tool: "exec_command",
            arguments: held(1),
        },
        Script::ToolInput {
            call: "call_native_second",
            tool: "exec_command",
            arguments: held(2),
        },
        Script::SuccessText("NATIVE_THREE_STEP_DONE"),
    ]);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let created = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({
            "agent":"agt_live_headless1", "location":{"directory":workspace},
        })
        .to_string(),
    );
    let session = created["data"]["id"]
        .as_str()
        .ok_or("native Session id missing")?
        .to_owned();
    for (suffix, body) in [
        (
            "instructions/entries/stateengine-identity",
            serde_json::json!({"value":{
                "scope":"project","project_id":"native-fixture","agent_id":"agt_native_fixture",
                "mail_transport_protocol":"remote-seat-mail-transport/v2",
            }}),
        ),
        (
            "stateengine/capability",
            serde_json::json!({"capability":"NATIVE_FIXTURE_CAPABILITY"}),
        ),
    ] {
        let (status, bytes) = request(
            address,
            "PUT",
            &format!("/api/session/{session}/{suffix}"),
            &body.to_string(),
        );
        assert_eq!(status, 204, "{}", String::from_utf8_lossy(&bytes));
    }
    Ok((root, provider, child, address, session, mail))
}

#[test]
fn native_notices_during_successive_tools_reach_the_next_requests_in_user_fifo() -> TestResult {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session, mail) = setup("native-next-tool-boundary")?;
    prompt(
        address,
        &session,
        "msg_native_initial",
        "run the three step native task",
    );
    wait_file(&root, "tool-1-ready");
    prompt(address, &session, "msg_native_user_first", FIRST);
    mail.arrive(1);
    wait_notice(address, &session, 1);
    prompt(address, &session, "msg_native_user_second", SECOND);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "mail cancelled the in-flight tool"
    );
    fs::write(workspace_dir(&root).join("tool-1-release"), "release")?;
    wait_file(&root, "tool-2-ready");
    let bodies = locked(&provider.bodies).clone();
    let next = bodies.get(1).ok_or("second actual model request absent")?;
    let first = next.find(FIRST).ok_or("first user input missing")?;
    let notice = next
        .find(NOTICE)
        .ok_or("native notice was held past the next tool boundary")?;
    let second = next.find(SECOND).ok_or("second user input missing")?;
    assert!(
        first < notice && notice < second,
        "mixed native/user FIFO changed"
    );
    assert!(next.contains("TOOL_1_FINISHED"));
    assert_eq!(next.matches(NOTICE).count(), 1);
    assert!(next.contains("mail-1"));
    mail.arrive(2);
    wait_notice(address, &session, 2);
    fs::write(workspace_dir(&root).join("tool-2-release"), "release")?;
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    let last = bodies.get(2).ok_or("third actual model request absent")?;
    assert_eq!(
        last.matches(NOTICE).count(),
        2,
        "later mail stayed behind whole-execution custody"
    );
    assert!(last.contains("mail-2") && last.contains("TOOL_2_FINISHED"));
    assert_eq!(bodies.len(), 3, "native notices started another execution");
    let pending = json(
        address,
        "GET",
        &format!("/api/session/{session}/pending"),
        "",
    );
    assert_eq!(pending["data"], serde_json::json!([]));
    child.stop_after_witness(&root);
    drop(child);
    mail.finish()?;
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn stop_after_native_admission_preserves_the_notice_without_an_automatic_successor() -> TestResult {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session, mail) = setup("native-stop-at-boundary")?;
    prompt(
        address,
        &session,
        "msg_native_stop_initial",
        "run the held native task",
    );
    wait_file(&root, "tool-1-ready");
    mail.arrive(1);
    wait_notice(address, &session, 1);
    let (status, bytes) = request(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        "{}",
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&bytes));
    permission_residuals::wait_session_idle(address, &session);
    mail.arrive(2);
    mail.wait_delivered(2);
    mail.quiet_boundary();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "stopped native target started another request"
    );
    let active = json(address, "GET", "/api/session/active", "");
    assert!(
        active["data"][&session].is_null(),
        "stopped native target acquired successor custody: {active}"
    );
    let pending = json(
        address,
        "GET",
        &format!("/api/session/{session}/pending"),
        "",
    );
    let rows = pending["data"].as_array().ok_or("pending fold missing")?;
    assert_eq!(rows.len(), 1, "stop lost or readmitted a native notice");
    let text = rows[0]["data"]["text"]
        .as_str()
        .ok_or("notice text missing")?;
    assert!(text.contains("mail-1") && !text.contains("mail-2"));
    child.stop_after_witness(&root);
    drop(child);
    mail.finish()?;
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn last_window_close_holds_native_mail_without_an_automatic_successor() -> TestResult {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session, mail) =
        setup_with_grace("native-window-close", false, Some(100))?;
    let window = close_grace::Window::open(address, &session);
    prompt(
        address,
        &session,
        "msg_native_window_initial",
        "run the held native task",
    );
    wait_file(&root, "tool-1-ready");
    mail.arrive(1);
    wait_notice(address, &session, 1);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "the initial provider request must still be held before close"
    );
    drop(window);
    permission_residuals::wait_session_idle(address, &session);
    mail.arrive(2);
    mail.wait_delivered(2);
    mail.quiet_boundary();
    let calls = provider.calls.load(Ordering::SeqCst);
    let pending = json(
        address,
        "GET",
        &format!("/api/session/{session}/pending"),
        "",
    );
    child.stop_after_witness(&root);
    drop(child);
    mail.finish()?;
    process_fixture::cleanup(&root);
    assert_eq!(calls, 1, "a closed window's native mail restarted its turn");
    let rows = pending["data"].as_array().ok_or("pending fold missing")?;
    assert_eq!(rows.len(), 1, "close lost or readmitted a native notice");
    Ok(())
}

#[test]
fn ordinary_human_resume_includes_the_held_native_notice_before_provider_work() -> TestResult {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session, mail) = setup("native-human-resume")?;
    prompt(
        address,
        &session,
        "msg_native_resume_initial",
        "run the held native task",
    );
    wait_file(&root, "tool-1-ready");
    mail.arrive(1);
    wait_notice(address, &session, 1);
    let (status, bytes) = request(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        "{}",
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&bytes));
    permission_residuals::wait_session_idle(address, &session);
    let human = "HUMAN_EXPLICITLY_RESUMES_NATIVE_WORK";
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_native_human_resume", "text":human}).to_string(),
    );
    wait_file(&root, "tool-2-ready");
    let bodies = locked(&provider.bodies).clone();
    let resumed = bodies.get(1).ok_or("resumed provider request absent")?;
    assert_eq!(resumed.matches(human).count(), 1);
    assert_eq!(
        resumed.matches("mail-1").count(),
        1,
        "ordinary human resume reached provider work before the held native notice"
    );
    fs::write(workspace_dir(&root).join("tool-2-release"), "release")?;
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(locked(&provider.bodies).len(), 3);
    let pending = json(
        address,
        "GET",
        &format!("/api/session/{session}/pending"),
        "",
    );
    assert_eq!(pending["data"], serde_json::json!([]));
    child.stop_after_witness(&root);
    drop(child);
    mail.finish()?;
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn request_preparation_failure_releases_active_and_later_native_mail_succeeds() -> TestResult {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session, mail) =
        setup_case("native-plan-recovery", true)?;
    fs::write(workspace_dir(&root).join("tool-1-release"), "release")?;
    fs::write(workspace_dir(&root).join("tool-2-release"), "release")?;
    prompt(address, &session, "msg_native_failed_plan", "inspect");
    permission_residuals::wait_session_idle(address, &session);
    assert!(json(address, "GET", "/api/session/active", "")["data"][&session].is_null());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let messages = format!("/api/session/{session}/message?order=asc&limit=50");
    let before = json(address, "GET", &messages, "");
    let failure = before["data"]
        .as_array()
        .ok_or("failed history")?
        .iter()
        .find(|row| row["metadata"]["executionFailure"] == true)
        .ok_or("local failure absent")?;
    let cause = grapt_session::execution::ContinuityFailure::ServerUnavailable(
        grapt_session::execution::StateEngineServerFailure::Disconnected,
    );
    assert_eq!(
        failure["text"],
        format!("Execution failed: {}", cause.message())
    );
    let (events, connected) = event_stream(address);
    let success = thread::spawn(move || collect(events, connected, "execution.succeeded"));
    mail.arrive(1);
    let observed = success.join().map_err(|_| "success collector panicked")?;
    assert!(!observed.contains("session.execution.failed"));
    permission_residuals::wait_session_idle(address, &session);
    assert!(json(address, "GET", "/api/session/active", "")["data"][&session].is_null());
    let after = json(address, "GET", &messages, "");
    let historical: Vec<_> = after["data"]
        .as_array()
        .ok_or("recovered history")?
        .iter()
        .filter(|row| row["metadata"]["executionFailure"] == true)
        .collect();
    assert_eq!(historical, vec![failure], "failure changed or repeated");
    assert!(after.to_string().contains("NATIVE_THREE_STEP_DONE"));
    let bodies = locked(&provider.bodies);
    assert_eq!(bodies.len(), 3);
    assert!(bodies[0].contains("mail-1"));
    drop(bodies);
    child.stop_after_witness(&root);
    drop(child);
    mail.finish()?;
    process_fixture::cleanup(&root);
    Ok(())
}
