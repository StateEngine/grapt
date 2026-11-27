//! `/compact` (CODING-QUALITY-01, option iii): an admitted manual
//! compaction runs inside the next input's first Step, through the real
//! drain, and always settles. The request is admitted while the service is
//! stopped, so each journey also crosses a restart with it pending.
use super::*;
use grapt_inbox::{CompactionAdmitRequest, Inbox};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{EventId, MessageId};

#[path = "manual_compaction/cache_prefix.rs"]
mod cache_prefix;

const SUMMARY_REQUEST: &str = "Output exactly the Markdown structure shown inside <template>";
const REQUEST: &str = "msg_manual_compact";

fn store(root: &Path) -> EventStore {
    EventStore::open(&root.join("database.sqlite3"), "live-headless1")
        .unwrap_or_else(|error| panic!("offline store open failed: {error}"))
}

pub(super) fn admit_offline(root: &Path, session: &str) {
    let session: SessionId = session
        .parse()
        .unwrap_or_else(|_| panic!("session id refused"));
    let mut store = store(root);
    store
        .elect_writer(&AggregateId::Session(session.clone()))
        .unwrap_or_else(|error| panic!("offline writer election failed: {error}"));
    let mut inbox = Inbox::new(store).unwrap_or_else(|error| panic!("inbox failed: {error}"));
    let event: EventId = "evt_manual_compact_admit"
        .parse()
        .unwrap_or_else(|_| panic!("event id refused"));
    let input: MessageId = REQUEST
        .parse()
        .unwrap_or_else(|_| panic!("input id refused"));
    inbox
        .admit_compaction(CompactionAdmitRequest {
            event_id: &event,
            session: &session,
            input: &input,
            created_ms: 1_789_560_000_000,
            instructions: None,
        })
        .unwrap_or_else(|error| panic!("offline /compact admission failed: {error}"));
}

/// Durable payloads of one event type, in order.
fn events(root: &Path, kind: &str) -> Vec<serde_json::Value> {
    let store = store(root);
    let mut query = store
        .connection()
        .prepare("SELECT data FROM event WHERE type = ?1 ORDER BY seq")
        .unwrap_or_else(|error| panic!("event query failed: {error}"));
    query
        .query_map([kind], |row| row.get::<_, String>(0))
        .unwrap_or_else(|error| panic!("event read failed: {error}"))
        .map(|data| {
            serde_json::from_str(&data.unwrap_or_else(|error| panic!("row failed: {error}")))
                .unwrap_or_else(|error| panic!("event data is not JSON: {error}"))
        })
        .collect()
}

/// The durable terminal of the execution the input `input` opened: the first
/// execution started after that input's promotion, matched by its execution
/// id. Read-only, so it runs while the service still owns the store. `None`
/// only after successful reads find no such terminal through the allowance.
fn carrying_terminal(root: &Path, input: &str, allowance: Duration) -> Option<String> {
    let read = || -> rusqlite::Result<Option<String>> {
        let db = rusqlite::Connection::open_with_flags(
            root.join("database.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        db.query_row(
            "SELECT t.type FROM event t
             WHERE t.type IN ('session.execution.succeeded.1',
                              'session.execution.failed.1',
                              'session.execution.interrupted.1')
               AND t.aggregate_id = (SELECT p.aggregate_id FROM event p
                   WHERE p.type = 'session.input.promoted.1'
                     AND json_extract(p.data, '$.inputID') = ?1)
               AND json_extract(t.data, '$.executionID') = (
                 SELECT json_extract(s.data, '$.executionID') FROM event s
                 WHERE s.type = 'session.execution.started.1'
                   AND s.aggregate_id = t.aggregate_id
                   AND s.seq > (SELECT p.seq FROM event p
                                WHERE p.type = 'session.input.promoted.1'
                                  AND json_extract(p.data, '$.inputID') = ?1)
                 ORDER BY s.seq LIMIT 1)
             ORDER BY t.seq LIMIT 1",
            [input],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
    };
    // Only a successful query that finds no terminal is absence. A busy or
    // locked store is retried within the bound; any other read failure, or
    // a store that stays busy past it, fails the journey.
    let mut busy = false;
    let wait = PollDeadline::within(allowance);
    while wait.pending() {
        match read() {
            Ok(Some(terminal)) => return Some(terminal),
            Ok(None) => busy = false,
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                busy = true;
            }
            Err(error) => panic!("execution terminal read failed for {input}: {error}"),
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !busy,
        "the store stayed busy; {input}'s terminal was never read: {wait}"
    );
    None
}

/// Waits, within the fixture bound, for the carrying input's own terminal and
/// fails on its absence before the service is stopped.
pub(super) fn assert_carried(root: &Path, input: &str, expected: &str) {
    assert_eq!(
        carrying_terminal(root, input, Duration::from_secs(60)).as_deref(),
        Some(expected),
        "{input} has no execution terminal of its own"
    );
}

fn pending(root: &Path) -> i64 {
    store(root)
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM session_pending WHERE input_type = 'compaction'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("pending query failed: {error}"))
}

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &format!(r#"{{"id":"{id}","text":"{text}","delivery":"queue","resume":true}}"#),
    );
}

/// A seeded Session (one tool Step and an answer), stopped, `/compact`
/// admitted, restarted: returns the running service and its Session.
fn seeded(
    label: &str,
    summary: Script,
) -> (
    PathBuf,
    ProviderFixture,
    process_fixture::ManagedChild,
    SocketAddr,
    String,
) {
    seeded_with(label, vec![summary])
}

/// As [`seeded`], with every summary call the request makes.
fn seeded_with(
    label: &str,
    summaries: Vec<Script>,
) -> (
    PathBuf,
    ProviderFixture,
    process_fixture::ManagedChild,
    SocketAddr,
    String,
) {
    seeded_with_instructions(label, summaries, None)
}

fn seeded_with_instructions(
    label: &str,
    summaries: Vec<Script>,
    instructions: Option<&str>,
) -> (
    PathBuf,
    ProviderFixture,
    process_fixture::ManagedChild,
    SocketAddr,
    String,
) {
    seeded_configured(label, summaries, instructions, &[])
}

fn seeded_configured(
    label: &str,
    summaries: Vec<Script>,
    instructions: Option<&str>,
    hook_events: &[&str],
) -> (
    PathBuf,
    ProviderFixture,
    process_fixture::ManagedChild,
    SocketAddr,
    String,
) {
    let root = root(label);
    prepare(&root, 120_000);
    if !hook_events.is_empty() {
        lifecycle_hooks::configure_events(&root, hook_events)
            .unwrap_or_else(|error| panic!("compaction hook setup failed: {error}"));
    }
    fs::write(root.join("seed.txt"), vec![b's'; 7_524])
        .unwrap_or_else(|error| panic!("seed fixture write failed: {error}"));
    let mut scripts = vec![
        Script::ToolRead {
            call: "call_manual_seed",
            path: "seed.txt",
        },
        Script::SuccessText("seeded"),
    ];
    scripts.extend(summaries);
    scripts.push(Script::SuccessText("answered after compact"));
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    prompt(address, &session, "msg_manual_seed", "read the seed");
    compaction_continuity::approve(address, &session, "call_manual_seed");
    permission_residuals::wait_session_idle(address, &session);
    if let Some(instructions) = instructions {
        let admitted = json(
            address,
            "POST",
            &format!("/api/session/{session}/compact"),
            &serde_json::json!({"id":REQUEST,"instructions":instructions}).to_string(),
        );
        assert_eq!(admitted["data"]["type"], "compaction");
    }
    child.stop_after_witness(&root);
    drop(child);
    if instructions.is_none() {
        admit_offline(&root, &session);
    }
    assert_eq!(pending(&root), 1, "the admitted /compact is not pending");
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    (root, provider, child, address, session)
}

#[test]
fn a_requested_compaction_runs_in_the_next_inputs_first_step_and_settles() {
    let _journey = process_journey_custody();
    let (root, provider, mut child, address, session) = seeded_with_instructions(
        "manual-compact-runs",
        vec![Script::SuccessText("## Objective\n- the seed was read")],
        Some("Preserve the seed decision after restart"),
    );
    prompt(address, &session, "msg_manual_next", "continue");
    permission_residuals::wait_session_idle(address, &session);
    assert_carried(&root, "msg_manual_next", "session.execution.succeeded.1");
    child.stop_after_witness(&root);
    drop(child);

    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let bodies = locked(&provider.bodies).clone();
    assert!(
        bodies[2].contains(SUMMARY_REQUEST),
        "the next input's first call was not the summary"
    );
    assert!(bodies[2].contains("Preserve the seed decision after restart"));
    assert!(!bodies[3].contains("Preserve the seed decision after restart"));
    let ended = events(&root, "session.compaction.ended.2");
    assert_eq!(ended.len(), 1, "no descriptor: {ended:?}");
    assert_eq!(ended[0]["trigger"], "userRequested");
    assert_eq!(
        pending(&root),
        0,
        "the descriptor did not settle the request"
    );
    assert!(events(&root, "session.compaction.failed.1").is_empty());
    process_fixture::cleanup(&root);
}

#[test]
fn a_failed_requested_compaction_settles_and_the_message_still_runs() {
    let _journey = process_journey_custody();
    // An empty summary is no progress: the request fails, the user's message
    // does not.
    let (root, provider, mut child, address, session) = seeded_with_instructions(
        "manual-compact-fails",
        vec![Script::SuccessText("")],
        Some("Preserve the seed decision through retries"),
    );
    prompt(address, &session, "msg_manual_next", "continue");
    permission_residuals::wait_session_idle(address, &session);
    assert_carried(&root, "msg_manual_next", "session.execution.succeeded.1");
    child.stop_after_witness(&root);
    drop(child);

    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let bodies = locked(&provider.bodies).clone();
    assert!(bodies[2].contains("Preserve the seed decision through retries"));
    assert!(!bodies[3].contains("Preserve the seed decision through retries"));
    let heads = locked(&provider.heads).clone();
    assert_eq!(heads.len(), 4);
    for head in &heads {
        cache_scope::assert_thread_headers(head, &session);
    }
    // The summary preserves the coding cache key across restart.
    let warm: serde_json::Value = serde_json::from_str(&bodies[2])
        .unwrap_or_else(|error| panic!("warm summary body: {error}"));
    let seed: serde_json::Value = serde_json::from_str(&bodies[0])
        .unwrap_or_else(|error| panic!("seed coding body: {error}"));
    assert!(warm["prompt_cache_key"].is_string());
    assert_eq!(warm["prompt_cache_key"], seed["prompt_cache_key"]);
    let failed = events(&root, "session.compaction.failed.1");
    assert_eq!(failed.len(), 1, "no manual failure: {failed:?}");
    assert_eq!(failed[0]["reason"], "manual");
    assert_eq!(failed[0]["error"]["type"], "compaction.failed");
    assert_eq!(failed[0]["inputID"], REQUEST);
    assert_eq!(pending(&root), 0, "a failed request stayed pending");
    assert!(events(&root, "session.compaction.ended.2").is_empty());
    process_fixture::cleanup(&root);
}

#[test]
fn an_interrupted_requested_compaction_settles_aborted() {
    let _journey = process_journey_custody();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (root, provider, mut child, address, session) = seeded(
        "manual-compact-interrupted",
        Script::HeldOutput {
            bytes: 1_000,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
    );
    prompt(address, &session, "msg_manual_next", "continue");
    entered.wait();
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        "{}",
    );
    assert_eq!(status, 204);
    release.wait();
    permission_residuals::wait_session_idle(address, &session);
    assert_carried(&root, "msg_manual_next", "session.execution.interrupted.1");
    child.stop_after_witness(&root);
    drop(child);

    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    let failed = events(&root, "session.compaction.failed.1");
    assert_eq!(failed.len(), 1, "no manual failure: {failed:?}");
    assert_eq!(failed[0]["error"]["type"], "aborted");
    assert_eq!(failed[0]["inputID"], REQUEST);
    assert_eq!(pending(&root), 0, "an interrupted request stayed pending");
    process_fixture::cleanup(&root);
}

/// The process dies while the requested compaction is running. The request
/// stays pending across the restart, runs again in the next input's first
/// Step, and settles; that input is answered.
#[test]
fn a_requested_compaction_cut_by_process_loss_runs_again_and_settles() {
    let _journey = process_journey_custody();
    let root = root("manual-compact-process-loss");
    prepare(&root, 120_000);
    fs::write(root.join("seed.txt"), vec![b's'; 7_524])
        .unwrap_or_else(|error| panic!("seed fixture write failed: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_manual_seed",
            path: "seed.txt",
        },
        Script::SuccessText("seeded"),
        Script::StallBeforeFirstFragment,
        Script::SuccessText("## Objective\n- the seed was read"),
        Script::SuccessText("answered after the restart"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    prompt(address, &session, "msg_manual_seed", "read the seed");
    compaction_continuity::approve(address, &session, "call_manual_seed");
    permission_residuals::wait_session_idle(address, &session);
    child.stop_after_witness(&root);
    drop(child);
    admit_offline(&root, &session);

    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    prompt(address, &session, "msg_manual_cut", "continue");
    wait_calls(&provider.calls, 3);
    child
        .kill()
        .unwrap_or_else(|error| panic!("process-loss kill failed: {error}"));
    child
        .wait()
        .unwrap_or_else(|error| panic!("process-loss wait failed: {error}"));
    drop(child);
    assert_eq!(pending(&root), 1, "process loss settled the request");
    // As process_loss.rs: the service manager clears stale registration.
    for stale in [
        "registration",
        "registration.elected",
        "registration.lock",
        "serve-ready.json",
    ] {
        let path = root.join(stale);
        if path.exists() {
            fs::remove_file(&path)
                .unwrap_or_else(|error| panic!("stale {stale} removal failed: {error}"));
        }
    }

    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    prompt(address, &session, "msg_manual_after", "continue again");
    permission_residuals::wait_session_idle(address, &session);
    assert_carried(&root, "msg_manual_after", "session.execution.succeeded.1");
    // Negative control: the execution the process loss cut started but has no
    // terminal, while a later and an earlier one succeeded. A newest-terminal
    // read would call it succeeded; its own receipt is absent.
    assert_eq!(
        carrying_terminal(&root, "msg_manual_cut", Duration::from_millis(250)),
        None
    );
    child.stop_after_witness(&root);
    drop(child);

    assert_eq!(provider.calls.load(Ordering::SeqCst), 5);
    let bodies = locked(&provider.bodies).clone();
    assert!(bodies[2].contains(SUMMARY_REQUEST) && bodies[3].contains(SUMMARY_REQUEST));
    let ended = events(&root, "session.compaction.ended.2");
    assert_eq!(ended.len(), 1, "the rerun did not settle: {ended:?}");
    assert_eq!(ended[0]["trigger"], "userRequested");
    assert_eq!(pending(&root), 0);
    process_fixture::cleanup(&root);
}

/// Review HOLD on 9abec016, finding 3: the request meets a structural
/// history bound at its input's first Step (promoted inputs past the
/// declared-bytes backstop). The
/// requested compaction is the one that runs; it settles once, and no
/// automatic structural compaction runs beside it.
#[test]
fn a_requested_compaction_at_a_structural_bound_runs_once_and_settles() {
    const PROMOTED: usize = DECLARED_INPUTS;
    let _journey = process_journey_custody();
    let root = root("manual-compact-structural");
    prepare(&root, 700_000);
    prepare_model_inventory(&root, DECLARED_CONTEXT_WINDOW);
    let mut scripts = vec![Script::Success; PROMOTED - 1];
    scripts.push(Script::SuccessText(
        "## Objective\n- the conversation so far",
    ));
    scripts.push(Script::SuccessText("answered at the bound"));
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    for index in 0..PROMOTED - 1 {
        let input = format!("msg_manual_bound_{index:03}");
        prompt(
            address,
            &session,
            &input,
            &declared_input(&format!("promoted input {index}")),
        );
        // An idle display can precede an admitted wake taking custody. Each
        // seed input must actually settle before the next one is submitted.
        assert_carried(&root, &input, "session.execution.succeeded.1");
        permission_residuals::wait_session_idle(address, &session);
    }
    child.stop_after_witness(&root);
    drop(child);
    admit_offline(&root, &session);

    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let last = format!("msg_manual_bound_{:03}", PROMOTED - 1);
    prompt(
        address,
        &session,
        &last,
        &declared_input(&format!("promoted input {}", PROMOTED - 1)),
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_carried(&root, &last, "session.execution.succeeded.1");
    child.stop_after_witness(&root);
    drop(child);

    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        PROMOTED + 1,
        "not exactly one summary call and one rebuilt attempt"
    );
    let bodies = locked(&provider.bodies).clone();
    assert!(bodies[PROMOTED - 1].contains(SUMMARY_REQUEST));
    assert!(!bodies[PROMOTED].contains(SUMMARY_REQUEST));
    let ended = events(&root, "session.compaction.ended.2");
    assert_eq!(ended.len(), 1, "more than one compaction ran: {ended:?}");
    assert_eq!(ended[0]["trigger"], "userRequested");
    assert_eq!(pending(&root), 0);
    assert!(events(&root, "session.compaction.failed.1").is_empty());
    process_fixture::cleanup(&root);
}

/// Review of 07fed7d6: an unreadable store is never absence. A store
/// without history fails the read instead of reporting "no terminal".
#[test]
#[should_panic(expected = "execution terminal read failed")]
fn an_unreadable_store_is_a_failed_read_not_an_absent_terminal() {
    let root = root("manual-compact-unreadable");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    let _fresh = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap_or_else(|error| panic!("root failed: {error}"));
    rusqlite::Connection::open(root.join("database.sqlite3"))
        .and_then(|db| db.execute_batch("CREATE TABLE unrelated (id INTEGER)"))
        .unwrap_or_else(|error| panic!("empty store failed: {error}"));
    carrying_terminal(&root, "msg_unreadable", Duration::from_millis(150));
}

/// `session.compact` (slice 3): one barrier per Session, listed in the
/// pending work, and the frozen refusals.
#[test]
fn the_compact_route_admits_one_barrier_and_lists_it() {
    let _journey = process_journey_custody();
    let root = root("manual-compact-route");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![Script::Success]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    prompt(address, &session, "msg_route_user", "hello");
    permission_residuals::wait_session_idle(address, &session);
    let path = format!("/api/session/{session}/compact");

    // An id another input already owns is a conflict.
    let (status, _) = request(address, "POST", &path, r#"{"id":"msg_route_user"}"#);
    assert_eq!(status, 409);
    let admitted = json(address, "POST", &path, "{}");
    assert_eq!(admitted["data"]["type"], "compaction");
    assert_eq!(admitted["data"]["sessionID"], session.as_str());
    let id = admitted["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no compaction id: {admitted}"))
        .to_owned();
    // A second request answers with the pending barrier.
    let again = json(address, "POST", &path, r#"{"id":"msg_route_second"}"#);
    assert_eq!(again["data"]["id"], id.as_str());
    let pending = json(
        address,
        "GET",
        &format!("/api/session/{session}/pending"),
        "",
    );
    assert_eq!(pending["data"][0]["type"], "compaction", "{pending}");
    assert_eq!(pending["data"][0]["id"], id.as_str());
    let (status, _) = request(
        address,
        "POST",
        "/api/session/ses_00000000000000000000000000000000x0/compact",
        "{}",
    );
    assert_eq!(status, 404);
    child.stop_after_witness(&root);
    drop(child);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(events(&root, "session.compaction.admitted.1").len(), 1);
    process_fixture::cleanup(&root);
}

/// Review HOLD on 9abec016, finding 2, through the real drain: a request
/// admitted while a tool chain runs does not compact that chain; it runs
/// exactly once, before the next input.
#[test]
fn a_compaction_requested_mid_chain_waits_for_the_next_input() {
    let _journey = process_journey_custody();
    let root = root("manual-compact-mid-chain");
    prepare(&root, 120_000);
    attended(&root);
    fs::write(root.join("seed.txt"), vec![b's'; 7_524])
        .unwrap_or_else(|error| panic!("seed fixture write failed: {error}"));
    let provider = ProviderFixture::start(vec![
        // The Author preset asks before replacing a file, so the chain
        // is still running, at its tool Step, when the compaction arrives.
        write_call("call_mid_chain", "seed.txt", "replaced\n"),
        Script::SuccessText("the chain finished"),
        Script::SuccessText("## Objective\n- the seed was read"),
        Script::SuccessText("answered after compact"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    prompt(address, &session, "msg_mid_chain", "read the seed");
    // The chain is running: its tool Step waits on permission.
    let permissions = format!("/api/session/{session}/permission");
    let waiting = permission_residuals::wait_until(
        Duration::from_secs(60),
        Duration::from_millis(50),
        || {
            let rows = json(address, "GET", &permissions, "");
            rows["data"].as_array().is_some_and(|rows| !rows.is_empty())
        },
    )
    .is_some();
    assert!(waiting, "the tool Step never asked");
    json(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    compaction_continuity::approve(address, &session, "call_mid_chain");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert!(
        !locked(&provider.bodies)
            .iter()
            .any(|body| body.contains(SUMMARY_REQUEST)),
        "a request admitted mid-chain compacted that chain"
    );

    prompt(address, &session, "msg_mid_chain_next", "continue");
    permission_residuals::wait_session_idle(address, &session);
    assert_carried(&root, "msg_mid_chain", "session.execution.succeeded.1");
    assert_carried(&root, "msg_mid_chain_next", "session.execution.succeeded.1");
    child.stop_after_witness(&root);
    drop(child);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let bodies = locked(&provider.bodies).clone();
    assert!(bodies[2].contains(SUMMARY_REQUEST) && !bodies[3].contains(SUMMARY_REQUEST));
    let ended = events(&root, "session.compaction.ended.2");
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0]["trigger"], "userRequested");
    assert_eq!(pending(&root), 0);
    process_fixture::cleanup(&root);
}

/// Review of 71949676: only an unknown Session is 404. A Session
/// whose seed is corrupt exists, so its request is an explicit refusal, and
/// no barrier is admitted.
#[test]
fn a_corrupt_session_seed_is_refused_not_reported_missing() {
    let _journey = process_journey_custody();
    let root = root("manual-compact-corrupt-seed");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    child.stop_after_witness(&root);
    drop(child);
    rusqlite::Connection::open(root.join("database.sqlite3"))
        .and_then(|db| {
            db.execute(
                "UPDATE event SET data = json_remove(data, '$.info.directory')
                 WHERE type = 'session.created.1'",
                [],
            )
        })
        .unwrap_or_else(|error| panic!("seed corruption failed: {error}"));
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    child.stop_after_witness(&root);
    drop(child);
    let body = String::from_utf8_lossy(&body);
    assert_eq!(status, 400, "a corrupt Session was not refused: {body}");
    assert!(!body.contains("SessionNotFoundError"), "{body}");
    assert!(events(&root, "session.compaction.admitted.1").is_empty());
    assert_eq!(pending(&root), 0);
    process_fixture::cleanup(&root);
}

/// A WAL writer does not block Session reads. Its competing write still
/// refuses compaction admission as a conflict, never as a missing Session.
#[test]
fn a_compaction_write_conflict_is_refused_not_reported_missing() {
    let _journey = process_journey_custody();
    let root = root("manual-compact-store-read");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    let database = root.join("database.sqlite3");
    // The worker reports readiness, or the reason it could not take the
    // lock, so a setup failure fails this test instead of parking it.
    let (ready, locked) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
    let (release, released) = std::sync::mpsc::sync_channel::<()>(1);
    let locker = {
        let database = database.clone();
        thread::spawn(move || {
            let held = rusqlite::Connection::open(&database).and_then(|db| {
                db.execute_batch("BEGIN EXCLUSIVE")?;
                Ok(db)
            });
            let db = match held {
                Ok(db) => db,
                Err(error) => {
                    let _sent = ready.send(Err(format!("exclusive lock failed: {error}")));
                    return;
                }
            };
            if ready.send(Ok(())).is_err() {
                return;
            }
            // Hold through the real refusal, then release promptly. Disconnect
            // also releases on unwinding; a lost signal retains the old bound.
            match released.recv_timeout(Duration::from_secs(14)) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    panic!("store lock was not released after the refusal");
                }
            }
            let _released = db.execute_batch("ROLLBACK");
        })
    };
    match locked.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => panic!("the store lock was not taken: {reason}"),
        Err(error) => panic!("the store lock worker never reported: {error}"),
    }
    // Longer than the service's 10 s busy wait, within the lock worker's bound.
    let (status, body) = request_with_read_timeout(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
        Duration::from_secs(13),
    );
    release
        .send(())
        .unwrap_or_else(|error| panic!("store lock release failed: {error}"));
    locker
        .join()
        .unwrap_or_else(|_| panic!("lock thread panicked"));
    child.stop_after_witness(&root);
    drop(child);
    let body = String::from_utf8_lossy(&body);
    assert_eq!(status, 409, "a competing writer was not refused: {body}");
    assert!(
        body.contains("ConflictError") && body.contains("database is locked"),
        "{body}"
    );
    assert!(!body.contains("SessionNotFoundError"), "{body}");
    assert!(events(&root, "session.compaction.admitted.1").is_empty());
    process_fixture::cleanup(&root);
}

/// Slice 4: an ACP client's `/compact` admits the request through the route
/// and ends its turn with the notice; the request then runs once, before the
/// client's next message.
#[cfg(unix)]
#[test]
fn an_acp_compact_command_runs_before_the_next_message() {
    use super::acp_process::{now, AcpObserved, AcpProcess, ACP_FRAMES};
    let _journey = process_journey_custody();
    let root = root("manual-compact-acp");
    prepare(&root, 120_000);
    fs::write(root.join("seed.txt"), vec![b's'; 7_524])
        .unwrap_or_else(|error| panic!("seed fixture write failed: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_acp_seed",
            path: "seed.txt",
        },
        Script::SuccessText("seeded"),
        Script::SuccessText("## Objective\n- the seed was read"),
        Script::SuccessText("answered after compact"),
    ]);
    let deadline = now() + Duration::from_secs(60);
    let mut acp = AcpProcess::start(&root, provider.address, ACP_FRAMES);
    let mut seeded = AcpObserved::default();
    let mut compacted = AcpObserved::default();
    let mut next = AcpObserved::default();
    let outcome = (|| {
        acp.request(
            1,
            "initialize",
            serde_json::json!({"protocolVersion": 1, "clientCapabilities": {}}),
            deadline,
            &mut seeded,
        )?;
        let created = acp.request(
            2,
            "session/new",
            serde_json::json!({"cwd": root, "mcpServers": []}),
            deadline,
            &mut seeded,
        )?;
        let session = created["sessionId"]
            .as_str()
            .ok_or("no sessionId")?
            .to_owned();
        let turn = |text: &str| serde_json::json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]});
        acp.request(
            3,
            "session/prompt",
            turn("read the seed"),
            deadline,
            &mut seeded,
        )?;
        let calls_before = provider.calls.load(Ordering::SeqCst);
        let compact = acp.request(
            4,
            "session/prompt",
            turn("/compact Preserve the seed decision"),
            deadline,
            &mut compacted,
        )?;
        let calls_at_compact = provider.calls.load(Ordering::SeqCst);
        let answer = acp.request(5, "session/prompt", turn("continue"), deadline, &mut next)?;
        Ok::<_, String>((compact, answer, calls_before, calls_at_compact))
    })();
    acp.cleanup();
    let cleaned_up = acp.cleaned_up;
    drop(acp);
    let (compact, answer, calls_before, calls_at_compact) =
        outcome.unwrap_or_else(|error| panic!("ACP turn failed: {error}"));
    assert!(
        cleaned_up,
        "the ACP CLI or its product outlived the journey"
    );
    assert_eq!(compact["stopReason"], "end_turn", "{compact}");
    assert_eq!(
        compacted.updates,
        vec!["agent_message_chunk".to_owned()],
        "/compact answered with something other than its notice"
    );
    assert_eq!(calls_before, 2);
    assert_eq!(calls_at_compact, 2, "/compact ran a provider call");
    assert_eq!(answer["stopReason"], "end_turn", "{answer}");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let bodies = locked(&provider.bodies).clone();
    assert!(bodies[2].contains(SUMMARY_REQUEST) && !bodies[3].contains(SUMMARY_REQUEST));
    assert_eq!(events(&root, "session.compaction.admitted.1").len(), 1);
    assert!(bodies[2].contains("Preserve the seed decision"));
    assert!(!bodies[3].contains("Preserve the seed decision"));
    let ended = events(&root, "session.compaction.ended.2");
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0]["trigger"], "userRequested");
    assert_eq!(pending(&root), 0);
    process_fixture::cleanup(&root);
}

/// Repository command and skill names cannot replace the ACP built-in:
/// its local dispatch requests compaction without submitting model input.
#[cfg(unix)]
fn configured_compact_cannot_shadow(label: &str, name: &str, text: &str) {
    use super::acp_process::{now, AcpObserved, AcpProcess, ACP_FRAMES};
    let _journey = process_journey_custody();
    let root = root(label);
    prepare(&root, 120_000);
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap_or(&root))
        .unwrap_or_else(|error| panic!("configured {name} directory failed: {error}"));
    fs::write(&path, text)
        .unwrap_or_else(|error| panic!("configured {name} write failed: {error}"));
    let provider = ProviderFixture::start(vec![Script::SuccessText("ran as a prompt")]);
    let deadline = now() + Duration::from_secs(60);
    let mut acp = AcpProcess::start(&root, provider.address, ACP_FRAMES);
    let mut observed = AcpObserved::default();
    let outcome = (|| {
        acp.request(
            1,
            "initialize",
            serde_json::json!({"protocolVersion": 1, "clientCapabilities": {}}),
            deadline,
            &mut observed,
        )?;
        let created = acp.request(
            2,
            "session/new",
            serde_json::json!({"cwd": root, "mcpServers": []}),
            deadline,
            &mut observed,
        )?;
        let session = created["sessionId"]
            .as_str()
            .ok_or("no sessionId")?
            .to_owned();
        acp.request(
            3,
            "session/prompt",
            serde_json::json!({"sessionId": session, "prompt": [{"type": "text", "text": "/compact"}]}),
            deadline,
            &mut observed,
        )
    })();
    acp.cleanup();
    drop(acp);
    let answer = outcome.unwrap_or_else(|error| panic!("ACP turn failed: {error}"));
    assert_eq!(answer["stopReason"], "end_turn", "{answer}");
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "configured {name} replaced built-in dispatch and reached the model"
    );
    assert_eq!(events(&root, "session.compaction.admitted.1").len(), 1);
    let advertised = observed
        .update_values
        .iter()
        .find_map(|row| {
            row.get("availableCommands")
                .and_then(serde_json::Value::as_array)
        })
        .unwrap_or_else(|| panic!("ACP command catalog absent"));
    let compact = advertised
        .iter()
        .filter(|row| row["name"] == "compact")
        .collect::<Vec<_>>();
    assert_eq!(
        compact.len(),
        1,
        "colliding compact was advertised beside the built-in"
    );
    if name.ends_with("SKILL.md") {
        assert!(compact[0]["description"]
            .as_str()
            .is_some_and(|text| text.contains("unavailable")));
    } else {
        assert!(advertised
            .iter()
            .any(|row| row["name"] == "project:compact"));
    }
    process_fixture::cleanup(&root);
}

#[cfg(unix)]
#[test]
fn a_configured_compact_command_cannot_shadow_the_acp_builtin() {
    configured_compact_cannot_shadow(
        "manual-compact-acp-command",
        ".grapt/command/compact.md",
        "---\ndescription: the project's own compact\n---\nsummarize it our way\n",
    );
}

#[cfg(unix)]
#[test]
fn a_configured_compact_skill_cannot_shadow_the_acp_builtin() {
    configured_compact_cannot_shadow(
        "manual-compact-acp-skill",
        ".agents/skills/compact/SKILL.md",
        "---\nname: compact\ndescription: the project's own compact skill\n---\nsummarize it our way\n",
    );
}

#[path = "manual_compaction/hook_events.rs"]
mod hook_events;
