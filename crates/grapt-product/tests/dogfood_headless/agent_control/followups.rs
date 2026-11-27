//! Real provider transport and native tool execution; no paid provider.
use super::*;

fn database(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("{error}"))
}

fn executions(root: &Path, child: &str) -> i64 {
    database(root)
        .query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_family='session'
        AND aggregate_id=?1 AND type='session.execution.started.1'",
            [child],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("{error}"))
}

pub(super) fn settled(root: &Path, child: &str, expected: i64) {
    let wait = PollDeadline::new();
    while wait.pending() {
        let closed: i64 = database(root)
            .query_row(
                "SELECT COUNT(*) FROM subagent_operation
            WHERE child_session_id=?1 AND status='completed'",
                [child],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("{error}"));
        if closed == expected {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("child did not complete {expected} runs ({wait})");
}

fn captured(root: &Path, child: &str, marker: &str) -> i64 {
    database(root).query_row("SELECT COUNT(*) FROM event e WHERE e.aggregate_id=?1
        AND e.type='session.input.admitted.1' AND json_extract(e.data,'$.input.type')='agent-message'
        AND json_extract(e.data,'$.input.data.text')=?2 AND EXISTS(
        SELECT 1 FROM event c WHERE c.id='evt_agent_mail_' || json_extract(e.data,'$.inputID'))",
        rusqlite::params![child,marker],|row|row.get(0)).unwrap_or_else(|error|panic!("{error}"))
}

#[test]
fn an_idle_followup_starts_one_successor_and_preserves_the_original_answer() {
    let _journey = process_journey_custody();
    let root = root("followup-idle");
    prepare(&root, 200_000);
    attended(&root);
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_initial_child",
            task: "answer the first task",
        },
        Script::SuccessText("original-child-answer"),
        Script::Success,
    ]);
    let mut process = spawn(&root, provider.address);
    let address = await_ready(&root, &mut process);
    let _approver = approval::Attachment::open(address, "*");
    let parent = create(address);
    start_prompt(address, &parent, "call_initial_child");
    permission_residuals::wait_session_idle(address, &parent);
    let page = json(address, "GET", "/api/session", "");
    let child = page["data"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["parentID"] == parent))
        .and_then(|row| row["id"].as_str())
        .unwrap_or_else(|| panic!("no child"))
        .to_owned();
    settled(&root, &child, 1);
    let before = executions(&root, &child);
    provider.append(vec![
        Script::ToolInput {
            call: "call_idle_followup",
            tool: "followup_task",
            arguments: serde_json::json!({"target":child,"message":"idle-followup-work-marker"}),
        },
        Script::SuccessText("new-followup-answer"),
        Script::SuccessText("new-followup-answer"),
    ]);
    start_prompt(address, &parent, "call_idle_followup");
    permission_residuals::wait_session_idle(address, &parent);
    settled(&root, &child, 2);
    permission_residuals::wait_session_idle(address, &child);
    assert_eq!(executions(&root, &child), before + 1);
    assert_eq!(captured(&root, &child, "idle-followup-work-marker"), 1);
    let db = database(&root);
    let original: String = db
        .query_row(
            "SELECT output FROM subagent_operation
        WHERE child_session_id=?1 AND NOT EXISTS(SELECT 1 FROM subagent_run r
          WHERE r.operation_id=subagent_operation.operation_id)",
            [&child],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(original.contains("original-child-answer"), "{original}");
    let (owner, state): (String, String) = db
        .query_row(
            "SELECT parent_session_id,status FROM subagent_operation
        WHERE operation_id=(SELECT operation_id FROM subagent_run WHERE child_session_id=?1)",
            [&child],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(owner, parent);
    assert_eq!(state, "completed");
    drop(db);
    process.stop_after_witness(&root);
    drop(process);
    process_fixture::cleanup(&root);
}

#[test]
fn an_active_child_captures_followup_without_starting_a_second_execution() {
    let _journey = process_journey_custody();
    let root = root("followup-active");
    prepare(&root, 200_000);
    attended(&root);
    let held_tool = || Script::ToolInput {
        call: "call_followup_hold",
        tool: "list_agents",
        arguments: serde_json::json!({}),
    };
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_followup_spawn",
            tool: "spawn_agent",
            arguments: serde_json::json!({"task_name":"inspector","message":"inspect the agent family"}),
        },
        held_tool(),
        held_tool(),
    ]);
    let mut process = spawn(&root, provider.address);
    let address = await_ready(&root, &mut process);
    let _approver = approval::Attachment::open(address, "*");
    let parent = create(address);
    start_prompt(address, &parent, "call_followup_spawn");
    let parent_ask = permission_boundaries::pending(address, &parent, "call_followup_hold");
    let child = tool(&provider, "call_followup_spawn")["agent_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no spawned child"))
        .to_owned();
    let child_ask = permission_boundaries::pending(address, &child, "call_followup_hold");
    provider.append(vec![Script::Success]);
    permission_boundaries::reply(address, &parent, &parent_ask, "once");
    permission_residuals::wait_session_idle(address, &parent);
    let before = executions(&root, &child);
    provider.append(vec![
        Script::ToolInput {
            call: "call_active_followup",
            tool: "followup_task",
            arguments: serde_json::json!({"target":child,"message":"active-followup-work-marker"}),
        },
        Script::Success,
    ]);
    start_prompt(address, &parent, "call_active_followup");
    permission_residuals::wait_session_idle(address, &parent);
    provider.append(vec![Script::SuccessText("active-child-followup-answer")]);
    permission_boundaries::reply(address, &child, &child_ask, "once");
    settled(&root, &child, 1);
    permission_residuals::wait_session_idle(address, &child);
    assert_eq!(captured(&root, &child, "active-followup-work-marker"), 1);
    assert_eq!(
        executions(&root, &child),
        before,
        "captured mail caused an extra execution"
    );
    let cancelled:i64=database(&root).query_row("SELECT COUNT(*) FROM subagent_followup WHERE child_session_id=?1 AND state='cancelled'",[&child],|row|row.get(0))
        .unwrap_or_else(|error|panic!("{error}"));
    assert_eq!(cancelled, 0, "captured active mail was reported cancelled");
    process.stop_after_witness(&root);
    drop(process);
    process_fixture::cleanup(&root);
}

#[test]
fn a_process_loss_cancels_pending_followup_without_replaying_a_provider_call() {
    let _journey = process_journey_custody();
    let mut pair = RunningPair::start("followup-process-loss");
    pair.parent_release.release();
    permission_residuals::wait_session_idle(pair.address, &pair.parent);
    pair.call(
        &pair.parent,
        "call_before_loss",
        "followup_task",
        serde_json::json!({"target":pair.child,"message":"pending-before-process-loss"}),
    );
    assert_eq!(tool(&pair.provider, "call_before_loss")["status"], "queued");
    let before = executions(&pair.root, &pair.child);
    pair.process.stop_after_witness(&pair.root);
    pair.child_disconnect.store(true, Ordering::SeqCst);
    pair.child_release.release();
    pair.provider
        .settle_terminal_workers()
        .unwrap_or_else(|error| panic!("{error}"));
    let mut resumed = spawn(&pair.root, pair.provider.address);
    let address = await_ready(&pair.root, &mut resumed);
    let _approver = approval::Attachment::open(address, "*");
    let (state, reason): (String, String) = database(&pair.root)
        .query_row(
            "SELECT state,reason FROM subagent_followup WHERE child_session_id=?1",
            [&pair.child],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(state, "cancelled");
    assert_eq!(reason, "process-loss");
    pair.provider.append(vec![
        Script::ToolInput {
            call: "call_after_loss",
            tool: "list_agents",
            arguments: serde_json::json!({}),
        },
        Script::Success,
    ]);
    start_prompt(address, &pair.parent, "call_after_loss");
    permission_residuals::wait_session_idle(address, &pair.parent);
    let page = tool(&pair.provider, "call_after_loss");
    assert_eq!(page["agents"][0]["latest_followup"]["status"], "cancelled");
    assert_eq!(
        page["agents"][0]["latest_followup"]["reason"],
        "process-loss"
    );
    assert_eq!(page["agents"][0]["queued_followups"], 0);
    assert_eq!(
        executions(&pair.root, &pair.child),
        before,
        "restart executed abandoned followup without a new instruction"
    );
    resumed.stop_after_witness(&pair.root);
    drop(resumed);
    // RunningPair's original service is already reaped; finish its fixture here.
    drop(pair.process);
    process_fixture::cleanup(&pair.root);
}
