//! How a subagent child SETTLES when no one is left to answer.
//!
//! Split from `subagent_child.rs`, which holds what a child ANSWERS. The seam
//! is the durable claim each witness makes: the tests here all assert what the
//! record says about a child whose parent stopped waiting — interrupted,
//! refused admission, lost with its runtime, or over budget — and every one of
//! them reads the row rather than the screen.

use super::subagent_child::approve_the_subagent_ask;
use super::*;

/// A parent's interrupt reaches its RUNNING child.
///
/// Every other built-in tool is short enough that an ignored cancellation costs
/// nothing. A subagent is not: it can run for the whole budget, so an
/// interrupted parent would leave a child spending capacity and provider budget
/// for an answer whose Step no longer exists — the same orphan the timeout path
/// already refuses to create, arriving by the other door.
///
/// The child's own provider turn is HELD open here, so the child is genuinely
/// mid-run at the moment the parent is interrupted; the release afterwards lets
/// the fixture finish rather than leaving the assertion to a timeout.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn interrupting_a_parent_cancels_the_child_it_is_waiting_on() {
    let _journey = process_journey_custody();
    let root = root("subagent-child-interrupt");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 8_000);
    attended(&root);
    let child_entered = Arc::new(Barrier::new(2));
    let release_child = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_subagent_interrupt",
            task: "this child will be interrupted",
        },
        // The CHILD's turn, held open until this test releases it.
        Script::HeldOutput {
            bytes: 32,
            entered: Arc::clone(&child_entered),
            release: Arc::clone(&release_child),
        },
        // Never reached if the parent's interruption really ends the call.
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");

    let session_id = create(address);
    let prompt_path = format!("/api/session/{session_id}/prompt");
    let prompt_address = address;
    let prompt = thread::spawn(move || {
        request(
            prompt_address,
            "POST",
            &prompt_path,
            r#"{"id":"msg_subagent_interrupt","text":"delegate this","delivery":"queue","resume":true}"#,
        )
    });

    approve_the_subagent_ask(address, &session_id);

    // THE CHILD IS NOW GENUINELY RUNNING: its own provider request has arrived
    // and is being held. Interrupting before this point would prove nothing
    // about a running child.
    let _ = child_entered.wait();
    let active = json(address, "GET", "/api/session/active", "");
    let child_id = active["data"]
        .as_object()
        .unwrap_or_else(|| panic!("active projection was not an object: {active}"))
        .keys()
        .find(|id| *id != &session_id)
        .unwrap_or_else(|| panic!("the child is not active while its turn is held: {active}"))
        .to_owned();
    let execution = active["data"][&session_id]["executionID"]
        .as_str()
        .unwrap_or_else(|| panic!("the parent has no active execution: {active}"))
        .to_owned();

    let (interrupt_status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session_id}/interrupt"),
        &format!(r#"{{"executionID":"{execution}"}}"#),
    );
    assert_eq!(interrupt_status, 204, "the parent must be interruptible");

    // The child must leave the active map WHILE ITS PROVIDER TURN IS STILL
    // HELD. Releasing first would let it finish on its own and prove nothing:
    // a child that was never cancelled also stops running eventually.
    let cancelled =
        permission_residuals::wait_until(Duration::from_secs(6), Duration::from_millis(10), || {
            let active = json(address, "GET", "/api/session/active", "");
            active["data"].get(&child_id).is_none()
        })
        .is_some();
    let active_now = json(address, "GET", "/api/session/active", "");
    // Release the held provider turn: whatever the child would have said, it
    // must not become an answer now.
    let _ = release_child.wait();
    assert!(
        cancelled,
        "the interrupted parent left its child running: {active_now}"
    );

    let _ = prompt
        .join()
        .unwrap_or_else(|_| panic!("the subagent prompt thread panicked"));
    permission_residuals::wait_session_idle(address, &session_id);
    permission_residuals::wait_session_idle(address, &child_id);

    // THE CHILD'S OWN STEP DID NOT SUCCEED. Its provider turn was released
    // after the cancellation, so a child that had merely been left alone would
    // have finished and committed that text as an ordinary answer.
    let child_history = json(
        address,
        "GET",
        &format!("/api/session/{child_id}/message"),
        "",
    );
    let child_assistant = child_history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("child history was not an array"))
        .iter()
        .find(|message| message["type"] == "assistant")
        .cloned()
        .unwrap_or_else(|| panic!("the child opened no Step at all: {child_history}"));
    assert_ne!(
        child_assistant["finish"], "stop",
        "a cancelled child must not settle as an ordinary completion: {child_assistant}"
    );
    assert_eq!(
        child_assistant["error"]["type"], "aborted",
        "the child's Step must record the interruption: {child_assistant}"
    );

    // AND THE PARENT'S CALL FAILED rather than reporting an answer. The parent
    // Step is itself interrupted, so what matters is that the subagent call
    // carries no result.
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
        .find(|part| part["id"].as_str() == Some("call_subagent_interrupt"))
        .cloned()
        .unwrap_or_else(|| panic!("the parent records no subagent call: {parent_history}"));
    assert_eq!(
        call["state"]["status"], "error",
        "an interrupted subagent call must not report a result: {call}"
    );

    // AND THE OPERATION ROW SAYS SO, WHILE THIS RUNTIME IS STILL THE LIVE ONE.
    //
    // Review blocker B1, and the half I had missed. The settlement sits after the
    // await, and an interrupted parent DROPS that future — so the row stayed
    // `running` with a live generation, and only the NEXT startup touched it,
    // relabelling it `process-loss`. Two falsehoods: it was not running, and it
    // was not lost with its runtime.
    //
    // The product is stopped WITHOUT restarting it, so nothing recovery-shaped
    // can have written this row: what is asserted here is what the interrupted
    // process itself recorded at the time.
    child
        .kill()
        .unwrap_or_else(|error| panic!("interrupt inspection kill failed: {error}"));
    child
        .wait()
        .unwrap_or_else(|error| panic!("interrupt inspection wait failed: {error}"));
    let store = EventStore::open(
        &root.join("database.sqlite3"),
        "subagent-interrupt-inspection",
    )
    .unwrap_or_else(|error| panic!("interrupt inspection store failed: {error}"));
    let (status, reason, generation): (String, Option<String>, Option<i64>) = store
        .connection()
        .query_row(
            "SELECT status, reason, generation FROM subagent_operation",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap_or_else(|error| panic!("interrupt inspection read failed: {error}"));
    assert_eq!(
        (status.as_str(), reason.as_deref(), generation),
        ("cancelled", Some("interrupted"), None),
        "the interrupting runtime must settle its own operation as cancelled,          not leave it running for a later startup to call process loss"
    );
    let rows: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM subagent_operation", [], |row| {
            row.get(0)
        })
        .unwrap_or_else(|error| panic!("interrupt inspection count failed: {error}"));
    assert_eq!(rows, 1, "one call admitted one operation, settled once");
    drop(store);

    // AND A LATER RUNTIME LEAVES IT ALONE. The settlement and the drop guard
    // can both fire for one operation, and startup recovery runs over every
    // row it finds — so the question a reader actually has is whether any of
    // that can overwrite a truthful `cancelled` with `process-loss`. It cannot:
    // recovery moves only rows still `running`, and the CAS refuses a second
    // settlement. Asserted rather than reasoned, because this is the exact
    // relabel that made the original defect two lies instead of one.
    for stale in [
        "registration",
        "registration.elected",
        "registration.lock",
        "serve-ready.json",
    ] {
        let path = root.join(stale);
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
    }
    let mut restarted = spawn(&root, provider.address);
    let _ = await_ready(&root, &mut restarted);
    let store = EventStore::open(&root.join("database.sqlite3"), "subagent-interrupt-restart")
        .unwrap_or_else(|error| panic!("restart inspection store failed: {error}"));
    let (status, reason, generation): (String, Option<String>, Option<i64>) = store
        .connection()
        .query_row(
            "SELECT status, reason, generation FROM subagent_operation",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap_or_else(|error| panic!("restart inspection read failed: {error}"));
    assert_eq!(
        (status.as_str(), reason.as_deref(), generation),
        ("cancelled", Some("interrupted"), None),
        "a later runtime must not relabel a truthfully cancelled operation"
    );
    drop(store);
}

/// Capacity exhausted: the child is REFUSED, and the parent does not wait for a
/// slot.
///
/// This is the counterexample for the deadlock the re-entrancy ruling names.
/// A parent that waited for capacity while holding its own execution's slot
/// would be waiting for something only it could release. So the admission is
/// allowed to fail, and when it does the tool fails immediately rather than
/// queueing behind a permit.
///
/// The bound is reached honestly: sixty-three ordinary Sessions hold their
/// provider turns open, the parent's own execution takes the sixty-fourth, and
/// the child is the admission that finds nothing left. Nothing here lowers a
/// bound to make the refusal appear.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_child_that_cannot_be_admitted_fails_the_call_instead_of_waiting() {
    let _journey = process_journey_custody();
    const HELD: usize = 63;
    let root = root("subagent-child-capacity");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 300_000);
    attended(&root);
    // Held responses must outlive readiness, approval and refusal checks.
    let configuration = root.join("service.conf");
    let config = fs::read_to_string(&configuration)
        .unwrap_or_else(|error| panic!("capacity configuration read failed: {error}"));
    let head_limit = "provider_response_head_ms=30000\n";
    assert_eq!(config.matches(head_limit).count(), 1);
    fs::write(
        &configuration,
        config.replacen(head_limit, "provider_response_head_ms=300000\n", 1),
    )
    .unwrap_or_else(|error| panic!("capacity configuration write failed: {error}"));
    let entered = Arc::new(Barrier::new(HELD + 1));
    let release = Arc::new(Barrier::within(HELD + 1, Duration::from_secs(180)));
    let mut scripts: Vec<Script> = (0..HELD)
        .map(|_| Script::HeldOutput {
            bytes: 16,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        })
        .collect();
    scripts.push(Script::ToolSubagent {
        call: "call_subagent_capacity",
        task: "this child cannot be admitted",
    });
    scripts.push(Script::Success);
    let provider = ProviderFixture::start(scripts);
    let provider_bodies = Arc::clone(&provider.bodies);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let approver = approval::Attachment::open(address, "*");

    // Finish durable session creation before the first provider hold starts.
    let sessions: Vec<_> = (0..HELD).map(|_| create(address)).collect();
    let session_id = create(address);
    // Admit requests one at a time; the provider turns remain concurrent.
    // A burst would test the HTTP in-flight bound before execution capacity.
    for (index, session) in sessions.iter().enumerate() {
        let path = format!("/api/session/{session}/prompt");
        let (status, body) = request_with_read_timeout(
            address,
            "POST",
            &path,
            &format!(
                r#"{{"id":"msg_hold_{index}","text":"hold","delivery":"queue","resume":true}}"#
            ),
            Duration::from_secs(60),
        );
        assert_eq!(
            status,
            200,
            "capacity holder admission failed: {}",
            String::from_utf8_lossy(&body)
        );
    }
    // Every one of them has reached the provider, so every slot is taken before
    // the parent asks for a child.
    let _ = entered.wait();

    let prompt_path = format!("/api/session/{session_id}/prompt");
    let prompt_address = address;
    let prompt = thread::spawn(move || {
        request_with_read_timeout(
            prompt_address,
            "POST",
            &prompt_path,
            r#"{"id":"msg_subagent_capacity","text":"delegate this","delivery":"queue","resume":true}"#,
            Duration::from_secs(60),
        )
    });

    approve_the_subagent_ask(address, &session_id);

    // THE CALL ENDS WHILE EVERY SLOT IS STILL HELD. A tool that waited for
    // capacity would still be blocked here, and this join would not return
    // until the release below — which is the deadlock this proves against.
    let wait = PollDeadline::new();
    let parent_history = wait
        .poll(Duration::from_millis(10), || {
            let history = json(
                address,
                "GET",
                &format!("/api/session/{session_id}/message"),
                "",
            );
            let settled = history["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|message| message["content"].as_array())
                .flatten()
                .any(|part| {
                    part["id"].as_str() == Some("call_subagent_capacity")
                        && part["state"]["status"] != "running"
                });
            if !settled {
                return None;
            }
            Some(history)
        })
        .unwrap_or_else(|| panic!("the subagent call never settled while capacity was held"));

    let call = parent_history["data"]
        .as_array()
        .unwrap_or_else(|| panic!("parent history was not an array"))
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .find(|part| part["id"].as_str() == Some("call_subagent_capacity"))
        .cloned()
        .unwrap_or_else(|| panic!("the parent records no subagent call: {parent_history}"));
    assert_eq!(
        call["state"]["status"], "error",
        "an unadmitted child must fail its tool call: {call}"
    );

    // AND IT FAILED FOR THE RIGHT REASON. "error" alone would also be true of a
    // child that ran and failed, so the refusal itself is asserted: the tool's
    // detail artifact is folded into the continuation the parent sends onward,
    // which is where the concrete reason becomes observable.
    let named =
        permission_residuals::wait_until(Duration::from_secs(6), Duration::from_millis(10), || {
            let sent = provider_bodies
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .join(
                    "
",
                );
            sent.contains("the child was not admitted")
        })
        .is_some();
    assert!(
        named,
        "the refusal must name the admission, not some later failure"
    );

    let active = json(address, "GET", "/api/session/active", "");
    for session in &sessions {
        assert!(
            active["data"].get(session).is_some(),
            "capacity holder {session} ended before the refusal"
        );
    }
    let _ = release.wait();
    prompt
        .join()
        .unwrap_or_else(|_| panic!("capacity parent request panicked"));
    for session in sessions.iter().chain(std::iter::once(&session_id)) {
        permission_residuals::wait_session_idle(address, session);
    }
    drop(approver);
    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("capacity service cleanup failed: {error}"));
    assert!(
        output.status.success(),
        "capacity service refused cleanup: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A child whose runtime dies is settled on restart, and never replayed.
///
/// The tool call is the foreground operation's only runtime. If the process
/// holding it is lost, nothing in that process will ever settle the operation,
/// and a row left `running` is a durable lie: the next reconcile reads it as
/// work still in flight, under a generation whose runtime is gone.
///
/// So the replacement process settles it `Cancelled(ProcessLoss)` and does NOT
/// re-run it. Re-running is the worse failure of the two: the child may already
/// have spoken to a provider, and a silent second run would spend that budget
/// again and could act twice on the same task.
#[test]
fn a_child_left_running_by_a_lost_process_settles_as_process_loss() {
    let _journey = process_journey_custody();
    let root = root("subagent-child-restart");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 20_000);
    attended(&root);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_subagent_restart",
            task: "this child outlives its runtime",
        },
        Script::HeldOutput {
            bytes: 16,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");

    let session_id = create(address);
    let prompt_path = format!("/api/session/{session_id}/prompt");
    let prompt_address = address;
    let prompt = thread::spawn(move || {
        request(
            prompt_address,
            "POST",
            &prompt_path,
            r#"{"id":"msg_subagent_restart","text":"delegate this","delivery":"queue","resume":true}"#,
        )
    });

    approve_the_subagent_ask(address, &session_id);

    // The child is mid-run, so the operation is durably `running` under this
    // process's generation when the process is lost.
    let _ = entered.wait();
    child
        .kill()
        .unwrap_or_else(|error| panic!("subagent process-loss kill failed: {error}"));
    child
        .wait()
        .unwrap_or_else(|error| panic!("subagent process-loss wait failed: {error}"));
    let _ = release.wait();
    let _ = prompt.join();

    // The service manager owns stale registration cleanup before relaunch; this
    // witness invokes the binary directly, so it models that act without
    // touching the durable database under test.
    for stale in [
        "registration",
        "registration.elected",
        "registration.lock",
        "serve-ready.json",
    ] {
        let path = root.join(stale);
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
    }
    let calls_before_restart = provider.calls.load(Ordering::SeqCst);

    let mut restarted = spawn(&root, provider.address);
    let restarted_address = await_ready(&root, &mut restarted);
    let _approver = approval::Attachment::open(restarted_address, "*");

    // SETTLED, NOT RUNNING, and settled as what actually happened.
    let store = EventStore::open(
        &root.join("database.sqlite3"),
        "subagent-restart-inspection",
    )
    .unwrap_or_else(|error| panic!("restart inspection store failed: {error}"));
    let (status, reason, generation): (String, Option<String>, Option<i64>) = store
        .connection()
        .query_row(
            "SELECT status, reason, generation FROM subagent_operation",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap_or_else(|error| panic!("restart operation read failed: {error}"));
    assert_eq!(
        (status.as_str(), reason.as_deref(), generation),
        ("cancelled", Some("process-loss"), None),
        "a lost runtime's operation must settle as process loss, not stay running"
    );
    drop(store);

    // AND NOT REPLAYED. The child is not re-run, and nothing is projected
    // active: recovery settles, it does not resume.
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        calls_before_restart,
        "restart re-ran a child that had already spoken to a provider"
    );
    assert_eq!(
        json(restarted_address, "GET", "/api/session/active", "")["data"]
            .as_object()
            .map(serde_json::Map::len),
        Some(0),
        "the restarted process projected a dead child as active"
    );
}

/// A child that outruns its budget fails the call and is cancelled.
///
/// The budget is finite for the reason every bound here is: a child that never
/// settles must not hold its parent's Step open forever, and under a gate a
/// deadlock has to surface as a failure rather than as a hang. The production
/// budget is five minutes, which no gate can wait out, so this composition is
/// launched with a narrowed one — narrowed only; the seam cannot widen it.
///
/// What must be true is not just that the call fails: the child must not be
/// left running afterwards. A tool that timed out and walked away would leave
/// an orphan spending capacity and provider budget for an answer nobody will
/// read, which is the same orphan an interrupt would otherwise create.
#[test]
fn a_child_that_outruns_its_budget_fails_the_call_and_is_cancelled() {
    let _journey = process_journey_custody();
    let root = root("subagent-child-budget");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 20_000);
    attended(&root);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_subagent_budget",
            task: "this child will overrun",
        },
        Script::HeldOutput {
            bytes: 16,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        Script::Success,
    ]);
    let provider_bodies = Arc::clone(&provider.bodies);
    let mut child = spawn_with(
        &root,
        provider.address,
        &[("GRAPT_SUBAGENT_BUDGET_MS", "700")],
    );
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");

    let session_id = create(address);
    let prompt_path = format!("/api/session/{session_id}/prompt");
    let prompt_address = address;
    let prompt = thread::spawn(move || {
        request(
            prompt_address,
            "POST",
            &prompt_path,
            r#"{"id":"msg_subagent_budget","text":"delegate this","delivery":"queue","resume":true}"#,
        )
    });

    approve_the_subagent_ask(address, &session_id);

    // The child is still holding its provider turn when the budget runs out, so
    // the failure below is the budget and not the child finishing.
    let _ = entered.wait();

    // THE CALL FAILS ON THE BUDGET, named concretely: the tool's detail
    // artifact is folded into the continuation the parent sends onward.
    let named = permission_residuals::wait_until(
        Duration::from_secs(12),
        Duration::from_millis(10),
        || {
            let sent = provider_bodies
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .join("\n");
            sent.contains("did not settle within its budget")
        },
    )
    .is_some();
    assert!(named, "the refusal must name the budget it exceeded");

    // AND THE CHILD IS NOT LEFT RUNNING.
    let cancelled =
        permission_residuals::wait_until(Duration::from_secs(6), Duration::from_millis(10), || {
            let active = json(address, "GET", "/api/session/active", "");
            let children = active["data"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(session, _)| *session != &session_id)
                .count();
            children == 0
        })
        .is_some();
    let _ = release.wait();
    let _ = prompt.join();
    assert!(cancelled, "the timed-out call left its child running");
}
