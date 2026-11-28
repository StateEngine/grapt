use super::store::{
    has_pending_approval_or_form, has_unresolved_or_quarantined_attempt, interrupted_turn,
    load_row, synthetic_message_id, upsert_row, ResumeState,
};
use super::*;
use grapt_store::{AggregateId, AppendInput, Appended, OsRandomIds, SystemClock};
use grapt_types::{DeterministicAuthorities, EventId, EventVersion, ExecutionId};

fn test_store() -> Result<(EventStore, OsRandomIds, SystemClock), String> {
    let mut store = EventStore::open_in_memory().map_err(|error| error.to_string())?;
    grapt_inbox::Inbox::ensure_tables(store.connection()).map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    ensure_schema_within(&tx)?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    let ids = OsRandomIds::seed().map_err(|error| error.to_string())?;
    Ok((store, ids, SystemClock))
}

fn append_event(
    store: &mut EventStore,
    ids: &OsRandomIds,
    clock: &SystemClock,
    session: &SessionId,
    kind_name: &str,
    version_num: u32,
    data: Value,
) -> Result<u64, String> {
    let aggregate = AggregateId::Session(session.clone());
    let writer = match store.fencing_token(&aggregate) {
        Ok(token) => token,
        Err(_) => {
            store
                .elect_writer(&aggregate)
                .map_err(|error| error.to_string())?;
            store
                .fencing_token(&aggregate)
                .map_err(|error| error.to_string())?
        }
    };
    let id: EventId = format!("evt_{}", ids.mint().as_str())
        .parse()
        .map_err(|_| "event id".to_owned())?;
    let version = EventVersion::new(version_num).ok_or("event version")?;
    let (appended, notices) = store
        .append(
            AppendInput {
                id: &id,
                aggregate,
                writer,
                kind_name,
                version,
                created_ms: clock.now().as_millis(),
                data: &data,
            },
            &mut [],
        )
        .map_err(|error| error.to_string())?;
    notices.discard();
    let seq = match appended {
        Appended::Committed { seq } | Appended::Idempotent { seq } => seq,
    };
    Ok(seq)
}

fn seed_active_turn(
    store: &mut EventStore,
    ids: &OsRandomIds,
    clock: &SystemClock,
    session: &SessionId,
    parent: Option<&str>,
    exec_name: &str,
) -> Result<u64, String> {
    let authorities = DeterministicAuthorities::new(clock, ids);
    let directory = std::env::temp_dir();
    let directory = directory.to_str().ok_or("temp dir not utf8")?;
    let project = grapt_types::ProjectId::new("prj_auto_resume").ok_or("project")?;
    let mut seed = grapt_session::SessionSeed::new(project, directory, "auto-resume")
        .map_err(|error| error.to_string())?
        .with_requested_id(session.clone())
        .map_err(|error| error.to_string())?;
    if let Some(parent) = parent {
        let parent_id: SessionId = parent.parse().map_err(|error| format!("{error}"))?;
        seed = seed.with_parent(parent_id);
    }
    grapt_session::create_session(store, &authorities, &seed, &mut [])
        .map_err(|error| error.to_string())?;
    let user_seq = append_event(
        store,
        ids,
        clock,
        session,
        "session.input.admitted",
        1,
        json!({"inputID": "msg_u1", "input": {"type": "user", "data": {"text": "do the work"}}}),
    )?;
    let _ = append_event(
        store,
        ids,
        clock,
        session,
        "session.input.promoted",
        1,
        json!({"sessionID": session.as_str(), "inputID": "msg_u1"}),
    )?;
    let _ = append_event(
        store,
        ids,
        clock,
        session,
        "session.execution.started",
        1,
        json!({"sessionID": session.as_str(), "executionID": exec_name}),
    )?;
    let execution = ExecutionId::new(exec_name).ok_or("exec id")?;
    grapt_service_state::record(
        store,
        &authorities,
        session,
        &execution,
        State::plain(Kind::Running),
        Change::Start,
    )?;
    Ok(user_seq)
}

#[test]
fn eligible_interrupted_turn_enqueues_and_respects_crash_loop_cap() -> Result<(), String> {
    let (mut store, ids, clock) = test_store()?;
    let empty_forms = HashSet::new();
    let session: SessionId = "ses_auto_resume_cap".parse().map_err(|e| format!("{e}"))?;
    let user_seq = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_1")?;

    enqueue_if_eligible(&mut store, &session, &empty_forms, &clock, &ids)?;
    let row = load_row(store.connection(), &session)?.ok_or("missing pending_resume row")?;
    assert_eq!(row.state, ResumeState::Pending);
    assert_eq!(row.turn_origin_seq, user_seq);
    assert_eq!(row.attempts, 0);

    upsert_row(
        &mut store,
        &session,
        user_seq,
        "exec_2",
        MAX_AUTO_RESUME_ATTEMPTS,
        ResumeState::Running,
        None,
        clock.now().as_millis(),
    )?;
    enqueue_if_eligible(&mut store, &session, &empty_forms, &clock, &ids)?;
    let capped = load_row(store.connection(), &session)?.ok_or("missing capped row")?;
    assert_eq!(capped.state, ResumeState::Stopped);
    assert_eq!(
        capped.reason.as_deref(),
        Some("max_auto_resume_attempts_exceeded")
    );
    let status = grapt_service_state::read(store.connection(), &session)?;
    assert_eq!(status.state.kind, Kind::Disconnected);
    assert!(status
        .state
        .message
        .as_deref()
        .is_some_and(|msg| msg.contains("Automatic resume stopped")));

    // A brand new human prompt + execution starts a fresh turn_origin_seq with 0 attempts.
    let next_seq = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.input.admitted",
        1,
        json!({"inputID": "msg_u2", "input": {"type": "user", "data": {"text": "try again"}}}),
    )?;
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.input.promoted",
        1,
        json!({"sessionID": session.as_str(), "inputID": "msg_u2"}),
    )?;
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.execution.started",
        1,
        json!({"sessionID": session.as_str(), "executionID": "exec_3"}),
    )?;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let exec_3 = ExecutionId::new("exec_3").ok_or("exec_3")?;
    grapt_service_state::record(
        &mut store,
        &authorities,
        &session,
        &exec_3,
        State::plain(Kind::Running),
        Change::Start,
    )?;
    enqueue_if_eligible(&mut store, &session, &empty_forms, &clock, &ids)?;
    let fresh = load_row(store.connection(), &session)?.ok_or("missing fresh row")?;
    assert_eq!(fresh.state, ResumeState::Pending);
    assert_eq!(fresh.turn_origin_seq, next_seq);
    assert_eq!(fresh.attempts, 0);
    Ok(())
}

#[test]
fn quarantined_attempt_pre_recovery_ask_and_subagent_child_do_not_enqueue() -> Result<(), String> {
    let (mut store, ids, clock) = test_store()?;
    let empty_forms = HashSet::new();

    // 1. Quarantined provider attempt (no ProcessLoss event emitted).
    let s_quar: SessionId = "ses_auto_quar".parse().map_err(|e| format!("{e}"))?;
    seed_active_turn(&mut store, &ids, &clock, &s_quar, None, "exec_q")?;
    let aggregate = AggregateId::Session(s_quar.clone());
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let ceiling = grapt_store::attempt_receipt::AttemptRecordCeiling::new(
        std::num::NonZeroU32::new(4096).ok_or("ceiling")?,
    );
    let max_id = std::num::NonZeroU32::new(1024).ok_or("max_id")?;
    let attempt_id = grapt_types::PhysicalAttemptId::new("att_auto_quar").ok_or("attempt id")?;
    let bounded_attempt = grapt_types::BoundedAttemptId::new(attempt_id.clone(), max_id)
        .map_err(|e| format!("{e:?}"))?;
    let bounded_session =
        grapt_types::BoundedSessionId::new(s_quar.clone(), max_id).map_err(|e| format!("{e:?}"))?;
    let step = grapt_types::StepNumber::new(1).ok_or("step")?;
    let ordinal = grapt_types::AttemptOrdinal::new(1).ok_or("ordinal")?;
    let receipt = grapt_types::DurableAttemptReceipt::reserved(
        bounded_attempt,
        bounded_session,
        step,
        ordinal,
        grapt_types::ReservedMarker::new(clock.now()),
    );
    let terminal_event: EventId = format!("evt_{}", ids.mint().as_str())
        .parse()
        .map_err(|_| "terminal event id".to_owned())?;
    let terminal = grapt_store::attempt_receipt::TerminalCoordinates::new(
        terminal_event,
        clock.now().as_millis(),
    );
    store
        .reserve_attempt_receipt(&grapt_store::attempt_receipt::ReserveInput {
            receipt: &receipt,
            writer,
            terminal: &terminal,
            ceiling,
        })
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    EventStore::quarantine_attempt_receipt_within(
        &tx,
        &attempt_id,
        writer,
        grapt_store::attempt_receipt::QuarantineReason::Undecodable,
        clock.now(),
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    assert!(has_unresolved_or_quarantined_attempt(
        store.connection(),
        &s_quar
    )?);
    enqueue_if_eligible(&mut store, &s_quar, &empty_forms, &clock, &ids)?;
    let row = load_row(store.connection(), &s_quar)?.ok_or("missing quar row")?;
    assert_eq!(row.state, ResumeState::Stopped);
    assert_eq!(
        row.reason.as_deref(),
        Some("unresolved_or_quarantined_attempt")
    );

    // 2. Pre-recovery interactive ask/form session (even after form_request is cancelled).
    let s_ask: SessionId = "ses_auto_ask".parse().map_err(|e| format!("{e}"))?;
    seed_active_turn(&mut store, &ids, &clock, &s_ask, None, "exec_ask")?;
    let mut pre_forms = HashSet::new();
    pre_forms.insert(s_ask.clone());
    enqueue_if_eligible(&mut store, &s_ask, &pre_forms, &clock, &ids)?;
    let row = load_row(store.connection(), &s_ask)?.ok_or("missing ask row")?;
    assert_eq!(row.state, ResumeState::Stopped);
    assert_eq!(row.reason.as_deref(), Some("pending_approval_or_form"));

    // 3. Parent session with an active/lost subagent child.
    let s_parent: SessionId = "ses_auto_parent".parse().map_err(|e| format!("{e}"))?;
    seed_active_turn(&mut store, &ids, &clock, &s_parent, None, "exec_p")?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS subagent_operation (operation_id TEXT PRIMARY KEY, parent_session_id TEXT NOT NULL, status TEXT NOT NULL);
         INSERT INTO subagent_operation (operation_id, parent_session_id, status) VALUES ('op_1', 'ses_auto_parent', 'running');",
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    enqueue_if_eligible(&mut store, &s_parent, &empty_forms, &clock, &ids)?;
    let row = load_row(store.connection(), &s_parent)?.ok_or("missing parent row")?;
    assert_eq!(row.state, ResumeState::Stopped);
    assert_eq!(row.reason.as_deref(), Some("pending_subagent_child"));
    Ok(())
}

#[test]
fn queued_prompt_and_seat_startup_do_not_shift_or_satisfy_turn_origin() -> Result<(), String> {
    let (mut store, ids, clock) = test_store()?;
    let empty_forms = HashSet::new();

    // Seat-startup input alone must NOT satisfy the human prompt requirement.
    let s_seat: SessionId = "ses_auto_seat".parse().map_err(|e| format!("{e}"))?;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let directory = std::env::temp_dir();
    let directory = directory.to_str().ok_or("temp dir not utf8")?;
    let project = grapt_types::ProjectId::new("prj_auto_resume").ok_or("project")?;
    let seed = grapt_session::SessionSeed::new(project, directory, "seat")
        .map_err(|error| error.to_string())?
        .with_requested_id(s_seat.clone())
        .map_err(|error| error.to_string())?;
    grapt_session::create_session(&mut store, &authorities, &seed, &mut [])
        .map_err(|error| error.to_string())?;
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &s_seat,
        "session.input.admitted",
        1,
        json!({
            "inputID": "msg_seat_boot",
            "input": {"type": "user", "data": {"text": "boot", "metadata": {"origin": "seat-startup"}}}
        }),
    )?;
    let exec_seat = ExecutionId::new("exec_seat").ok_or("exec_seat")?;
    grapt_service_state::record(
        &mut store,
        &authorities,
        &s_seat,
        &exec_seat,
        State::plain(Kind::Running),
        Change::Start,
    )?;
    enqueue_if_eligible(&mut store, &s_seat, &empty_forms, &clock, &ids)?;
    assert!(load_row(store.connection(), &s_seat)?.is_none());

    // A queued user prompt admitted AFTER exec_1 started must not shift exec_1's origin.
    let s_queued: SessionId = "ses_auto_queued".parse().map_err(|e| format!("{e}"))?;
    let origin_seq = seed_active_turn(&mut store, &ids, &clock, &s_queued, None, "exec_q1")?;
    let _queued_seq = append_event(
        &mut store,
        &ids,
        &clock,
        &s_queued,
        "session.input.admitted",
        1,
        json!({"inputID": "msg_queued", "input": {"type": "user", "data": {"text": "later"}}}),
    )?;
    let exec_q1 = ExecutionId::new("exec_q1").ok_or("exec_q1")?;
    assert_eq!(
        interrupted_turn(store.connection(), &s_queued, &exec_q1)?.map(|t| t.origin_seq),
        Some(origin_seq)
    );
    Ok(())
}

#[test]
fn synthetic_message_ids_never_collide_on_shared_session_prefix() -> Result<(), String> {
    let s1: SessionId = "ses_shared_prefix_0000000000000000_alpha"
        .parse()
        .map_err(|e| format!("{e}"))?;
    let s2: SessionId = "ses_shared_prefix_0000000000000000_beta"
        .parse()
        .map_err(|e| format!("{e}"))?;
    let id1 = synthetic_message_id(&s1, 7, 1)?;
    let id2 = synthetic_message_id(&s2, 7, 1)?;
    assert_ne!(id1, id2);
    assert_eq!(id1, synthetic_message_id(&s1, 7, 1)?);
    Ok(())
}

#[test]
fn blocker_queries_fail_closed_on_corrupt_schema() -> Result<(), String> {
    let conn = rusqlite::Connection::open_in_memory().map_err(|error| error.to_string())?;
    let session: SessionId = "ses_auto_corrupt".parse().map_err(|e| format!("{e}"))?;
    // Corrupt permission_request table missing required columns must return Err, not Ok(false).
    conn.execute_batch("CREATE TABLE permission_request (broken INTEGER);")
        .map_err(|error| error.to_string())?;
    assert!(has_pending_approval_or_form(&conn, &session).is_err());
    Ok(())
}

#[test]
fn running_claim_without_execution_start_is_reclaimed_on_restart() -> Result<(), String> {
    let (mut store, ids, clock) = test_store()?;
    let session: SessionId = "ses_auto_reclaim".parse().map_err(|e| format!("{e}"))?;
    let origin_seq = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_r1")?;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let exec_r1 = ExecutionId::new("exec_r1").ok_or("exec_r1")?;
    grapt_service_state::record(
        &mut store,
        &authorities,
        &session,
        &exec_r1,
        State::described(
            Kind::Disconnected,
            Reason::ProcessLoss,
            "process lost",
            Some(Action::FollowUp),
        ),
        Change::Recover,
    )?;

    // Simulate a crash after claiming `Running` (attempt=1) and BEFORE admitting the synthetic input.
    upsert_row(
        &mut store,
        &session,
        origin_seq,
        "exec_r1",
        1,
        ResumeState::Running,
        None,
        clock.now().as_millis(),
    )?;
    let runnable = list_runnable_after(store.connection(), "", AUTO_RESUME_PAGE_SIZE)?;
    assert_eq!(runnable.len(), 1);
    assert_eq!(runnable[0].session, session);
    assert_eq!(runnable[0].attempts, 1);

    let msg_id = synthetic_message_id(&session, origin_seq, 1)?;
    assert_eq!(
        synthetic_input_progress(store.connection(), &session, &msg_id)?,
        SyntheticProgress::NotPromoted
    );

    // Admitting the synthetic input WITHOUT promoting it (even if an unrelated
    // execution started in the session) must remain NotPromoted.
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.input.admitted",
        1,
        json!({"inputID": msg_id.as_str(), "input": {"type": "synthetic", "data": {"text": "resume"}}}),
    )?;
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.execution.started",
        1,
        json!({"sessionID": session.as_str(), "executionID": "exec_unrelated"}),
    )?;
    assert_eq!(
        synthetic_input_progress(store.connection(), &session, &msg_id)?,
        SyntheticProgress::NotPromoted
    );

    // Promoting msg_id without an execution start (crash between promote_next
    // and execution start) reports PromotedNotStarted rather than Started.
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.input.promoted",
        1,
        json!({"sessionID": session.as_str(), "inputID": msg_id.as_str()}),
    )?;
    assert_eq!(
        synthetic_input_progress(store.connection(), &session, &msg_id)?,
        SyntheticProgress::PromotedNotStarted
    );

    // Once execution actually starts after msg_id's promotion, it is Started.
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.execution.started",
        1,
        json!({"sessionID": session.as_str(), "executionID": "exec_r2"}),
    )?;
    assert_eq!(
        synthetic_input_progress(store.connection(), &session, &msg_id)?,
        SyntheticProgress::Started
    );
    Ok(())
}

fn append_process_loss_step(
    store: &mut EventStore,
    ids: &OsRandomIds,
    clock: &SystemClock,
    session: &SessionId,
    step_num: u32,
) -> Result<(), String> {
    let step = grapt_types::StepNumber::new(step_num).ok_or("step")?;
    let fact = grapt_event::session_step_failed::StepFailedFact::new(
        step,
        grapt_types::StepTerminalReason::ProcessLoss {
            effect: grapt_types::ExternalEffectState::NoneInitiated,
        },
    )
    .map_err(|e| format!("{e:?}"))?;
    let encoded = grapt_event::session_step_failed::encode(&fact).map_err(|e| format!("{e:?}"))?;
    let _ = append_event(
        store,
        ids,
        clock,
        session,
        "session.step.failed",
        2,
        encoded,
    )?;
    Ok(())
}

#[test]
fn real_serialized_process_loss_step_failed_blocks_auto_resume() -> Result<(), String> {
    let (mut store, ids, clock) = test_store()?;
    let empty_forms = HashSet::new();
    let session: SessionId = "ses_auto_real_pl".parse().map_err(|e| format!("{e}"))?;
    let origin = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_pl1")?;

    append_process_loss_step(&mut store, &ids, &clock, &session, 1)?;
    enqueue_if_eligible(&mut store, &session, &empty_forms, &clock, &ids)?;
    let row = load_row(store.connection(), &session)?.ok_or("missing process-loss row")?;
    assert_eq!(row.state, ResumeState::Stopped);
    assert_eq!(row.reason.as_deref(), Some("ambiguous_provider_attempt"));

    // A streaming ToolCalled / session.tool.input.started.2 row WITHOUT
    // durable provider-terminal usage must STILL block auto-resume (Law D1).
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.tool.input.started",
        2,
        json!({"call": "call_1", "tool": "bash"}),
    )?;
    append_process_loss_step(&mut store, &ids, &clock, &session, 2)?;
    assert!(has_ambiguous_process_loss_step(
        store.connection(),
        &session,
        origin
    )?);

    // Durable provider-terminal usage (`session.usage.observed.2` with
    // `phase = "terminal"`) proves AttemptFragment::ProviderTerminal arrived
    // before the tool crash, so auto-resume is safe.
    let attempt = grapt_types::PhysicalAttemptId::new("att_term_1").ok_or("attempt")?;
    let usage = grapt_types::AttemptUsage::observed(
        grapt_types::UsageField::ABSENT,
        grapt_types::UsageField::ABSENT,
        grapt_types::UsageField::ABSENT,
        grapt_types::UsageField::ABSENT,
        grapt_types::UsageField::ABSENT,
        grapt_types::UsageField::ABSENT,
        grapt_types::UsageField::ABSENT,
        grapt_types::UsageObservationPhase::Terminal,
    );
    let usage_fact = grapt_event::session_message::UsageObservedFact::new(attempt, usage);
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.usage.observed",
        2,
        grapt_event::session_message::encode_observed(&usage_fact),
    )?;
    append_process_loss_step(&mut store, &ids, &clock, &session, 3)?;
    assert!(!has_ambiguous_process_loss_step(
        store.connection(),
        &session,
        origin
    )?);

    // A subsequent step with no terminal provider usage of its own still blocks.
    append_process_loss_step(&mut store, &ids, &clock, &session, 4)?;
    assert!(has_ambiguous_process_loss_step(
        store.connection(),
        &session,
        origin
    )?);
    Ok(())
}

#[test]
fn global_null_form_does_not_abort_and_durable_fence_survives_second_crash() -> Result<(), String> {
    let (mut store, ids, clock) = test_store()?;
    let session: SessionId = "ses_auto_fence".parse().map_err(|e| format!("{e}"))?;
    let _origin = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_f1")?;

    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS form_request (
            id TEXT PRIMARY KEY,
            session_id TEXT,
            status TEXT NOT NULL
         );
         INSERT INTO form_request (id, session_id, status) VALUES ('frm_global', NULL, 'pending');
         INSERT INTO form_request (id, session_id, status) VALUES ('frm_sess', 'ses_auto_fence', 'pending');",
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?.discard();

    // Global NULL form_request must not error, and session form gets durably fenced.
    let fenced = fence_pre_recovery_form_sessions(&mut store, &clock)?;
    assert!(fenced.contains(&session));
    assert_eq!(fenced.len(), 1);

    // Simulate elicitation::recover cancelling the form followed by a second crash
    // (empty in-memory pre_recovery_forms on the next restart).
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    tx.execute(
        "UPDATE form_request SET status = 'cancelled' WHERE id = 'frm_sess'",
        [],
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?.discard();

    let empty_forms = HashSet::new();
    enqueue_if_eligible(&mut store, &session, &empty_forms, &clock, &ids)?;
    let row = load_row(store.connection(), &session)?.ok_or("missing fenced row")?;
    assert_eq!(row.state, ResumeState::Stopped);
    assert_eq!(row.reason.as_deref(), Some("pending_approval_or_form"));
    Ok(())
}

fn append_promoted_execution(
    store: &mut EventStore,
    ids: &OsRandomIds,
    clock: &SystemClock,
    session: &SessionId,
    prev_exec: &str,
    input_id: &str,
    next_exec: &str,
) -> Result<ExecutionId, String> {
    let _ = append_event(
        store,
        ids,
        clock,
        session,
        "session.execution.succeeded",
        1,
        json!({"sessionID": session.as_str(), "executionID": prev_exec}),
    )?;
    let _ = append_event(
        store,
        ids,
        clock,
        session,
        "session.input.promoted",
        1,
        json!({"sessionID": session.as_str(), "inputID": input_id}),
    )?;
    let _ = append_event(
        store,
        ids,
        clock,
        session,
        "session.execution.started",
        1,
        json!({"sessionID": session.as_str(), "executionID": next_exec}),
    )?;
    ExecutionId::new(next_exec).ok_or_else(|| next_exec.to_owned())
}

#[test]
fn non_resume_synthetic_or_missing_start_never_inherits_finished_human_turn() -> Result<(), String>
{
    let (mut store, ids, clock) = test_store()?;
    let session: SessionId = "ses_auto_synth".parse().map_err(|e| format!("{e}"))?;
    let origin = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_h1")?;

    // Queue a second human prompt DURING exec_h1, then promote it to open exec_h2.
    let queued_seq = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.input.admitted",
        1,
        json!({"inputID": "msg_u_queued", "input": {"type": "user", "data": {"text": "next turn"}}}),
    )?;
    let exec_h2 = append_promoted_execution(
        &mut store,
        &ids,
        &clock,
        &session,
        "exec_h1",
        "msg_u_queued",
        "exec_h2",
    )?;
    assert_eq!(
        interrupted_turn(store.connection(), &session, &exec_h2)?.map(|t| t.origin_seq),
        Some(queued_seq)
    );

    // Forged synthetic metadata without owner-issued `pending_resume_input` fails closed.
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.input.admitted",
        1,
        json!({
            "inputID": "msg_forged",
            "input": {
                "type": "synthetic",
                "data": {"text": "forged", "metadata": {"kind": "auto_resume", "turnOriginSeq": origin}}
            }
        }),
    )?;
    let exec_forged = append_promoted_execution(
        &mut store,
        &ids,
        &clock,
        &session,
        "exec_h2",
        "msg_forged",
        "exec_forged",
    )?;
    assert_eq!(
        interrupted_turn(store.connection(), &session, &exec_forged)?,
        None
    );

    // Owner-issued `session_resume_input` binding proves lineage without metadata.
    let owned_id = synthetic_message_id(&session, origin, 1)?;
    upsert_row_with_input(
        &mut store,
        &session,
        origin,
        "exec_h1",
        1,
        ResumeState::Running,
        None,
        clock.now().as_millis(),
        Some(&owned_id),
    )?;
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.input.admitted",
        1,
        json!({"inputID": owned_id.as_str(), "input": {"type": "synthetic", "data": {"text": "resume"}}}),
    )?;
    let exec_owned = append_promoted_execution(
        &mut store,
        &ids,
        &clock,
        &session,
        "exec_forged",
        owned_id.as_str(),
        "exec_owned",
    )?;
    assert_eq!(
        interrupted_turn(store.connection(), &session, &exec_owned)?.map(|t| t.origin_seq),
        Some(origin)
    );

    let exec_unknown = ExecutionId::new("exec_missing").ok_or("exec_missing")?;
    assert_eq!(
        interrupted_turn(store.connection(), &session, &exec_unknown)?,
        None
    );
    Ok(())
}

fn admit_resume_in_inbox(
    inbox: &mut grapt_inbox::Inbox,
    ids: &OsRandomIds,
    clock: &SystemClock,
    session: &SessionId,
    resume_id: &grapt_types::MessageId,
) -> Result<grapt_inbox::AdmitOutcome, String> {
    let ev: EventId = format!("evt_{}", ids.mint().as_str())
        .parse()
        .map_err(|e| format!("{e}"))?;
    inbox
        .admit_synthetic(grapt_inbox::SyntheticAdmitRequest {
            event_id: &ev,
            session,
            input: resume_id,
            text: "resume interrupted turn",
            description: None,
            metadata: None,
            delivery: grapt_types::DeliveryMode::Queue,
            resume: true,
            created_ms: clock.now().as_millis(),
        })
        .map_err(|e| e.to_string())
}

#[test]
fn retired_auto_resume_input_is_skipped_by_inbox_promotion() -> Result<(), String> {
    let (mut store, ids, clock) = test_store()?;
    let session: SessionId = "ses_auto_retire".parse().map_err(|e| format!("{e}"))?;
    let origin = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_ret1")?;
    let resume_id = synthetic_message_id(&session, origin, 1)?;
    upsert_row_with_input(
        &mut store,
        &session,
        origin,
        "exec_ret1",
        1,
        ResumeState::Running,
        None,
        clock.now().as_millis(),
        Some(&resume_id),
    )?;
    let mut inbox = grapt_inbox::Inbox::new(store).map_err(|e| e.to_string())?;
    let _ = admit_resume_in_inbox(&mut inbox, &ids, &clock, &session, &resume_id)?;
    let pending_rows = |conn: &rusqlite::Connection| -> Result<i64, String> {
        conn.query_row(
            "SELECT COUNT(*) FROM session_pending WHERE session_id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())
    };
    assert_eq!(pending_rows(inbox.store().connection())?, 1);
    upsert_row(
        inbox.store_mut(),
        &session,
        origin,
        "exec_ret1",
        1,
        ResumeState::Completed,
        Some("superseded_by_newer_prompt"),
        clock.now().as_millis(),
    )?;
    assert_eq!(pending_rows(inbox.store().connection())?, 0);
    // Exact-key retry of the retired synthetic input reconciles as Settled
    // (never WakeAdvised) so prompt_submission never schedules a terminal input.
    let retried = admit_resume_in_inbox(&mut inbox, &ids, &clock, &session, &resume_id)?;
    assert_eq!(
        retried,
        grapt_inbox::AdmitOutcome::Reconciled {
            wake: grapt_inbox::WakeDisposition::Settled
        }
    );
    assert!(!crate::dogfood::support::should_execute(&retried));
    assert_eq!(pending_rows(inbox.store().connection())?, 0);
    let human_id: grapt_types::MessageId = "msg_newer_human".parse().map_err(|e| format!("{e}"))?;
    let ev_human: EventId = format!("evt_{}", ids.mint().as_str())
        .parse()
        .map_err(|e| format!("{e}"))?;
    inbox
        .admit(grapt_inbox::AdmitRequest {
            event_id: &ev_human,
            session: &session,
            input: &human_id,
            text: "new user request",
            delivery: grapt_types::DeliveryMode::Queue,
            resume: true,
            created_ms: clock.now().as_millis(),
        })
        .map_err(|e| e.to_string())?;
    let mut seq_n = 0u32;
    let mut mint = || {
        seq_n += 1;
        format!("evt_promo_{seq_n}")
            .parse::<EventId>()
            .unwrap_or_else(|_| ev_human.clone())
    };
    let promoted = inbox
        .promote_next(
            &session,
            grapt_inbox::PromoteScope::InputAtIdle,
            &mut mint,
            clock.now().as_millis(),
        )
        .map_err(|e| e.to_string())?
        .ok_or("expected human prompt to be promoted")?;
    assert_eq!(promoted.input, human_id);
    let second = inbox
        .promote_next(
            &session,
            grapt_inbox::PromoteScope::InputAtIdle,
            &mut mint,
            clock.now().as_millis(),
        )
        .map_err(|e| e.to_string())?;
    assert!(second.is_none());
    Ok(())
}

#[test]
fn queued_prompt_promoted_after_prior_process_loss_does_not_inherit_ambiguity() -> Result<(), String>
{
    let (mut store, ids, clock) = test_store()?;
    let empty_forms = HashSet::new();
    let session: SessionId = "ses_auto_queued_pl".parse().map_err(|e| format!("{e}"))?;
    let _origin_a = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_a")?;

    // Prompt B is queued while execution A is running, BEFORE execution A
    // suffers an ambiguous ProcessLoss.
    let queued_b_seq = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.input.admitted",
        1,
        json!({"inputID": "msg_b", "input": {"type": "user", "data": {"text": "second prompt"}}}),
    )?;
    append_process_loss_step(&mut store, &ids, &clock, &session, 1)?;
    let exec_b = append_promoted_execution(
        &mut store, &ids, &clock, &session, "exec_a", "msg_b", "exec_b",
    )?;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    grapt_service_state::record(
        &mut store,
        &authorities,
        &session,
        &exec_b,
        State::plain(Kind::Running),
        Change::Start,
    )?;

    // Execution B has no ProcessLoss of its own and must enqueue cleanly rather
    // than inheriting Execution A's earlier ambiguous ProcessLoss.
    enqueue_if_eligible(&mut store, &session, &empty_forms, &clock, &ids)?;
    let row = load_row(store.connection(), &session)?.ok_or("missing queued-b row")?;
    assert_eq!(row.state, ResumeState::Pending);
    assert_eq!(row.turn_origin_seq, queued_b_seq);
    Ok(())
}

#[test]
fn stale_writer_epoch_cannot_retire_resume_input_on_empty_promotion() -> Result<(), String> {
    let dir = tempfile::Builder::new()
        .prefix("grapt-auto-resume-stale-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let db_path = dir.path().join("events.sqlite");
    let ids = OsRandomIds::seed().map_err(|e| e.to_string())?;
    let clock = SystemClock;
    let session: SessionId = "ses_auto_stale_epoch".parse().map_err(|e| format!("{e}"))?;

    let mut store_a = EventStore::open(&db_path, "test").map_err(|e| e.to_string())?;
    grapt_inbox::Inbox::ensure_tables(store_a.connection()).map_err(|e| e.to_string())?;
    let tx = store_a.governed_transaction().map_err(|e| e.to_string())?;
    ensure_schema_within(&tx)?;
    tx.commit().map_err(|e| e.to_string())?.discard();

    let origin = seed_active_turn(&mut store_a, &ids, &clock, &session, None, "exec_s1")?;
    let resume_id = synthetic_message_id(&session, origin, 1)?;
    upsert_row_with_input(
        &mut store_a,
        &session,
        origin,
        "exec_s1",
        1,
        ResumeState::Running,
        None,
        clock.now().as_millis(),
        Some(&resume_id),
    )?;
    let mut inbox_a = grapt_inbox::Inbox::new(store_a).map_err(|e| e.to_string())?;
    let _ = admit_resume_in_inbox(&mut inbox_a, &ids, &clock, &session, &resume_id)?;

    // Second store elects a newer writer epoch and appends a newer human prompt
    // event (leaving `resume_id` as the only pending row so `promote_next` hits
    // the empty-selection retirement path).
    let mut store_b = EventStore::open(&db_path, "test").map_err(|e| e.to_string())?;
    let aggregate = AggregateId::Session(session.clone());
    store_b
        .elect_writer(&aggregate)
        .map_err(|e| e.to_string())?;
    let _ = append_event(
        &mut store_b,
        &ids,
        &clock,
        &session,
        "session.input.admitted",
        1,
        json!({"inputID": "msg_newer_b", "input": {"type": "user", "data": {"text": "newer"}}}),
    )?;

    // Stale store_a must be refused with StaleWriter and must NOT mutate
    // `session_resume_input` or delete `resume_id` from `session_pending`.
    let ev_promo: EventId = format!("evt_{}", ids.mint().as_str())
        .parse()
        .map_err(|e| format!("{e}"))?;
    let mut mint = || ev_promo.clone();
    let err = inbox_a
        .promote_next(
            &session,
            grapt_inbox::PromoteScope::InputAtIdle,
            &mut mint,
            clock.now().as_millis(),
        )
        .err()
        .ok_or("stale writer promote_next must fail")?;
    assert!(
        matches!(
            err,
            grapt_inbox::InboxError::Store(grapt_store::WriteError::Election(
                grapt_store::ElectionError::StaleWriter { .. }
            ))
        ),
        "expected StaleWriter, got {err:?}"
    );
    let active: i64 = store_b
        .connection()
        .query_row(
            "SELECT active FROM session_resume_input WHERE session_id = ?1 AND input_id = ?2",
            rusqlite::params![session.as_str(), resume_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    assert_eq!(active, 1);
    let still_pending: i64 = store_b
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM session_pending WHERE session_id = ?1 AND id = ?2",
            rusqlite::params![session.as_str(), resume_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    assert_eq!(still_pending, 1);
    Ok(())
}

fn assert_stale_inbox_error(err: grapt_inbox::InboxError) {
    assert!(
        matches!(
            err,
            grapt_inbox::InboxError::Store(grapt_store::WriteError::Election(
                grapt_store::ElectionError::StaleWriter { .. }
            ))
        ),
        "expected StaleWriter, got {err:?}"
    );
}

struct StaleTwoStoreFixture {
    _dir: tempfile::TempDir,
    session: SessionId,
    origin: u64,
    resume_1: grapt_types::MessageId,
    inbox_a: grapt_inbox::Inbox,
    store_b: EventStore,
    stale_writer: grapt_store::FencedWriter,
}

fn open_two_store_stale_resume_fixture(session_raw: &str) -> Result<StaleTwoStoreFixture, String> {
    let dir = tempfile::Builder::new()
        .prefix("grapt-auto-resume-stale-reg-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let db_path = dir.path().join("events.sqlite");
    let ids = OsRandomIds::seed().map_err(|e| e.to_string())?;
    let clock = SystemClock;
    let session: SessionId = session_raw.parse().map_err(|e| format!("{e}"))?;
    let mut store_a = EventStore::open(&db_path, "test").map_err(|e| e.to_string())?;
    grapt_inbox::Inbox::ensure_tables(store_a.connection()).map_err(|e| e.to_string())?;
    let tx = store_a.governed_transaction().map_err(|e| e.to_string())?;
    ensure_schema_within(&tx)?;
    tx.commit().map_err(|e| e.to_string())?.discard();

    let origin = seed_active_turn(&mut store_a, &ids, &clock, &session, None, "exec_sr1")?;
    let resume_1 = synthetic_message_id(&session, origin, 1)?;
    upsert_row_with_input(
        &mut store_a,
        &session,
        origin,
        "exec_sr1",
        1,
        ResumeState::Running,
        None,
        clock.now().as_millis(),
        Some(&resume_1),
    )?;
    let mut inbox_a = grapt_inbox::Inbox::new(store_a).map_err(|e| e.to_string())?;
    let _ = admit_resume_in_inbox(&mut inbox_a, &ids, &clock, &session, &resume_1)?;

    let mut store_b = EventStore::open(&db_path, "test").map_err(|e| e.to_string())?;
    let aggregate = AggregateId::Session(session.clone());
    store_b
        .elect_writer(&aggregate)
        .map_err(|e| e.to_string())?;
    let stale_writer = inbox_a
        .store()
        .fencing_token(&aggregate)
        .map_err(|e| e.to_string())?;
    Ok(StaleTwoStoreFixture {
        _dir: dir,
        session,
        origin,
        resume_1,
        inbox_a,
        store_b,
        stale_writer,
    })
}

#[test]
fn stale_writer_epoch_cannot_register_or_retire_resume_input() -> Result<(), String> {
    let mut fx = open_two_store_stale_resume_fixture("ses_auto_stale_reg")?;
    let clock = SystemClock;
    let resume_2 = synthetic_message_id(&fx.session, fx.origin, 2)?;

    {
        let tx = fx
            .inbox_a
            .store_mut()
            .governed_transaction()
            .map_err(|e| e.to_string())?;
        assert_stale_inbox_error(
            grapt_inbox::Inbox::register_resume_input_within(
                &tx,
                fx.stale_writer,
                &fx.session,
                &resume_2,
                fx.origin,
            )
            .err()
            .ok_or("stale register_resume_input_within must fail")?,
        );
    }
    let reg_err = upsert_row_with_input(
        fx.inbox_a.store_mut(),
        &fx.session,
        fx.origin,
        "exec_sr1",
        2,
        ResumeState::Running,
        None,
        clock.now().as_millis(),
        Some(&resume_2),
    )
    .err()
    .ok_or("stale upsert_row_with_input must fail")?;
    assert!(reg_err.contains("writer fence"), "{reg_err}");

    {
        let tx = fx
            .inbox_a
            .store_mut()
            .governed_transaction()
            .map_err(|e| e.to_string())?;
        assert_stale_inbox_error(
            grapt_inbox::Inbox::retire_resume_inputs_within(&tx, fx.stale_writer, &fx.session)
                .err()
                .ok_or("stale retire_resume_inputs_within must fail")?,
        );
    }
    let ret_err = upsert_row(
        fx.inbox_a.store_mut(),
        &fx.session,
        fx.origin,
        "exec_sr1",
        1,
        ResumeState::Completed,
        Some("stale"),
        clock.now().as_millis(),
    )
    .err()
    .ok_or("stale upsert_row retirement must fail")?;
    assert!(ret_err.contains("writer fence"), "{ret_err}");

    let (active, pending_count): (i64, i64) = (
        fx.store_b
            .connection()
            .query_row(
                "SELECT active FROM session_resume_input WHERE session_id = ?1 AND input_id = ?2",
                rusqlite::params![fx.session.as_str(), fx.resume_1.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?,
        fx.store_b
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM session_pending WHERE session_id = ?1 AND id = ?2",
                rusqlite::params![fx.session.as_str(), fx.resume_1.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?,
    );
    assert_eq!((active, pending_count), (1, 1));
    Ok(())
}

#[test]
fn empty_promotion_retirement_reports_retired_only_for_stream_refresh() -> Result<(), String> {
    let (mut store, ids, clock) = test_store()?;
    let session: SessionId = "ses_auto_retired_only"
        .parse()
        .map_err(|e| format!("{e}"))?;
    let origin = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_ro1")?;
    let resume_id = synthetic_message_id(&session, origin, 1)?;
    upsert_row_with_input(
        &mut store,
        &session,
        origin,
        "exec_ro1",
        1,
        ResumeState::Running,
        None,
        clock.now().as_millis(),
        Some(&resume_id),
    )?;
    let mut inbox = grapt_inbox::Inbox::new(store).map_err(|e| e.to_string())?;
    let _ = admit_resume_in_inbox(&mut inbox, &ids, &clock, &session, &resume_id)?;
    // A newer human prompt arrives in the event log so `resume_id` is superseded
    // during `promote_next_with_retirement`, leaving zero promotable rows.
    let _ = append_event(
        inbox.store_mut(),
        &ids,
        &clock,
        &session,
        "session.input.admitted",
        1,
        json!({"inputID": "msg_human_newer", "input": {"type": "user", "data": {"text": "hi"}}}),
    )?;
    let ev: EventId = format!("evt_{}", ids.mint().as_str())
        .parse()
        .map_err(|e| format!("{e}"))?;
    let mut mint = || ev.clone();
    let outcome = inbox
        .promote_next_with_retirement(
            &session,
            grapt_inbox::PromoteScope::InputAtIdle,
            &mut mint,
            clock.now().as_millis(),
        )
        .map_err(|e| e.to_string())?;
    assert_eq!(
        outcome,
        grapt_inbox::PromoteOutcome::RetiredOnly { retired: 1 }
    );
    let second = inbox
        .promote_next_with_retirement(
            &session,
            grapt_inbox::PromoteScope::InputAtIdle,
            &mut mint,
            clock.now().as_millis(),
        )
        .map_err(|e| e.to_string())?;
    assert_eq!(second, grapt_inbox::PromoteOutcome::Empty);
    Ok(())
}

#[test]
fn promoted_and_retired_resume_retry_returns_settled_not_wake_advised() -> Result<(), String> {
    let (mut store, ids, clock) = test_store()?;
    let session: SessionId = "ses_auto_proj_retired"
        .parse()
        .map_err(|e| format!("{e}"))?;
    let origin = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_pr1")?;
    let resume_id = synthetic_message_id(&session, origin, 1)?;
    upsert_row_with_input(
        &mut store,
        &session,
        origin,
        "exec_pr1",
        1,
        ResumeState::Running,
        None,
        clock.now().as_millis(),
        Some(&resume_id),
    )?;
    let mut inbox = grapt_inbox::Inbox::new(store).map_err(|e| e.to_string())?;
    let _ = admit_resume_in_inbox(&mut inbox, &ids, &clock, &session, &resume_id)?;
    let ev: EventId = format!("evt_{}", ids.mint().as_str())
        .parse()
        .map_err(|e| format!("{e}"))?;
    let mut mint = || ev.clone();
    let promoted = inbox
        .promote_next(
            &session,
            grapt_inbox::PromoteScope::InputAtIdle,
            &mut mint,
            clock.now().as_millis(),
        )
        .map_err(|e| e.to_string())?;
    assert!(promoted.is_some());
    // Retire the already-promoted resume input (creating a projected row with active=0).
    upsert_row(
        inbox.store_mut(),
        &session,
        origin,
        "exec_pr1",
        1,
        ResumeState::Completed,
        Some("execution_succeeded"),
        clock.now().as_millis(),
    )?;
    let retried = admit_resume_in_inbox(&mut inbox, &ids, &clock, &session, &resume_id)?;
    assert_eq!(
        retried,
        grapt_inbox::AdmitOutcome::Reconciled {
            wake: grapt_inbox::WakeDisposition::Settled
        }
    );
    assert!(!crate::dogfood::support::should_execute(&retried));
    Ok(())
}

#[test]
fn custody_tied_terminal_settlement_ignores_unrelated_input_and_commits_atomically(
) -> Result<(), String> {
    let (mut store, ids, clock) = test_store()?;
    let session: SessionId = "ses_auto_custody_settle"
        .parse()
        .map_err(|e| format!("{e}"))?;
    let origin = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_cs1")?;
    let resume_id = synthetic_message_id(&session, origin, 1)?;
    let unrelated_id: grapt_types::MessageId =
        "msg_unrelated_wake".parse().map_err(|e| format!("{e}"))?;
    upsert_row_with_input(
        &mut store,
        &session,
        origin,
        "exec_cs1",
        1,
        ResumeState::Running,
        None,
        clock.now().as_millis(),
        Some(&resume_id),
    )?;
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate).map_err(|e| e.to_string())?;
    // Unowned or unrelated input must NOT settle the running resume claim.
    {
        let tx = store.governed_transaction().map_err(|e| e.to_string())?;
        assert_eq!(
            settle_for_execution_within(
                &tx,
                writer,
                &session,
                None,
                grapt_session::execution::Outcome::Interrupted,
                clock.now().as_millis(),
            )?,
            0
        );
        assert_eq!(
            settle_for_execution_within(
                &tx,
                writer,
                &session,
                Some(&unrelated_id),
                grapt_session::execution::Outcome::Interrupted,
                clock.now().as_millis(),
            )?,
            0
        );
        tx.commit().map_err(|e| e.to_string())?.discard();
    }
    let still_running = load_row(store.connection(), &session)?.ok_or("missing row")?;
    assert_eq!(still_running.state, ResumeState::Running);

    // Matching resume input settles atomically with the execution terminal commit.
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let exec_2 = ExecutionId::new("exec_cs2").ok_or("exec_cs2")?;
    grapt_service_state::lifecycle::begin(&mut store, &authorities, &session, &exec_2)?;
    grapt_service_state::lifecycle::record_with(
        &mut store,
        &authorities,
        &session,
        &exec_2,
        grapt_session::execution::Change::Succeeded,
        |tx, w| {
            let _ = settle_for_execution_within(
                tx,
                w,
                &session,
                Some(&resume_id),
                grapt_session::execution::Outcome::Succeeded,
                clock.now().as_millis(),
            )?;
            Ok(())
        },
    )?;
    let settled = load_row(store.connection(), &session)?.ok_or("missing settled row")?;
    assert_eq!(settled.state, ResumeState::Completed);
    let active: i64 = store
        .connection()
        .query_row(
            "SELECT active FROM session_resume_input WHERE session_id = ?1 AND input_id = ?2",
            rusqlite::params![session.as_str(), resume_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    assert_eq!(active, 0);
    Ok(())
}
