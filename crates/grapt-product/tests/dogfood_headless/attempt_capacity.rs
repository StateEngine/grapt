//! SC-REL-13: one provider adapter serves every Session in the process, and
//! its active-attempt capacity is shared by all of them. Row A runs with a
//! capacity of 1. There a background subagent, forked while its parent's
//! stream still held the only slot, asked for its compaction summary at once;
//! the summary was refused ("the adapter already holds its capacity of 1
//! active attempts"), automatic compaction paused as if the failure were
//! permanent, and the child's work was lost. A parent and its child are two
//! Sessions on that one adapter, so two Sessions reproduce it without a
//! scheduler race: a summary call now waits, bounded, for the slot instead of
//! meeting whatever stream happens to be open.
use super::*;
use terminal_coordination::Phase;

const SUMMARY_REQUEST: &str = "Output exactly the Markdown structure shown inside <template>";

fn one_slot(root: &Path) {
    let config = root.join("service.conf");
    let text = fs::read_to_string(&config)
        .unwrap_or_else(|error| panic!("service config unreadable: {error}"));
    assert!(text.contains("provider_active_attempts=64\n"));
    fs::write(
        &config,
        text.replace(
            "provider_active_attempts=64\n",
            "provider_active_attempts=1\n",
        ),
    )
    .unwrap_or_else(|error| panic!("service config failed: {error}"));
    child_slots(root, 1);
}

fn child_slots(root: &Path, children: usize) {
    let profile = root.join("grapt.jsonc");
    let mut settings: serde_json::Value = serde_json::from_slice(
        &fs::read(&profile).unwrap_or_else(|error| panic!("profile read: {error}")),
    )
    .unwrap_or_else(|error| panic!("profile parse: {error}"));
    settings["provider_capacity"] = serde_json::json!({"children":children});
    fs::write(profile, settings.to_string())
        .unwrap_or_else(|error| panic!("child budget write: {error}"));
}

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
    );
}

fn compactions_ended(root: &Path, session: &str) -> usize {
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("capacity store read failed: {error}"));
    db.busy_timeout(Duration::from_secs(2))
        .unwrap_or_else(|error| panic!("capacity read bound failed: {error}"));
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM event WHERE type = 'session.compaction.ended.2' AND aggregate_id = ?1",
            [session],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("capacity compaction count unreadable: {error}"));
    usize::try_from(count).unwrap_or_else(|error| panic!("capacity count invalid: {error}"))
}

#[test]
fn a_compaction_waits_for_another_sessions_stream_on_a_one_slot_adapter() {
    let _journey = process_journey_custody();
    let root = root("attempt-capacity-compaction");
    prepare(&root, 200_000);
    attended(&root);
    one_slot(&root);
    fs::write(root.join("seed.txt"), vec![b's'; 7_524])
        .unwrap_or_else(|error| panic!("seed fixture write failed: {error}"));
    let (entered, release) = (Arc::new(Phase::new()), Arc::new(Phase::new()));
    let provider = ProviderFixture::start(vec![
        // A tool output the summary can drop, as the manual compaction seed.
        Script::ToolRead {
            call: "call_capacity_seed",
            path: "seed.txt",
        },
        Script::SuccessText("seeded"),
        // The other Session's stream: it holds the only slot until released.
        Script::ControlledTerminalOutput {
            bytes: 64,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            disconnect: Arc::new(AtomicBool::new(false)),
        },
        Script::SuccessText("## Objective\n- continue the user task after compaction"),
        Script::SuccessText("answered after the compaction"),
    ]);
    let mut process = spawn(&root, provider.address);
    let address = await_ready(&root, &mut process);
    let _approver = approval::Attachment::open(address, "*");
    let compacting = create(address);
    prompt(address, &compacting, "msg_capacity_seed", "start the task");
    compaction_continuity::approve(address, &compacting, "call_capacity_seed");
    manual_compaction::assert_carried(&root, "msg_capacity_seed", "session.execution.succeeded.1");
    permission_residuals::wait_session_idle(address, &compacting);
    json(
        address,
        "POST",
        &format!("/api/session/{compacting}/compact"),
        "{}",
    );

    let holding = create(address);
    prompt(address, &holding, "msg_capacity_holding", "keep working");
    entered
        .wait("the holding session's provider stream", &provider.stop)
        .unwrap_or_else(|error| panic!("{error}"));
    prompt(
        address,
        &compacting,
        "msg_capacity_next",
        "continue the task",
    );
    // Long enough for a refused summary to have failed the turn.
    thread::sleep(Duration::from_secs(2));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "the summary reached the provider past a full adapter"
    );
    release.release();

    manual_compaction::assert_carried(
        &root,
        "msg_capacity_holding",
        "session.execution.succeeded.1",
    );
    manual_compaction::assert_carried(&root, "msg_capacity_next", "session.execution.succeeded.1");
    permission_residuals::wait_session_idle(address, &compacting);
    assert_eq!(
        compactions_ended(&root, &compacting),
        1,
        "the requested compaction did not end"
    );
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 5);
    assert!(
        bodies[3].contains(SUMMARY_REQUEST) && !bodies[4].contains(SUMMARY_REQUEST),
        "the summary did not follow the holding stream"
    );
    provider
        .settle_terminal_workers()
        .unwrap_or_else(|error| panic!("{error}"));
    process.stop_after_witness(&root);
    drop(process);
    process_fixture::cleanup(&root);
}

/// Row A lost two more subagents at step 1: their first attempt was refused
/// with the same capacity error while another Session streamed. A first
/// attempt now waits for the slot as the summary call does.
#[test]
fn a_first_attempt_waits_for_another_sessions_stream_on_a_one_slot_adapter() {
    let _journey = process_journey_custody();
    let root = root("attempt-capacity-first-step");
    prepare(&root, 200_000);
    attended(&root);
    one_slot(&root);
    let (entered, release) = (Arc::new(Phase::new()), Arc::new(Phase::new()));
    let provider = ProviderFixture::start(vec![
        Script::ControlledTerminalOutput {
            bytes: 64,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            disconnect: Arc::new(AtomicBool::new(false)),
        },
        Script::SuccessText("the waiting session's first answer"),
    ]);
    let mut process = spawn(&root, provider.address);
    let address = await_ready(&root, &mut process);
    let _approver = approval::Attachment::open(address, "*");
    let holding = create(address);
    prompt(address, &holding, "msg_first_holding", "keep working");
    entered
        .wait("the holding session's provider stream", &provider.stop)
        .unwrap_or_else(|error| panic!("{error}"));
    let waiting = create(address);
    prompt(address, &waiting, "msg_first_waiting", "start the task");
    // Long enough for a refused first attempt to have failed the turn.
    thread::sleep(Duration::from_secs(2));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "the first attempt reached the provider past a full adapter"
    );
    release.release();

    manual_compaction::assert_carried(&root, "msg_first_holding", "session.execution.succeeded.1");
    manual_compaction::assert_carried(&root, "msg_first_waiting", "session.execution.succeeded.1");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    provider
        .settle_terminal_workers()
        .unwrap_or_else(|error| panic!("{error}"));
    process.stop_after_witness(&root);
    drop(process);
    process_fixture::cleanup(&root);
}

#[path = "attempt_capacity/children.rs"]
mod children;

#[path = "attempt_capacity/nested.rs"]
mod nested;

#[path = "attempt_capacity/rate_limit.rs"]
mod rate_limit;
