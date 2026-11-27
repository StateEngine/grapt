//! An unfinished checklist survives compaction and a fresh same-session reader.
//! Only the model is scripted; tool execution, compaction and history are real.
use super::*;

#[derive(Clone, Copy)]
enum Case {
    Completed,
    Failed,
    Cancelled,
}

fn plan(done: bool) -> serde_json::Value {
    serde_json::json!({"explanation":"Keep the current checklist", "plan":[
        {"step":"Reproduce","status":"completed"},
        {"step":"Fix","status":if done { "completed" } else { "in_progress" }},
        {"step":"Verify","status":if done { "completed" } else { "pending" }}
    ]})
}

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
    );
}

fn history(address: SocketAddr, session: &str) -> serde_json::Value {
    json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=200"),
        "",
    )
}

fn plans(history: &serde_json::Value) -> Vec<&serde_json::Value> {
    history["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "tool" && part["name"] == "update_plan")
        .collect()
}

fn result_count(bodies: &[String], call: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let mut count = 0;
    for body in bodies {
        let body: serde_json::Value = serde_json::from_str(body)?;
        for item in body["input"].as_array().into_iter().flatten() {
            if item["type"] == "function_call_output" && item["call_id"] == call {
                assert_eq!(item["output"], "Plan updated");
                count += 1;
            }
        }
    }
    Ok(count)
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn journey(case: Case, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root(label);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 200_000);
    let entered = Arc::new(terminal_coordination::Phase::new());
    let release = Arc::new(terminal_coordination::Phase::new());
    let disconnect = Arc::new(AtomicBool::new(false));
    let mut scripts = vec![
        Script::ToolInput {
            call: "call_pc_initial",
            tool: "update_plan",
            arguments: plan(false),
        },
        Script::SuccessText("The unfinished plan is stored."),
    ];
    match case {
        Case::Completed => scripts.push(Script::SuccessText(
            "## Objective\n- complete Fix then Verify; Reproduce is done",
        )),
        Case::Failed => scripts.push(Script::SuccessText("")),
        Case::Cancelled => scripts.push(Script::ControlledTerminalOutput {
            bytes: 1000,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            disconnect: Arc::clone(&disconnect),
        }),
    }
    if !matches!(case, Case::Cancelled) {
        scripts.push(Script::SuccessText("Continue with the existing checklist."));
    }
    scripts.extend([
        Script::ToolInput {
            call: "call_pc_final",
            tool: "update_plan",
            arguments: plan(true),
        },
        Script::SuccessText("The checklist is complete."),
    ]);
    let provider = ProviderFixture::start(scripts);
    let mut service = spawn(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let session = create(address);
    prompt(
        address,
        &session,
        "msg_pc_seed",
        "track the mixed-status plan",
    );
    manual_compaction::assert_carried(&root, "msg_pc_seed", "session.execution.succeeded.1");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let initial = history(address, &session);
    let rows = plans(&initial);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"]["input"], plan(false));
    assert_eq!(rows[0]["state"]["status"], "completed");

    let admitted = request(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    assert!(
        (200..300).contains(&admitted.0),
        "compact admission: {admitted:?}"
    );
    prompt(
        address,
        &session,
        "msg_pc_compact",
        "continue without replacing the checklist",
    );
    if matches!(case, Case::Cancelled) {
        entered.wait("plan compaction arrival", &provider.stop)?;
        let status = request(
            address,
            "POST",
            &format!("/api/session/{session}/interrupt"),
            "{}",
        )
        .0;
        // The interrupted client has closed this response; finish the owned
        // worker by disconnecting rather than writing a fake successful tail.
        disconnect.store(true, Ordering::SeqCst);
        release.release();
        assert_eq!(status, 204);
    }
    manual_compaction::assert_carried(
        &root,
        "msg_pc_compact",
        if matches!(case, Case::Cancelled) {
            "session.execution.interrupted.1"
        } else {
            "session.execution.succeeded.1"
        },
    );
    permission_residuals::wait_session_idle(address, &session);
    let expected_calls = match case {
        Case::Completed => 4,
        Case::Failed => 4,
        Case::Cancelled => 3,
    };
    assert_eq!(provider.calls.load(Ordering::SeqCst), expected_calls);
    let before_reopen = history(address, &session);
    let compacted = before_reopen["data"]
        .as_array()
        .ok_or("history page absent")?
        .iter()
        .filter(|row| row["type"] == "compaction")
        .count();
    assert_eq!(compacted, usize::from(matches!(case, Case::Completed)));
    let rows = plans(&before_reopen);
    assert_eq!(
        rows.len(),
        1,
        "compaction replayed or lost the plan: {before_reopen}"
    );
    assert_eq!(rows[0], plans(&initial)[0]);
    assert!(locked(&provider.bodies)[2]
        .contains("Output exactly the Markdown structure shown inside <template>"));

    // Reopen the actual shipped client twice while this exact service stays
    // alive; attaching reads history and must not manufacture a tool call.
    let service_path = root.to_str().ok_or("service path is not UTF-8")?;
    for _ in 0..2 {
        let attached = cli_continuity::cli(
            &root,
            &["attach", "--service", service_path, "--resume", "--jsonl"],
        );
        assert_eq!(attached.code, Some(0), "attach failed: {}", attached.stderr);
        assert_eq!(attached.receipt()["session"], session);
        let reopened = json(address, "GET", &format!("/api/session/{session}"), "");
        assert_eq!(reopened["data"]["id"], session);
        assert_eq!(history(address, &session), before_reopen);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            expected_calls,
            "reopen called the provider"
        );
    }
    prompt(
        address,
        &session,
        "msg_pc_complete",
        "complete Fix and Verify now",
    );
    manual_compaction::assert_carried(&root, "msg_pc_complete", "session.execution.succeeded.1");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), expected_calls + 2);
    let completed = history(address, &session);
    let rows = plans(&completed);
    assert_eq!(rows.len(), 2, "expected exactly one new update, no replay");
    assert_eq!(rows[0]["state"]["input"], plan(false));
    assert_eq!(rows[1]["state"]["input"], plan(true));
    assert_eq!(rows[1]["state"]["status"], "completed");
    let bodies = locked(&provider.bodies);
    assert_eq!(
        result_count(&bodies[expected_calls..], "call_pc_final")?,
        1,
        "the next real plan result must reach the provider once"
    );
    drop(bodies);
    provider.settle_terminal_workers()?;
    service.stop_after_witness(&root);
    drop(service);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn mixed_plan_survives_compaction_and_same_session_reopen_without_replay(
) -> Result<(), Box<dyn std::error::Error>> {
    journey(Case::Completed, "plan-compact-reopen")
}
#[test]
fn failed_compaction_preserves_the_unfinished_plan_without_replay(
) -> Result<(), Box<dyn std::error::Error>> {
    journey(Case::Failed, "plan-compact-failed")
}
#[test]
fn cancelled_compaction_preserves_the_unfinished_plan_without_replay(
) -> Result<(), Box<dyn std::error::Error>> {
    journey(Case::Cancelled, "plan-compact-cancelled")
}
