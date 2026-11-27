use super::*;
use process_fixture::scenario_lifetime::Scenario;

/// Wait for the subagent authority ask, answer it ONCE, and hand it back.
///
/// Every witness in this file needs the same three steps, and each had its own
/// copy: a bounded poll for the ask, its id, and a `once` reply asserted 204.
/// Seven copies of a twenty-line preamble is not seven witnesses of anything —
/// it is one act, and a change to how an ask is answered had seven places to
/// miss. The row is returned because the first witness also asserts what the
/// ask SAYS, which is its own claim and stays where it is made.
///
/// It answers `once` deliberately. A subagent call is an authority decision
/// like any other tool's, and a witness that saved the decision for the session
/// would stop exercising the ask on its second call.
pub(crate) fn approve_the_subagent_ask(address: SocketAddr, session_id: &str) -> serde_json::Value {
    let list_path = format!("/api/session/{session_id}/permission");
    let wait = PollDeadline::new();
    let permission = wait
        .poll(Duration::from_millis(10), || {
            let listed = json(address, "GET", &list_path, "");
            listed["data"]
                .as_array()
                .and_then(|rows| rows.first())
                .cloned()
        })
        .unwrap_or_else(|| panic!("the subagent permission ask was never displayed"));
    let permission_id = permission["id"]
        .as_str()
        .unwrap_or_else(|| panic!("the subagent permission carried no id"));
    let reply_path = format!("/api/session/{session_id}/permission/{permission_id}/reply");
    let (reply_status, _) = request(address, "POST", &reply_path, r#"{"reply":"once"}"#);
    assert_eq!(reply_status, 204, "the subagent ask must settle");
    permission
}

/// The real parent -> tool -> child -> result path, against the loopback
/// provider.
///
/// This is the witness that in-process coverage could not give. The subagent
/// tool is only reachable through the PRODUCTION composition, and `run_child`
/// needs a `SealedPrincipal`, whose constructor is crate-private to `grapt-api`
/// by design. Here the product is the real binary serving real authenticated
/// requests, so authority is minted at the admission boundary exactly as it is
/// in production — nothing fabricates a principal and no seal is exposed.
///
/// Three scripted provider turns, in order: the parent asks for a subagent, the
/// CHILD's own turn is answered, and the parent then finishes with the tool
/// result in hand. The child really runs; its answer is not mocked.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_subagent_tool_call_runs_a_real_child_and_returns_its_answer() {
    let _journey = process_journey_custody();
    let root = root("subagent-child");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 8_000);
    attended(&root);
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_subagent_once",
            task: "summarise the repository",
        },
        // The child's turn.
        Script::Success,
        // The parent, continuing once the tool result is back.
        Script::Success,
    ]);
    let (address, scenario) = Scenario::spawn(&root, provider.address);
    // Start the real parent/child journey after the old eight-second window.
    // No HTTP request is in flight during this lifetime witness.
    thread::sleep(Duration::from_secs(9));
    let _approver = approval::Attachment::open(address, "*");

    let session = json(address, "POST", "/api/session", "{}");
    let session_id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session response carried no id"))
        .to_owned();
    let prompt_path = format!("/api/session/{session_id}/prompt");
    let prompt_address = address;
    let prompt = thread::spawn(move || {
        request(
            prompt_address,
            "POST",
            &prompt_path,
            r#"{"id":"msg_subagent_once","text":"delegate this","delivery":"queue","resume":true}"#,
        )
    });

    // The subagent call is an authority decision like any other tool. It is
    // approved here rather than bypassed: a child launched without one would
    // prove the tool runs, not that it runs under the authority it claims.
    let permission = approve_the_subagent_ask(address, &session_id);
    assert_eq!(permission["action"], "subagent");
    assert_eq!(permission["source"]["type"], "tool");
    assert_eq!(permission["source"]["callID"], "call_subagent_once");

    let (prompt_status, prompt_body) = prompt
        .join()
        .unwrap_or_else(|_| panic!("the subagent prompt thread panicked"));
    assert_eq!(
        prompt_status,
        200,
        "the subagent prompt failed: {}",
        String::from_utf8_lossy(&prompt_body)
    );
    permission_residuals::wait_session_idle(address, &session_id);

    // A CHILD SESSION EXISTS, AND IT RECORDS ITS PARENT. The tool did not
    // answer out of its own head: a real Session was minted, ran, and is
    // attributed to the Session that asked for it.
    let sessions = json(address, "GET", "/api/session", "");
    let rows = sessions["data"]
        .as_array()
        .unwrap_or_else(|| panic!("session list was not an array"));
    let children: Vec<_> = rows
        .iter()
        .filter(|row| row["parentID"].as_str() == Some(session_id.as_str()))
        .collect();
    assert_eq!(
        children.len(),
        1,
        "exactly one child was launched and recorded its parent: {sessions}"
    );
    let child_id = children[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("the child carried no id"));
    assert_ne!(child_id, session_id, "the child is its own Session");

    // THE CHILD REALLY RAN. Its transcript carries the provider's answer, which
    // is the text the loopback script served to the child's own turn.
    let child_history = json(
        address,
        "GET",
        &format!("/api/session/{child_id}/message"),
        "",
    )
    .to_string();
    assert!(
        child_history.contains("Hello from the live provider."),
        "the child's own transcript holds what the provider answered: {child_history}"
    );

    // AND THE PARENT RECEIVED IT AS THE TOOL RESULT, rather than the tool
    // returning something the child never said.
    let parent_history = json(
        address,
        "GET",
        &format!("/api/session/{session_id}/message"),
        "",
    )
    .to_string();
    assert!(
        parent_history.contains("call_subagent_once"),
        "the parent's transcript records the subagent call: {parent_history}"
    );
    assert!(
        parent_history.contains("Hello from the live provider."),
        "the child's answer reached the parent as the tool result: {parent_history}"
    );

    // Check every created session for a product-recorded execution failure.
    let sessions = json(address, "GET", "/api/session", "");
    let ids: Vec<String> = sessions["data"]
        .as_array()
        .unwrap_or_else(|| panic!("session list was not an array"))
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect();
    let mut failed = Vec::new();
    for id in &ids {
        let history = json(address, "GET", &format!("/api/session/{id}/message"), "");
        let count = history["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["metadata"]["executionFailure"] == true)
            .count();
        if count > 0 {
            failed.push(format!("{id}: {count}"));
        }
    }
    assert!(
        failed.is_empty(),
        "sessions ended in a product-recorded execution failure: {failed:?}"
    );
    scenario.finish();
}

/// The counterexample for the durable-terminal gate.
///
/// The child is launched exactly as above, but its own turn is served a
/// malformed provider response, so its Step settles FAILED rather than `Ended`.
///
/// Nothing about the parent changes: it still gets a tool result back, and the
/// child still leaves the active map. That is the whole point — waiting for a
/// child to stop running proves only that it stopped. If the output were read
/// on liveness alone, whatever text was in the child's transcript would be
/// returned to the parent as an answer the child never successfully gave.
///
/// So the tool must FAIL here, and the operation must not be recorded as
/// completed.
///
/// The refusal this asserts is the `Failed` arm. It used to be the UNSETTLED
/// one, and that was a symptom rather than a fact about the domain: the wait
/// read the child's history before its Step terminal was committed, so an
/// unsettled Step was what a too-early read saw. With the wait corrected the
/// child settles first and the durable terminal is `Failed`, carrying the
/// concrete provider failure class.
#[test]
fn a_child_whose_own_turn_fails_does_not_answer_its_parent() {
    let _journey = process_journey_custody();
    let root = root("subagent-child-failed");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 8_000);
    attended(&root);
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_subagent_failed",
            task: "this child will not succeed",
        },
        // The CHILD's turn: real text, then a refusal. Partial history is what
        // separates "the child failed" from "the child said nothing".
        Script::PartialThenMalformed,
        // The parent, continuing with whatever the tool reported.
        Script::Success,
    ]);
    let provider_bodies = Arc::clone(&provider.bodies);
    let (address, scenario) = Scenario::spawn(&root, provider.address);
    let _approver = approval::Attachment::open(address, "*");

    let session = json(address, "POST", "/api/session", "{}");
    let session_id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session response carried no id"))
        .to_owned();
    let prompt_path = format!("/api/session/{session_id}/prompt");
    let prompt_address = address;
    let prompt = thread::spawn(move || {
        request(
            prompt_address,
            "POST",
            &prompt_path,
            r#"{"id":"msg_subagent_failed","text":"delegate this","delivery":"queue","resume":true}"#,
        )
    });

    approve_the_subagent_ask(address, &session_id);

    let _ = prompt
        .join()
        .unwrap_or_else(|_| panic!("the subagent prompt thread panicked"));
    permission_residuals::wait_session_idle(address, &session_id);

    // The child was still LAUNCHED and still recorded its lineage: the failure
    // is the child's turn, not the admission.
    let sessions = json(address, "GET", "/api/session", "");
    let rows = sessions["data"]
        .as_array()
        .unwrap_or_else(|| panic!("session list was not an array"));
    let children: Vec<_> = rows
        .iter()
        .filter(|row| row["parentID"].as_str() == Some(session_id.as_str()))
        .collect();
    assert_eq!(children.len(), 1, "the child was launched: {sessions}");

    // AND THE TOOL FAILED. The assertion is on the tool part itself rather than
    // on the transcript as a whole: the parent's OWN continuation turn is served
    // a success by this fixture, so the provider's text legitimately appears
    // elsewhere in the transcript. What must not happen is the subagent call
    // reporting a result.
    let parent_history = json(
        address,
        "GET",
        &format!("/api/session/{session_id}/message"),
        "",
    );
    let call = parent_history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("parent history was not an array"))
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .find(|part| part["id"].as_str() == Some("call_subagent_failed"))
        .unwrap_or_else(|| panic!("the parent records no subagent call: {parent_history}"));

    assert_eq!(
        call["state"]["status"], "error",
        "a failed child must fail its tool call: {call}"
    );
    assert!(
        !call.to_string().contains("Partial child text."),
        "a failed child must not hand its parent the text it managed to emit: {call}"
    );

    // THE CONCRETE REFUSAL, not a generic tool error. The failed terminal's
    // detail artifact is folded into the continuation the parent sends onward,
    // so the provider body is where the real reason becomes observable — and
    // asserting it is what stops an EARLIER refusal masking the terminal gate.
    let sent = provider_bodies
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .join(
            "
",
        );
    assert!(
        sent.contains("the child did not succeed: ProviderFailure"),
        "the refusal must name the child's FAILED terminal and its concrete          provider failure class: {sent}"
    );
    scenario.finish();
}

/// Direct child prompts steer its current work and remain usable after it ends.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_running_subagent_accepts_a_direct_human_prompt() {
    let _journey = process_journey_custody();
    let root = root("subagent-child-human");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 30_000);
    attended(&root);
    let entered = Arc::new(terminal_coordination::Phase::new());
    let release = Arc::new(terminal_coordination::Phase::new());
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_child_human",
            task: "summarise the repository",
        },
        Script::HeldTerminalOutput {
            bytes: 64,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        Script::SuccessText("The child followed the human direction."),
        Script::ForRequest(|body| {
            if body.contains("Answer the busy queued followup.") {
                Script::SuccessText("The busy queued answer belongs only to the child.")
            } else {
                Script::Success
            }
        }),
        Script::ForRequest(|body| {
            if body.contains("Answer the busy queued followup.") {
                Script::SuccessText("The busy queued answer belongs only to the child.")
            } else {
                Script::Success
            }
        }),
        Script::SuccessText("An independent child followup."),
    ]);
    let (address, scenario) = Scenario::spawn(&root, provider.address);
    let _approver = approval::Attachment::open(address, "*");
    let session = json(address, "POST", "/api/session", "{}");
    let parent = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session id absent"))
        .to_owned();
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{parent}/prompt"),
        r#"{"text":"delegate this","resume":true}"#,
    );
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    wait_calls(&provider.calls, 1);
    let marker = root.join("child-preparing");
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap_or_else(|error| panic!("{error}")))
            .unwrap_or_else(|error| panic!("{error}"));
    config["mcp"] = serde_json::json!({"preparing": {
        "type":"local", "command":[if cfg!(windows) {"python"} else {"python3"},
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/mcp-required-startup-peer.py"),
            marker, "all-handshakes"],
        "timeout":{"startup":5_000,"catalog":1_000}
    }});
    fs::write(&path, config.to_string()).unwrap_or_else(|error| panic!("{error}"));
    approve_the_subagent_ask(address, &parent);
    let child = PollDeadline::new()
        .poll(Duration::from_millis(10), || {
            let sessions = json(address, "GET", "/api/session", "");
            marker
                .exists()
                .then(|| {
                    sessions["data"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|row| row["parentID"].as_str() == Some(parent.as_str()))
                        .and_then(|row| row["id"].as_str())
                        .map(str::to_owned)
                })
                .flatten()
        })
        .unwrap_or_else(|| panic!("child never reached MCP preparation"));
    let prompt_path = format!("/api/session/{child}/prompt");
    let (early_status, early_body) = request(
        address,
        "POST",
        &prompt_path,
        r#"{"text":"Keep the original task and use the human direction.","delivery":"steer","resume":true}"#,
    );
    entered
        .wait("child provider arrival", &provider.stop)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        early_status,
        200,
        "{}",
        String::from_utf8_lossy(&early_body)
    );
    let first_child = locked(&provider.bodies).get(1).cloned().unwrap_or_default();
    assert!(
        first_child.contains("summarise the repository")
            && first_child.contains("Keep the original task"),
        "preparation steering must preserve the original task: {first_child}"
    );
    let (status, body) = request(
        address,
        "POST",
        &prompt_path,
        r#"{"id":"msg_child_human_steer","text":"Follow the human direction instead.","delivery":"steer","resume":true}"#,
    );
    let (queued_status, queued_body) = request(
        address,
        "POST",
        &prompt_path,
        r#"{"id":"msg_child_human_queue","text":"Answer the busy queued followup.","delivery":"queue","resume":true}"#,
    );
    // Release the provider before checking refusals.
    release.release();
    assert_eq!(
        status,
        200,
        "direct child prompt refused: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        queued_status,
        200,
        "{}",
        String::from_utf8_lossy(&queued_body)
    );
    permission_residuals::wait_session_idle(address, &parent);
    permission_residuals::wait_session_idle(address, &child);
    let parent_path = format!("/api/session/{parent}/message");
    let parent_history = json(address, "GET", &parent_path, "");
    assert!(
        parent_history
            .to_string()
            .contains("The child followed the human direction."),
        "{parent_history}"
    );
    assert!(
        !parent_history
            .to_string()
            .contains("The busy queued answer belongs only to the child."),
        "{parent_history}"
    );
    let child_history = json(address, "GET", &format!("/api/session/{child}/message"), "");
    assert!(
        child_history
            .to_string()
            .contains("The busy queued answer belongs only to the child."),
        "{child_history}"
    );
    assert!(locked(&provider.bodies)
        .iter()
        .any(|body| body.contains("Follow the human direction instead.")));
    let (status, body) = request(
        address,
        "POST",
        &prompt_path,
        r#"{"text":"Now answer an independent followup.","resume":true}"#,
    );
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    permission_residuals::wait_session_idle(address, &child);
    let history = json(address, "GET", &format!("/api/session/{child}/message"), "");
    assert!(
        history
            .to_string()
            .contains("An independent child followup."),
        "{history}"
    );
    assert_eq!(
        json(address, "GET", &parent_path, ""),
        parent_history,
        "a later child execution must not replace the original tool answer"
    );
    scenario.finish();
}
