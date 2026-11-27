//! Native context through real product processes and the published MCP path.
use super::*;
#[path = "stateengine_native/approval_unavailable.rs"]
mod approval_unavailable;
#[path = "stateengine_native/codemode_default.rs"]
mod codemode_default;

pub(super) fn configure(root: &Path, mode: &str) {
    // Retain coverage of explicit direct publication alongside the default.
    configure_exposure(root, mode, Some(false));
}

fn configure_exposure(root: &Path, mode: &str, codemode: Option<bool>) {
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("fixture config: {error}")),
    )
    .unwrap_or_else(|error| panic!("fixture config JSON: {error}"));
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/stateengine-native/fixture_server.py");
    assert!(script.is_file(), "StateEngine fixture is absent");
    // These journeys exercise attended and unavailable approval paths explicitly.
    config["permission"] = serde_json::json!({"stateengine_*":"ask"});
    config["mcp"] = serde_json::json!({"stateengine": {
        "type": "local", "command": ["python3", script, "--mode", mode]
    }});
    if let Some(codemode) = codemode {
        config["mcp"]["stateengine"]["codemode"] = serde_json::json!(codemode);
    }
    fs::write(&path, config.to_string()).unwrap_or_else(|error| panic!("fixture config: {error}"));
}

pub(super) use super::stateengine_binding::{bind, create_native, put_no_content};

fn run_native(root: &Path, session: &str) -> super::cli_continuity::CliRun {
    let service = root.to_string_lossy();
    super::cli_continuity::cli(
        root,
        &[
            "run",
            "--service",
            &service,
            "--session",
            session,
            "--prompt",
            "Inspect calc using current working state, recalled memory and Atlas pointers.",
        ],
    )
}

#[test]
fn native_continuity_failure_recovery_survives_restart_without_provider_calls() {
    use grapt_session::execution::ContinuityFailure;
    let _journey = process_journey_custody();
    // Authority, identity and reply failures fail closed; a transport outage
    // degrades (below).
    for (mode, capability, cause) in [
        ("ready", false, ContinuityFailure::Capability),
        ("foreign", true, ContinuityFailure::Principal),
        ("incomplete", true, ContinuityFailure::Incomplete),
        ("stale", true, ContinuityFailure::Incomplete),
    ] {
        let root = root(&format!("native-continuity-{cause:?}"));
        prepare(&root, 90_000);
        configure(&root, mode);
        let provider = ProviderFixture::start(Vec::new());
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let _approver = approval::Attachment::open(address, "*");
        let session = create_native(address, &root);
        bind(address, &session, capability);
        let run = run_native(&root, &session);
        assert_ne!(run.code, Some(0), "missing continuity reported success");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        child.stop_after_witness(&root);
        drop(child);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let _approver = approval::Attachment::open(address, "*");
        let history = json(
            address,
            "GET",
            &format!("/api/session/{session}/message?order=asc&limit=50"),
            "",
        );
        let rows = history["data"]
            .as_array()
            .unwrap_or_else(|| panic!("history missing: {history}"));
        let failures: Vec<_> = rows
            .iter()
            .filter(|row| row["metadata"]["executionFailure"] == true)
            .collect();
        assert_eq!(failures.len(), 1, "native recovery row missing: {history}");
        assert_eq!(
            failures[0]["text"],
            format!("Execution failed: {}", cause.message())
        );
        for private in [
            "NATIVE_FIXTURE_CAPABILITY",
            "NATIVE_FIXTURE_PRIVATE_CURSOR",
            "hostile fixture transport text",
            "agt_foreign",
        ] {
            assert!(
                !history.to_string().contains(private),
                "private detail reached native history"
            );
        }
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&root);
    }
}

/// The frames a new `/api/event` subscriber is offered in its first moments.
fn first_events(address: SocketAddr) -> String {
    let mut stream = TcpStream::connect(address).unwrap_or_else(|e| panic!("event stream: {e}"));
    stream
        .set_read_timeout(Some(Duration::from_millis(1500)))
        .unwrap_or_else(|e| panic!("event stream timeout: {e}"));
    write!(
        stream,
        "GET /api/event HTTP/1.1\r\nHost: {address}\r\nAuthorization: {AUTHORIZATION}\r\nConnection: close\r\n\r\n"
    )
    .unwrap_or_else(|e| panic!("event request: {e}"));
    let mut seen = Vec::new();
    let mut buffer = [0_u8; 8192];
    while let Ok(count) = stream.read(&mut buffer) {
        if count == 0 || seen.len() > 256 * 1024 {
            break;
        }
        seen.extend_from_slice(&buffer[..count]);
    }
    String::from_utf8_lossy(&seen).into_owned()
}

/// A continuity transport outage never fails the turn: the model is told
/// continuity is unavailable and why, and the turn completes without it.
#[test]
fn native_continuity_outage_degrades_and_the_turn_completes() {
    use grapt_session::execution::ContinuityFailure;
    let _journey = process_journey_custody();
    let (mode, capability, cause) = ("transport", true, ContinuityFailure::Transport);
    let root = root(&format!("native-continuity-outage-{mode}"));
    prepare(&root, 90_000);
    configure(&root, mode);
    let provider = ProviderFixture::start(vec![Script::Success]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create_native(address, &root);
    bind(address, &session, capability);
    let run = run_native(&root, &session);
    assert_eq!(
        run.code,
        Some(0),
        "{mode}: a continuity outage failed the turn: {}",
        run.stderr
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1, "{mode}");
    let bodies = provider
        .bodies
        .lock()
        .unwrap_or_else(|error| panic!("bodies: {error}"));
    let notice = format!(
        "StateEngine external continuity is unavailable this turn: {}",
        cause.message()
    );
    assert!(
        bodies.iter().any(|body| body.contains(&notice)),
        "{mode}: the model was not told continuity is unavailable"
    );
    assert!(
        bodies
            .iter()
            .all(|body| !body.contains("NATIVE_FIXTURE_CAPABILITY")),
        "{mode}: a capability reached the model"
    );
    drop(bodies);
    // A window that opens now still learns the Session is degraded.
    let offered = first_events(address);
    assert!(
        offered
            .lines()
            .any(|line| line.contains("session.continuity.attention")
                && line.contains(&session)
                && line.contains(r#""reason":"transport""#)),
        "a new subscriber was not offered the degraded level:\n{offered}"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

/// A service restart keeps the binding and loses the capability. A window
/// sees "rebind" before any turn, and putting the capability again recovers
/// the same service with no relaunch.
#[test]
fn a_restart_shows_rebind_until_the_capability_is_put_again() {
    let _journey = process_journey_custody();
    let root = root("native-continuity-rebind");
    prepare(&root, 90_000);
    configure(&root, "ready");
    let provider = ProviderFixture::start(vec![Script::Success]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create_native(address, &root);
    bind(address, &session, true);
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let degraded = |offered: &str| {
        offered.lines().any(|line| {
            line.contains("session.continuity.attention")
                && line.contains(&session)
                && line.contains(r#""reason":"capability""#)
        })
    };
    let offered = first_events(address);
    assert!(
        degraded(&offered),
        "no rebind level before any turn:\n{offered}"
    );
    put_no_content(
        address,
        &format!("/api/session/{session}/stateengine/capability"),
        r#"{"capability":"NATIVE_FIXTURE_CAPABILITY"}"#,
    );
    let offered = first_events(address);
    assert!(
        !degraded(&offered),
        "the rebind level outlived the put:\n{offered}"
    );
    let run = run_native(&root, &session);
    assert_eq!(run.code, Some(0), "the rebound turn failed: {}", run.stderr);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let bodies = provider
        .bodies
        .lock()
        .unwrap_or_else(|error| panic!("bodies: {error}"));
    assert!(
        bodies
            .iter()
            .any(|body| body.contains("StateEngine external continuity (context only")),
        "the rebound turn ran without its continuity context"
    );
    assert!(bodies
        .iter()
        .all(|body| !body.contains("unavailable this turn")));
    drop(bodies);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn native_memory_and_atlas_tools_preserve_source_and_stale_absent_evidence() {
    let _journey = process_journey_custody();
    let root = root("native-context-tools");
    prepare(&root, 90_000);
    configure(&root, "ready");
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_native_working",
            tool: "stateengine_stateengine_working_state_get",
            arguments: serde_json::json!({}),
        },
        Script::ToolInput {
            call: "call_native_memory",
            tool: "stateengine_stateengine_memory_recall",
            arguments: serde_json::json!({"query":"calc", "limit":3}),
        },
        Script::ToolInput {
            call: "call_native_map",
            tool: "stateengine_stateengine_atlas_repo_map",
            arguments: serde_json::json!({"query":"calc", "token_budget":512}),
        },
        Script::ToolInput {
            call: "call_native_impact",
            tool: "stateengine_stateengine_atlas_impact",
            arguments: serde_json::json!({"target":"src/calc.rs", "limit":3}),
        },
        Script::SuccessText("Fixture context inspected; impact evidence is stale and incomplete."),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create_native(address, &root);
    bind(address, &session, true);
    let run = thread::scope(|scope| {
        let run = scope.spawn(|| run_native(&root, &session));
        // The normal preset asks before MCP effects. Reply once to each
        // named, disposable read; do not bypass the permission owner.
        for call in [
            "call_native_working",
            "call_native_memory",
            "call_native_map",
            "call_native_impact",
        ] {
            let pending = super::permission_boundaries::pending(address, &session, call);
            super::permission_boundaries::reply(address, &session, &pending, "once");
        }
        run.join()
            .unwrap_or_else(|_| panic!("native CLI worker panicked"))
    });
    assert_eq!(
        run.code,
        Some(0),
        "native context tools failed: {}",
        run.stderr
    );
    let bodies = provider
        .bodies
        .lock()
        .unwrap_or_else(|_| panic!("provider request evidence poisoned"));
    assert_eq!(bodies.len(), 5);
    let first: serde_json::Value = serde_json::from_str(&bodies[0])
        .unwrap_or_else(|error| panic!("provider request: {error}"));
    let tools = first["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tool catalog absent"));
    for tool in [
        "stateengine_stateengine_working_state_get",
        "stateengine_stateengine_memory_recall",
        "stateengine_stateengine_atlas_repo_map",
        "stateengine_stateengine_atlas_impact",
    ] {
        assert!(
            tools.iter().any(|row| row["name"] == tool),
            "context tool unpublished: {tool}"
        );
    }
    assert!(tools
        .iter()
        .any(|row| row["name"] == "stateengine_stateengine_resume"));
    assert!(
        bodies[0].contains("NATIVE_SOURCE_VICTOR6284"),
        "automatic continuity missing"
    );
    let last = bodies
        .last()
        .unwrap_or_else(|| panic!("no rebuilt request"));
    for evidence in [
        "calc rounds down",
        "src/calc.rs",
        "fixture-index-stale",
        "fixture edge index absent",
        "disposable-fixture",
    ] {
        assert!(
            last.contains(evidence),
            "tool context lost evidence: {evidence}"
        );
    }
    assert!(last.contains("ratified"));
    for body in bodies.iter() {
        assert!(!body.contains("NATIVE_FIXTURE_CAPABILITY"));
        assert!(!body.contains("NATIVE_FIXTURE_PRIVATE_CURSOR"));
    }
    drop(bodies);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
fn native_full_mail_read_and_reply_use_the_existing_bound_permission_path() {
    let _journey = process_journey_custody();
    let root = root("native-mail-read-reply");
    prepare(&root, 90_000);
    configure(&root, "ready");
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_native_mail_read",
            tool: "stateengine_stateengine_msg_read",
            arguments: serde_json::json!({"msg_id":"msg_native_long"}),
        },
        Script::ToolInput {
            call: "call_native_mail_reply",
            tool: "stateengine_stateengine_msg_send",
            arguments: serde_json::json!({"to":"agt_native_sender","kind":"handoff","body":"Full message read; fixture reply."}),
        },
        Script::SuccessText("Full fixture message read and reply sent."),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create_native(address, &root);
    bind(address, &session, true);
    let run = thread::scope(|scope| {
        let run = scope.spawn(|| run_native(&root, &session));
        for call in ["call_native_mail_read", "call_native_mail_reply"] {
            let pending = super::permission_boundaries::pending(address, &session, call);
            super::permission_boundaries::reply(address, &session, &pending, "once");
        }
        run.join()
            .unwrap_or_else(|_| panic!("mail CLI worker panicked"))
    });
    assert_eq!(run.code, Some(0), "mail workflow failed: {}", run.stderr);
    let bodies = provider
        .bodies
        .lock()
        .unwrap_or_else(|_| panic!("request evidence poisoned"));
    assert_eq!(bodies.len(), 3);
    assert!(
        !bodies[0].contains("FULL_BODY_END"),
        "full body must be acquired on demand"
    );
    assert!(
        bodies[1].contains("FULL_BODY_END"),
        "read result never reached the model"
    );
    for receipt in [
        "msg_native_reply",
        "agt_native_sender",
        "delivery_acknowledgement",
        "dismissal",
        "unchanged",
    ] {
        assert!(
            bodies[2].contains(receipt),
            "mail evidence missing: {receipt}"
        );
    }
    for body in bodies.iter() {
        assert!(
            !body.contains("NATIVE_FIXTURE_CAPABILITY")
                && !body.contains("NATIVE_FIXTURE_PRIVATE_CURSOR")
        );
    }
    drop(bodies);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[path = "stateengine_native/start_recovery.rs"]
mod start_recovery;
