//! SC-REL-01: a slow tool never wedges its Session. An interrupted call is
//! cancelled; a call that outlives its execution budget is recorded as an
//! unknown effect, and every later request warns the model to verify that
//! state, across restart and compaction.
use super::*;

const NOTICE: &str = "Its effect state is unknown: it may have partly applied or may still be running. Verify the current state before continuing; do not repeat it blindly.";
const RETAINED: &str = "effect state unknown; verify before repeating it";
const SUMMARY: &str = "## Objective\n- the held call timed out";

/// A stdio MCP tool whose Nth call holds until the witness creates
/// `<release>N`, announcing itself through `<entered>N`. Given `path` and
/// `text`, it first writes them: an effect that happened before the hold.
const HELD_SERVER: &str = r#"
import json, os, sys, time
entered, release = sys.argv[1], sys.argv[2]
calls = 0
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if "id" not in message:
        continue
    if method == "initialize":
        body = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                "serverInfo": {"name": "held", "version": "1"}}
    elif method == "tools/list":
        body = {"tools": [{"name": "hold", "description": "Hold until released.",
                           "inputSchema": {"type": "object", "properties": {
                               "path": {"type": "string"}, "text": {"type": "string"}}}}]}
    elif method == "tools/call":
        calls += 1
        args = message["params"].get("arguments") or {}
        if "path" in args:
            with open(args["path"], "w") as handle:
                handle.write(args["text"])
        open(entered + str(calls), "w").close()
        while not os.path.exists(release + str(calls)):
            time.sleep(0.01)
        body = {"isError": False, "content": [{"type": "text", "text": "released call " + str(calls)}]}
    else:
        body = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": body}), flush=True)
"#;

fn write(path: &Path, bytes: &str) {
    fs::write(path, bytes).unwrap_or_else(|error| panic!("fixture write: {error}"));
}

/// `nested` adds a second held server offered only inside CodeMode.
fn configure(service: &Path, workspace: &Path, nested: bool) {
    let script = service.join("held_server.py");
    write(&script, HELD_SERVER);
    let path = service.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("fixture config: {error}")),
    )
    .unwrap_or_else(|error| panic!("fixture config JSON: {error}"));
    config["mcp"] = serde_json::json!({"held": {
        "type": "local",
        "codemode": false,
        "timeout": {"execution": 1_500},
        "command": ["python3", &script, workspace.join(".held-entered"), workspace.join(".held-release")]
    }});
    if nested {
        config["mcp"]["nested"] = serde_json::json!({
            "type": "local", "codemode": true,
            "timeout": {"execution": 1_500},
            "command": ["python3", &script, workspace.join(".nested-entered"), workspace.join(".nested-release")]
        });
    }
    fs::write(&path, config.to_string()).unwrap_or_else(|error| panic!("fixture config: {error}"));
}

fn prompt(address: SocketAddr, session: &str, input: &str) -> approval::Attachment {
    let approver = approval::Attachment::open(address, "*");
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":input,"text":"continue the work","delivery":"queue","resume":true})
            .to_string(),
    );
    approver
}

/// Answers asks until `done` holds; the held tool may ask first.
fn answering(address: SocketAddr, session: &str, what: &str, done: impl Fn() -> bool) {
    let wait = PollDeadline::new();
    while wait.pending() {
        if done() {
            return;
        }
        let asked = json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        );
        for row in asked["data"].as_array().into_iter().flatten() {
            if let Some(id) = row["id"].as_str() {
                let path = format!("/api/session/{session}/permission/{id}/reply");
                assert_eq!(
                    request(address, "POST", &path, r#"{"reply":"once"}"#).0,
                    204
                );
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("{what} never happened ({wait})");
}

fn settle(address: SocketAddr, session: &str, provider: &ProviderFixture, calls: usize) {
    answering(address, session, "the provider request", || {
        provider.calls.load(Ordering::SeqCst) >= calls
    });
    permission_residuals::wait_session_idle(address, session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
}

/// The execution terminal precedes release of its workspace reservation.
/// A preview has crossed that last boundary only once the exact busy refusal
/// stops; other conflicts remain the witness's result, never a retry reason.
fn settled_undo_preview(mut preview: impl FnMut() -> (u16, Vec<u8>)) -> (u16, Vec<u8>) {
    let mut response = None;
    let settled = permission_residuals::wait_until(
        client_allowance::CLI_REQUEST_ALLOWANCE,
        permission_residuals::SETTLE_POLL,
        || {
            let (status, body) = preview();
            let busy = status == 409
                && serde_json::from_slice::<serde_json::Value>(&body).is_ok_and(|error| {
                    error["_tag"] == "ConflictError"
                        && error["resource"] == "execution"
                        && error["message"]
                            == "this workspace or a nested workspace has admitted work or file undo in progress"
                });
            response = Some((status, body));
            !busy
        },
    );
    assert!(
        settled.is_some(),
        "undo workspace did not settle: {response:?}"
    );
    response.unwrap_or_else(|| panic!("undo preview was not observed"))
}

#[test]
fn undo_preview_waits_for_workspace_release_then_keeps_the_restore_refusal() {
    let busy = serde_json::json!({
        "_tag": "ConflictError", "resource": "execution",
        "message": "this workspace or a nested workspace has admitted work or file undo in progress"
    });
    let refused = serde_json::json!({
        "_tag": "ConflictError", "resource": "execution",
        "message": "the provider refused the operation"
    });
    let mut observations = 0;
    let response = settled_undo_preview(|| {
        observations += 1;
        let body = if observations < 3 { &busy } else { &refused };
        (409, body.to_string().into_bytes())
    });
    assert_eq!(observations, 3, "workspace release was not observed");
    assert_eq!(response, (409, refused.to_string().into_bytes()));
}

#[test]
fn undo_preview_does_not_retry_an_unrelated_conflict() {
    for body in [
        serde_json::json!({"_tag": "ConflictError", "resource": "execution", "message": "changed target"}),
        serde_json::json!({"_tag": "ConflictError", "resource": "permission", "message": "this workspace or a nested workspace has admitted work or file undo in progress"}),
        serde_json::json!({"_tag": "InvalidRequestError", "resource": "execution", "message": "this workspace or a nested workspace has admitted work or file undo in progress"}),
    ] {
        let mut observations = 0;
        let bytes = body.to_string().into_bytes();
        let response = settled_undo_preview(|| {
            observations += 1;
            assert_eq!(observations, 1, "unrelated refusal was retried");
            (409, bytes.clone())
        });
        assert_eq!(response, (409, bytes));
    }
}

fn interrupt(address: SocketAddr, session: &str) {
    let active = json(address, "GET", "/api/session/active", "");
    let execution = active["data"][session]["executionID"]
        .as_str()
        .unwrap_or_else(|| panic!("the held call is not active: {active}"));
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        &serde_json::json!({"executionID":execution}).to_string(),
    );
    assert_eq!(status, 204);
}

fn last_body(provider: &ProviderFixture) -> String {
    locked(&provider.bodies)
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("the provider saw no request"))
}

struct Journey {
    service: PathBuf,
    workspace: PathBuf,
    child: process_fixture::ManagedChild,
    provider: ProviderFixture,
    address: SocketAddr,
    session: String,
}

fn start(label: &str, scripts: impl FnOnce(&Path) -> Vec<Script>) -> Journey {
    start_with(label, false, scripts)
}

fn start_with(label: &str, nested: bool, scripts: impl FnOnce(&Path) -> Vec<Script>) -> Journey {
    let service = root(&format!("{label}-service"));
    prepare(&service, 120_000);
    let workspace = root(&format!("{label}-workspace"));
    process_fixture::cleanup(&workspace);
    fs::create_dir_all(&workspace).unwrap_or_else(|error| panic!("workspace: {error}"));
    let provider = ProviderFixture::start(scripts(&workspace));
    configure(&service, &workspace, nested);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("Session identity is absent"))
        .to_owned();
    Journey {
        service,
        workspace,
        child,
        provider,
        address,
        session,
    }
}

#[test]
fn sc_rel_01_a_slow_tool_never_wedges_and_an_unknown_effect_is_warned() {
    let _journey = process_journey_custody();
    let Journey {
        service,
        workspace,
        mut child,
        provider,
        address,
        session,
    } = start("unknown-effect", |_| {
        vec![
            Script::ToolInput {
                call: "call_interrupted_hold",
                tool: "held_hold",
                arguments: serde_json::json!({}),
            },
            Script::ToolInput {
                call: "call_timed_out_hold",
                tool: "held_hold",
                arguments: serde_json::json!({}),
            },
            Script::SuccessText("continued in the same chain"),
            Script::SuccessText("continued in the next prompt"),
            Script::SuccessText("continued after restart"),
            Script::SuccessText(SUMMARY),
            Script::SuccessText("continued after compaction"),
        ]
    });

    // An interrupted slow call is cancelled, and the next prompt runs.
    let _approver = prompt(address, &session, "msg_interrupted_hold");
    let entered = workspace.join(".held-entered1");
    answering(address, &session, "the first held call", || {
        entered.exists()
    });
    interrupt(address, &session);
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    // The server is still inside the cancelled call, so the next one outlives
    // its execution budget. It may yet run: its terminal records an unknown
    // effect, and the chain continues warned.
    let _approver = prompt(address, &session, "msg_timed_out_hold");
    settle(address, &session, &provider, 3);
    let bodies = locked(&provider.bodies).clone();
    assert!(
        bodies[1].contains("Tool cancelled: User"),
        "the interrupt was not a cancellation: {}",
        bodies[1]
    );
    assert!(
        bodies[2].contains(&format!("{NOTICE}\\nTimedOut")),
        "the chain lacks the timed-out notice: {}",
        bodies[2]
    );

    let _approver = prompt(address, &session, "msg_after_unknown");
    settle(address, &session, &provider, 4);
    let body = last_body(&provider);
    assert!(
        body.contains(NOTICE),
        "the next prompt lacks the notice: {body}"
    );

    child.stop_after_witness(&service);
    drop(child);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let _approver = prompt(address, &session, "msg_after_restart");
    settle(address, &session, &provider, 5);
    let body = last_body(&provider);
    assert!(body.contains(NOTICE), "restart lost the notice: {body}");

    json(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    let _approver = prompt(address, &session, "msg_after_compact");
    settle(address, &session, &provider, 7);
    let body = last_body(&provider);
    assert!(body.contains("the held call timed out"), "{body}");
    assert!(
        body.contains(NOTICE) || body.contains(RETAINED),
        "compaction lost the notice: {body}"
    );
    write(&workspace.join(".held-release1"), "release");
    write(&workspace.join(".held-release2"), "release");
    child.stop_after_witness(&service);
    drop(child);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
}

/// Admission is not settlement: an unknown effect over an undo target leaves
/// that undo to its own restore preconditions, which refuse it.
#[test]
fn sc_rel_01_undo_refuses_to_restore_over_an_uncertain_effect() {
    let _journey = process_journey_custody();
    let mut journey = start("unknown-effect-undo", |workspace| {
        vec![
            Script::ToolInput {
                call: "call_undoable_patch",
                tool: "apply_patch",
                arguments: serde_json::json!({"input": "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+two\n*** End Patch\n"}),
            },
            Script::SuccessText("patched"),
            Script::ToolInput {
                call: "call_uncertain_write",
                tool: "held_hold",
                arguments: serde_json::json!({"path": workspace.join("a.txt"), "text": "uncertain\n"}),
            },
            Script::SuccessText("continued after the unknown effect"),
        ]
    });
    let a = journey.workspace.join("a.txt");
    write(&a, "one\n");
    let (address, session) = (journey.address, journey.session.clone());
    let _approver = prompt(address, &session, "msg_undoable_patch");
    settle(address, &session, &journey.provider, 2);
    assert_eq!(fs::read_to_string(&a).unwrap_or_default(), "two\n");
    let preview = format!("/api/session/{session}/undo/preview");
    assert_eq!(
        settled_undo_preview(|| request(address, "POST", &preview, "{}")).0,
        200
    );

    let _approver = prompt(address, &session, "msg_uncertain_write");
    settle(address, &session, &journey.provider, 4);
    assert!(locked(&journey.provider.bodies)[3].contains(NOTICE));
    assert_eq!(fs::read_to_string(&a).unwrap_or_default(), "uncertain\n");
    let (status, body) = settled_undo_preview(|| request(address, "POST", &preview, "{}"));
    // The refusal is the restore precondition over the changed file, not the
    // former Session-wide quarantine.
    let body = String::from_utf8_lossy(&body);
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("files changed"), "{body}");
    assert!(body.contains("Inspect your edits"), "{body}");
    assert_eq!(fs::read_to_string(&a).unwrap_or_default(), "uncertain\n");
    write(&journey.workspace.join(".held-release1"), "release");
    journey.child.stop_after_witness(&journey.service);
    process_fixture::cleanup(&journey.workspace);
    process_fixture::cleanup(&journey.service);
}

const NESTED_NOTICE: &str = "[Harness notice: Tool nested_hold (call ";
const OLD_NOTICE: &str =
    "[Harness notice: Tool call call_old_unknown ended with an unknown effect.";

/// Review HOLD on ef580713: a nested child's unknown effect under a parent
/// that completed, and one older than the retained ledger, still warn after
/// compaction and restart.
#[test]
fn sc_rel_01_nested_and_older_than_ledger_unknown_effects_keep_their_notice() {
    let _journey = process_journey_custody();
    let reads: Vec<(&'static str, &'static str)> = (0..33)
        .map(|index| {
            let call: &'static str = Box::leak(format!("call_read_{index}").into_boxed_str());
            (call, "seed.txt")
        })
        .collect();
    let mut journey = start_with("unknown-effect-ledger", true, |workspace| {
        // Thirty-three results exceed the new exact working-set window, so
        // the older unknown output really needs its independent warning.
        write(&workspace.join("seed.txt"), &"seed".repeat(512));
        vec![
            // The child outlives its budget; the script catches the failure
            // and its parent completes normally.
            Script::ToolInput {
                call: "call_nested_parent",
                tool: "execute",
                arguments: serde_json::json!({"code":
                    "try { await tools['nested_hold']({}); } catch (error) {} return 'parent done';"}),
            },
            Script::SuccessText("the parent completed"),
            Script::ToolInput {
                call: "call_old_unknown",
                tool: "held_hold",
                arguments: serde_json::json!({}),
            },
            Script::SuccessText("the held call timed out"),
            Script::ToolReads {
                calls: reads[..17].to_vec(),
            },
            Script::ToolReads {
                calls: reads[17..].to_vec(),
            },
            Script::SuccessText("read the seed 33 times"),
            Script::SuccessText(SUMMARY),
            Script::SuccessText("continued after compaction"),
            Script::SuccessText("continued after restart"),
        ]
    });
    let (address, session) = (journey.address, journey.session.clone());
    let _approver = prompt(address, &session, "msg_nested_unknown");
    settle(address, &session, &journey.provider, 2);
    let body = last_body(&journey.provider);
    assert!(
        body.contains(NESTED_NOTICE),
        "the nested unknown effect is silent: {body}"
    );
    assert!(body.contains("run inside call_nested_parent"), "{body}");

    let _approver = prompt(address, &session, "msg_old_unknown");
    settle(address, &session, &journey.provider, 4);
    let _approver = prompt(address, &session, "msg_many_reads");
    settle(address, &session, &journey.provider, 7);

    json(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    let _approver = prompt(address, &session, "msg_after_compact");
    settle(address, &session, &journey.provider, 9);
    let body = last_body(&journey.provider);
    assert!(body.contains("the held call timed out"), "{body}");
    let compacted: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|error| panic!("compacted request: {error}"));
    assert!(
        !compacted["input"].as_array().is_some_and(|items| items
            .iter()
            .any(|item| item["type"] == "function_call_output"
                && item["call_id"] == "call_old_unknown")),
        "fixture kept the older output inside the exact working set"
    );
    assert!(
        body.contains(NESTED_NOTICE),
        "compaction lost the nested notice: {body}"
    );
    assert!(
        body.contains(OLD_NOTICE),
        "compaction lost the older notice: {body}"
    );

    journey.child.stop_after_witness(&journey.service);
    let mut child = spawn(&journey.service, journey.provider.address);
    let address = await_ready(&journey.service, &mut child);
    let _approver = prompt(address, &session, "msg_after_restart");
    settle(address, &session, &journey.provider, 10);
    let body = last_body(&journey.provider);
    assert!(
        body.contains(NESTED_NOTICE),
        "restart lost the nested notice: {body}"
    );
    assert!(
        body.contains(OLD_NOTICE),
        "restart lost the older notice: {body}"
    );
    write(&journey.workspace.join(".held-release1"), "release");
    write(&journey.workspace.join(".nested-release1"), "release");
    child.stop_after_witness(&journey.service);
    process_fixture::cleanup(&journey.workspace);
    process_fixture::cleanup(&journey.service);
}

/// SC-REL-01c: the server answers an interrupted call late; the next call on
/// the same connection still receives its own result.
#[test]
fn sc_rel_01c_a_late_reply_to_an_interrupted_call_never_poisons_the_next() {
    let _journey = process_journey_custody();
    let mut journey = start("late-mcp-reply", |_| {
        vec![
            Script::ToolInput {
                call: "call_interrupted_hold",
                tool: "held_hold",
                arguments: serde_json::json!({}),
            },
            Script::ToolInput {
                call: "call_next_hold",
                tool: "held_hold",
                arguments: serde_json::json!({}),
            },
            Script::SuccessText("the next call answered"),
        ]
    });
    let (address, session) = (journey.address, journey.session.clone());
    let workspace = journey.workspace.clone();
    let _approver = prompt(address, &session, "msg_interrupted_hold");
    let entered = workspace.join(".held-entered1");
    answering(address, &session, "the first held call", || {
        entered.exists()
    });
    interrupt(address, &session);
    permission_residuals::wait_session_idle(address, &session);
    // The server answers the cancelled call only now.
    write(&workspace.join(".held-release1"), "release");

    let _approver = prompt(address, &session, "msg_next_hold");
    let entered = workspace.join(".held-entered2");
    answering(address, &session, "the second held call", || {
        entered.exists()
    });
    write(&workspace.join(".held-release2"), "release");
    settle(address, &session, &journey.provider, 3);
    let body = locked(&journey.provider.bodies)[2].clone();
    assert!(
        body.contains("released call 2"),
        "the next call lost its result: {body}"
    );
    assert!(
        !body.contains("released call 1"),
        "the late reply answered the next call: {body}"
    );
    assert!(!body.contains("EffectUnknown"), "{body}");
    journey.child.stop_after_witness(&journey.service);
    process_fixture::cleanup(&journey.workspace);
    process_fixture::cleanup(&journey.service);
}

/// SC-REL-01b + G2.4: process loss mid nested run leaves a start with no terminal.
/// The restart records the lost run as unknown before admitting work, and
/// automatically resumes the interrupted turn with the settled unknown-effect
/// notice — no manual follow-up prompt or `/reconcile` needed.
#[test]
fn sc_rel_01b_restart_settles_a_nested_run_lost_with_its_process() {
    let _journey = process_journey_custody();
    let mut journey = start_with("reconcile-lost-nested", true, |_| {
        vec![
            Script::ToolInput {
                call: "call_lost_parent",
                tool: "execute",
                arguments: serde_json::json!({"code": "return await tools['nested_hold']({});"}),
            },
            Script::SuccessText("continued after restart"),
        ]
    });
    let (address, session) = (journey.address, journey.session.clone());
    let _approver = prompt(address, &session, "msg_lost_nested");
    let entered = journey.workspace.join(".nested-entered1");
    answering(address, &session, "the nested held call", || {
        entered.exists()
    });
    // Process loss while the nested call runs.
    journey.child.stop_after_witness(&journey.service);
    let mut child = spawn(&journey.service, journey.provider.address);
    let address = await_ready(&journey.service, &mut child);

    // G2.4 automatic resume picks up the interrupted turn after settling the lost run.
    settle(address, &session, &journey.provider, 2);
    let path = format!("/api/session/{session}/reconcile");
    let reconciled = json(address, "POST", &path, "{}");
    assert_eq!(
        reconciled["data"]["reconciled"], 0,
        "the restart left the lost run for /reconcile: {reconciled}"
    );
    let body = last_body(&journey.provider);
    assert!(
        body.contains(NESTED_NOTICE),
        "the settled run is silent: {body}"
    );
    assert!(body.contains("run inside call_lost_parent"), "{body}");
    write(&journey.workspace.join(".nested-release1"), "release");
    child.stop_after_witness(&journey.service);
    process_fixture::cleanup(&journey.workspace);
    process_fixture::cleanup(&journey.service);
}

/// G2.4: a poison turn that crashes repeatedly stops auto-resuming at the
/// retry cap (2) and records a durable status notice rather than crash-looping.
#[test]
fn g24_auto_resume_stops_at_crash_loop_cap_with_durable_notice() {
    let _journey = process_journey_custody();
    let mut journey = start_with("auto-resume-cap", true, |_| {
        vec![
            Script::ToolInput {
                call: "call_poison_0",
                tool: "execute",
                arguments: serde_json::json!({"code": "return await tools['nested_hold']({});"}),
            },
            Script::ToolInput {
                call: "call_poison_1",
                tool: "execute",
                arguments: serde_json::json!({"code": "return await tools['nested_hold']({});"}),
            },
            Script::ToolInput {
                call: "call_poison_2",
                tool: "execute",
                arguments: serde_json::json!({"code": "return await tools['nested_hold']({});"}),
            },
            Script::SuccessText("must not run past cap"),
        ]
    });
    let (address, session) = (journey.address, journey.session.clone());
    let _approver = prompt(address, &session, "msg_poison_turn");
    let entered1 = journey.workspace.join(".nested-entered1");
    answering(address, &session, "initial nested call", || {
        entered1.exists()
    });
    // Crash 1 (original attempt).
    journey.child.stop_after_witness(&journey.service);

    // Restart 1 -> auto-resume attempt 1 -> crashes again.
    let _ = fs::remove_file(&entered1);
    let mut child1 = spawn(&journey.service, journey.provider.address);
    let address1 = await_ready(&journey.service, &mut child1);
    answering(address1, &session, "auto-resume attempt 1", || {
        journey.provider.calls.load(Ordering::SeqCst) >= 2 && entered1.exists()
    });
    child1.stop_after_witness(&journey.service);

    // Restart 2 -> auto-resume attempt 2 -> crashes again.
    let _ = fs::remove_file(&entered1);
    let mut child2 = spawn(&journey.service, journey.provider.address);
    let address2 = await_ready(&journey.service, &mut child2);
    answering(address2, &session, "auto-resume attempt 2", || {
        journey.provider.calls.load(Ordering::SeqCst) >= 3 && entered1.exists()
    });
    child2.stop_after_witness(&journey.service);

    // Restart 3 -> cap (2) reached: must NOT make a 4th provider call and must
    // record a durable retry-limit notice.
    let mut child3 = spawn(&journey.service, journey.provider.address);
    let address3 = await_ready(&journey.service, &mut child3);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        journey.provider.calls.load(Ordering::SeqCst),
        3,
        "auto-resume exceeded retry cap and triggered a 4th provider call"
    );
    let info = json(address3, "GET", &format!("/api/session/{session}"), "");
    let status_message = info["data"]["state"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        status_message.contains("retry limit"),
        "missing durable retry-limit status notice: {info}"
    );
    write(&journey.workspace.join(".nested-release1"), "release");
    child3.stop_after_witness(&journey.service);
    process_fixture::cleanup(&journey.workspace);
    process_fixture::cleanup(&journey.service);
}

/// G2.4: a turn interrupted while waiting for a human permission decision is
/// NOT auto-resumed on restart.
#[test]
fn g24_auto_resume_skips_session_with_pending_permission() {
    let _journey = process_journey_custody();
    let service = root("auto-resume-pending-perm-service");
    prepare(&service, 120_000);
    fs::create_dir_all(service.join("global-config"))
        .unwrap_or_else(|error| panic!("global-config: {error}"));
    write(
        &service.join("global-config/grapt.json"),
        r#"{"permission":{"shell":"ask"}}"#,
    );
    let workspace = root("auto-resume-pending-perm-workspace");
    process_fixture::cleanup(&workspace);
    fs::create_dir_all(&workspace).unwrap_or_else(|error| panic!("workspace: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_perm_wait",
            tool: "shell",
            arguments: serde_json::json!({"command": "echo g24-permission-probe", "timeout_ms": 5000}),
        },
        Script::SuccessText("must not auto-resume while permission pending"),
    ]);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("Session identity is absent"))
        .to_owned();
    let _approver = approval::Attachment::open(address, "*");
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_perm_wait","text":"run gated shell","delivery":"queue","resume":true}"#,
    );
    let wait = PollDeadline::new();
    let mut saw_pending = false;
    while wait.pending() {
        if json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        )["data"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
        {
            saw_pending = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        saw_pending,
        "pending permission row never happened ({wait})"
    );
    // Crash while the permission request is pending.
    child.stop_after_witness(&service);

    let mut restarted = spawn(&service, provider.address);
    let _restarted_address = await_ready(&service, &mut restarted);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "restarted server auto-resumed a session with a pending permission"
    );
    restarted.stop_after_witness(&service);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
}

/// Review HOLD on 3513d1c9: a live composition's nested call has a start and
/// no terminal yet. Queue and steer prompts while it runs are admitted; only
/// an idle Session's unresolved start is a lost run.
#[test]
fn sc_rel_01b_a_live_nested_call_never_reads_as_a_lost_run() {
    let _journey = process_journey_custody();
    let mut journey = start_with("reconcile-live-nested", true, |_| {
        vec![
            Script::ToolInput {
                call: "call_live_parent",
                tool: "execute",
                arguments: serde_json::json!({"code": "return await tools['nested_hold']({});"}),
            },
            Script::SuccessText("the parent finished"),
            Script::SuccessText("answered the queued prompt"),
            Script::SuccessText("answered again"),
        ]
    });
    let (address, session) = (journey.address, journey.session.clone());
    let _approver = prompt(address, &session, "msg_live_nested");
    let entered = journey.workspace.join(".nested-entered1");
    answering(address, &session, "the nested held call", || {
        entered.exists()
    });
    for (id, delivery) in [
        ("msg_queued_during", "queue"),
        ("msg_steered_during", "steer"),
    ] {
        let (status, body) = request(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id":id,"text":"while it runs","delivery":delivery,"resume":true})
                .to_string(),
        );
        let body = String::from_utf8_lossy(&body);
        assert_eq!(status, 200, "{delivery} during a live nested call: {body}");
    }
    write(&journey.workspace.join(".nested-release1"), "release");
    answering(address, &session, "the queued prompt", || {
        journey.provider.calls.load(Ordering::SeqCst) >= 3
    });
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&journey.provider.bodies).clone();
    assert!(
        !bodies.iter().any(|body| body.contains("EffectUnknown")),
        "a settled live call reads as unknown"
    );
    journey.child.stop_after_witness(&journey.service);
    process_fixture::cleanup(&journey.workspace);
    process_fixture::cleanup(&journey.service);
}

/// G2.4: a real-process automatic resume terminates the interrupted execution,
/// settles `pending_resume` and `session_resume_input` on completion, and passes
/// offline `session_cleanup` preview with zero overlapping-start refusals.
#[test]
fn g24_real_process_auto_resume_settles_lifecycle_for_offline_session_cleanup() {
    let _journey = process_journey_custody();
    let mut journey = start_with("auto-resume-cleanup", true, |_| {
        vec![
            Script::ToolInput {
                call: "call_cleanup_parent",
                tool: "execute",
                arguments: serde_json::json!({"code": "return await tools['nested_hold']({});"}),
            },
            Script::SuccessText("resumed and completed cleanly"),
        ]
    });
    let (address, session) = (journey.address, journey.session.clone());
    let _approver = prompt(address, &session, "msg_cleanup_origin");
    let entered = journey.workspace.join(".nested-entered1");
    answering(address, &session, "the nested held call", || {
        entered.exists()
    });
    // Simulate process loss mid-execution, then restart and let auto-resume finish.
    journey.child.stop_after_witness(&journey.service);
    let mut child = spawn(&journey.service, journey.provider.address);
    let address = await_ready(&journey.service, &mut child);
    settle(address, &session, &journey.provider, 2);
    write(&journey.workspace.join(".nested-release1"), "release");
    child.stop_after_witness(&journey.service);

    // Inspect durable state: ProcessLoss disconnection recorded, resume claim & binding settled.
    let conn = rusqlite::Connection::open(journey.service.join("database.sqlite3"))
        .unwrap_or_else(|error| panic!("open database: {error}"));
    let disconnected_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.status.changed.1'
               AND json_extract(data, '$.state.type') = 'disconnected'
               AND json_extract(data, '$.state.reason') = 'process-loss'",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("query disconnected events: {error}"));
    assert_eq!(
        disconnected_count, 1,
        "interrupted execution must record a ProcessLoss disconnection on recovery"
    );
    let resume_state: String = conn
        .query_row(
            "SELECT state FROM pending_resume WHERE session_id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("query pending_resume: {error}"));
    assert_eq!(resume_state, "completed");
    let active_bindings: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_resume_input
             WHERE session_id = ?1 AND active != 0",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("query session_resume_input: {error}"));
    assert_eq!(
        active_bindings, 0,
        "completed auto-resume must retire active session_resume_input bindings"
    );
    drop(conn);

    // Run real offline session_cleanup dry-run preview against the service root.
    let service_str = journey.service.to_string_lossy().into_owned();
    let command = grapt_product::session_cleanup::CleanupCommand::parse([
        "--service",
        service_str.as_str(),
        "--cutoff-ms",
        "1",
        "--current-session",
        "ses_other_current",
    ])
    .unwrap_or_else(|error| panic!("parse cleanup command: {error}"));
    let clock = grapt_store::SystemClock;
    let ids = grapt_store::OsRandomIds::seed().unwrap_or_else(|error| panic!("ids: {error}"));
    let report = command
        .run("test-cleanup-preview", &clock, &ids)
        .unwrap_or_else(|error| panic!("offline session_cleanup preview refused: {error}"));
    assert!(
        report
            .sessions
            .iter()
            .any(|decision| decision.session.as_str() == session.as_str()),
        "resumed session missing from cleanup report: {report:?}"
    );

    process_fixture::cleanup(&journey.workspace);
    process_fixture::cleanup(&journey.service);
}
