// Scenario ordering.
// Modified for Grapt's durable queue, cancellation and shipped CLI reconnect.
//! Queue behind a real compaction in a history longer than one public page.
use super::*;

const SEEDS: usize = 32;
const ACTIVE: &str = "msg_rel19_active";
const QUEUED: &str = "msg_rel19_queued";
const MARKER: &str = "REL19_QUEUE_SURVIVES_EXACTLY_ONCE";
const ANSWER: &str = "REL19_QUEUED_ANSWER";

#[derive(Clone, Copy)]
enum Cut {
    Complete,
    Cancel,
    ProcessLoss,
}

fn read_store(root: &Path) -> Result<rusqlite::Connection, rusqlite::Error> {
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    db.busy_timeout(Duration::from_secs(2))?;
    Ok(db)
}

fn events(root: &Path, session: &str, kind: &str) -> Result<i64, rusqlite::Error> {
    read_store(root)?.query_row(
        "SELECT count(*) FROM event WHERE aggregate_family='session'
         AND aggregate_id=?1 AND type=?2",
        (session, kind),
        |row| row.get(0),
    )
}

fn input_events(root: &Path, session: &str, kind: &str) -> Result<i64, rusqlite::Error> {
    read_store(root)?.query_row(
        "SELECT count(*) FROM event WHERE aggregate_family='session'
         AND aggregate_id=?1 AND type=?2 AND json_extract(data,'$.inputID')=?3",
        (session, kind, QUEUED),
        |row| row.get(0),
    )
}

fn pending(root: &Path, session: &str) -> Result<i64, rusqlite::Error> {
    read_store(root)?.query_row(
        "SELECT count(*) FROM session_pending WHERE session_id=?1 AND id=?2",
        (session, QUEUED),
        |row| row.get(0),
    )
}

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) -> serde_json::Value {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
    )
}

/// Reuse the captured-child owner, with a finite wait and bounded output.
fn reopen(root: &Path, session: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut command = Command::new(cli_continuity::cli_binary());
    command
        .current_dir(root)
        .args(["attach", "--service"])
        .arg(root)
        .args(["--resume", "--jsonl"])
        .stdin(Stdio::null())
        .env("GRAPT_TEST_HOME", root.join("home"))
        .env("GRAPT_CONFIG_DIR", root.join("global-config"));
    let mut client = process_fixture::spawn_captured(root, &mut command);
    let mut exited = false;
    let wait = PollDeadline::within(Duration::from_secs(12));
    while wait.pending() {
        if client.try_wait()?.is_some() {
            exited = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(exited, "CLI reconnect exceeded twelve seconds");
    let output = client.wait_with_output()?;
    assert!(
        output.status.success(),
        "CLI reconnect: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = std::str::from_utf8(&output.stdout)?;
    let receipt: serde_json::Value =
        serde_json::from_str(text.lines().last().ok_or("CLI receipt absent")?)?;
    assert_eq!(receipt["session"], session);
    Ok(())
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn journey(cut: Cut, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root(label);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 200_000);
    set_compaction(&root, serde_json::json!({"auto":false}));
    let entered = Arc::new(terminal_coordination::Phase::new());
    let release = Arc::new(terminal_coordination::Phase::new());
    let disconnect = Arc::new(AtomicBool::new(false));
    let mut scripts = vec![Script::Output(1600); SEEDS];
    scripts.push(Script::ControlledTerminalOutput {
        bytes: 1000,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        disconnect: Arc::clone(&disconnect),
    });
    if matches!(cut, Cut::Complete) {
        scripts.push(Script::SuccessText("REL19_ACTIVE_ANSWER"));
    } else if matches!(cut, Cut::ProcessLoss) {
        // Process loss leaves the manual request pending. Its next carrying
        // input reruns the summary before receiving its own answer.
        scripts.push(Script::SuccessText(OBJECTIVE));
    }
    scripts.push(Script::SuccessText(ANSWER));
    let provider = ProviderFixture::start(scripts);
    let mut service = spawn(&root, provider.address);
    let mut address = await_ready(&root, &mut service);
    let session = create(address);
    for index in 0..SEEDS {
        let id = format!("msg_rel19_seed_{index}");
        prompt(address, &session, &id, &format!("complete seed {index}"));
        manual_compaction::assert_carried(&root, &id, "session.execution.succeeded.1");
        permission_residuals::wait_session_idle(address, &session);
    }
    let history_path = format!("/api/session/{session}/message");
    let history = json(address, "GET", &history_path, "");
    assert_eq!(
        history["data"]
            .as_array()
            .ok_or("history page absent")?
            .len(),
        50
    );
    assert_eq!(
        events(&root, &session, "session.assistant-message.opened.2")?,
        SEEDS as i64
    );
    json(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    prompt(
        address,
        &session,
        ACTIVE,
        "compact this long session, then finish",
    );
    entered.wait("queued-input summary arrival", &provider.stop)?;
    assert!(locked(&provider.bodies)[SEEDS].contains(SUMMARY));

    let first = prompt(address, &session, QUEUED, MARKER);
    let retry = prompt(address, &session, QUEUED, MARKER);
    let admitted = first["data"]["admittedSeq"]
        .as_u64()
        .ok_or("queued admission sequence absent")?;
    assert_eq!(retry["data"]["admittedSeq"].as_u64(), Some(admitted));
    assert_eq!(pending(&root, &session)?, 1);
    assert_eq!(
        input_events(&root, &session, "session.input.admitted.1")?,
        1
    );
    assert_eq!(
        input_events(&root, &session, "session.input.promoted.1")?,
        0
    );
    reopen(&root, &session)?;
    assert_eq!(provider.calls.load(Ordering::SeqCst), SEEDS + 1);

    match cut {
        Cut::Complete => release.release(),
        Cut::Cancel => {
            let interrupted = request(
                address,
                "POST",
                &format!("/api/session/{session}/interrupt"),
                "{}",
            );
            disconnect.store(true, Ordering::SeqCst);
            release.release();
            assert_eq!(interrupted.0, 204);
            manual_compaction::assert_carried(&root, ACTIVE, "session.execution.interrupted.1");
            permission_residuals::wait_session_idle(address, &session);
        }
        Cut::ProcessLoss => {
            service.stop_after_witness(&root);
            disconnect.store(true, Ordering::SeqCst);
            release.release();
            // Model the service manager's stale-registration cleanup, as the
            // existing process_loss and manual_compaction journeys do.
            for stale in [
                "registration",
                "registration.elected",
                "registration.lock",
                "serve-ready.json",
            ] {
                let path = root.join(stale);
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
            service = spawn(&root, provider.address);
            address = await_ready(&root, &mut service);
            assert_eq!(
                events(&root, &session, "session.step.failed.2")?,
                1,
                "startup did not settle the interrupted physical attempt"
            );
            assert!(
                json(address, "GET", "/api/session/active", "")["data"][&session].is_null(),
                "restart resurrected a dead execution as active"
            );
        }
    }
    if !matches!(cut, Cut::Complete) {
        // Interrupt stops the drain; reconnect alone must not execute pending
        // work. Retrying its stable identity explicitly resumes that same input.
        assert_eq!(pending(&root, &session)?, 1);
        assert_eq!(
            input_events(&root, &session, "session.input.promoted.1")?,
            0
        );
        reopen(&root, &session)?;
        let resumed = prompt(address, &session, QUEUED, MARKER);
        assert_eq!(resumed["data"]["admittedSeq"].as_u64(), Some(admitted));
    }
    // A live drain owns one execution across queued inputs. A stable retry
    // after cancellation or restart opens a new drain of its own.
    let carrying = if matches!(cut, Cut::Complete) {
        ACTIVE
    } else {
        QUEUED
    };
    manual_compaction::assert_carried(&root, carrying, "session.execution.succeeded.1");
    permission_residuals::wait_session_idle(address, &session);
    let expected_calls = SEEDS + 2 + usize::from(!matches!(cut, Cut::Cancel));
    let expected_executions = SEEDS + 1 + usize::from(!matches!(cut, Cut::Complete));
    assert_eq!(provider.calls.load(Ordering::SeqCst), expected_calls);
    assert_eq!(pending(&root, &session)?, 0);
    assert_eq!(
        input_events(&root, &session, "session.input.admitted.1")?,
        1
    );
    assert_eq!(
        input_events(&root, &session, "session.input.promoted.1")?,
        1
    );
    assert_eq!(
        events(&root, &session, "session.execution.started.1")?,
        expected_executions as i64
    );
    assert_eq!(
        events(&root, &session, "session.compaction.ended.2")?,
        i64::from(!matches!(cut, Cut::Cancel))
    );
    let bodies = locked(&provider.bodies);
    // A resumed manual summary includes its carrying input. Count coding
    // requests separately so summary context cannot masquerade as execution.
    assert_eq!(
        bodies
            .iter()
            .filter(|body| body.contains(MARKER) && !body.contains(SUMMARY))
            .count(),
        1
    );
    assert_eq!(
        bodies
            .iter()
            .filter(|body| body.contains(MARKER) && body.contains(SUMMARY))
            .count(),
        usize::from(matches!(cut, Cut::ProcessLoss))
    );
    assert_eq!(
        bodies
            .last()
            .ok_or("queued provider request absent")?
            .matches(MARKER)
            .count(),
        1
    );
    drop(bodies);
    let before = json(address, "GET", &history_path, "");
    assert!(before.to_string().contains(ANSWER));
    reopen(&root, &session)?;
    prompt(address, &session, QUEUED, MARKER);
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        expected_calls,
        "reconnect or completed-input retry executed twice"
    );
    assert_eq!(
        events(&root, &session, "session.execution.started.1")?,
        expected_executions as i64,
        "completed-input retry opened another execution"
    );
    let terminals = events(&root, &session, "session.execution.succeeded.1")?
        + events(&root, &session, "session.execution.interrupted.1")?
        + events(&root, &session, "session.execution.failed.1")?;
    assert_eq!(
        terminals,
        (expected_executions - usize::from(matches!(cut, Cut::ProcessLoss))) as i64,
        "a live drain was lost or settled twice; process loss settles its Step on startup"
    );
    assert_eq!(json(address, "GET", &history_path, ""), before);
    service.stop_after_witness(&root);
    Ok(())
}

#[test]
fn queued_input_runs_once_after_compaction_and_client_reconnect(
) -> Result<(), Box<dyn std::error::Error>> {
    journey(Cut::Complete, "rel19-compact-queue")
}

#[test]
fn cancelled_compaction_preserves_queued_input_for_a_stable_retry(
) -> Result<(), Box<dyn std::error::Error>> {
    journey(Cut::Cancel, "rel19-cancel-queue")
}

#[test]
fn process_loss_during_compaction_preserves_queued_input_after_reconnect(
) -> Result<(), Box<dyn std::error::Error>> {
    journey(Cut::ProcessLoss, "rel19-restart-queue")
}
