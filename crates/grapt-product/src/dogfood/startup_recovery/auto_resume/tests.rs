use super::store::{
    has_pending_approval_or_form, has_unresolved_or_quarantined_attempt,
    interrupted_turn_origin_seq, load_row, synthetic_message_id, upsert_row, ResumeState,
};
use super::*;
use grapt_store::{AggregateId, AppendInput, Appended, OsRandomIds, SystemClock};
use grapt_types::{DeterministicAuthorities, EventId, EventVersion, ExecutionId};

fn test_store() -> Result<(EventStore, OsRandomIds, SystemClock), String> {
    let mut store = EventStore::open_in_memory().map_err(|error| error.to_string())?;
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
        interrupted_turn_origin_seq(store.connection(), &s_queued, &exec_q1)?,
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
    assert!(!synthetic_input_started(
        store.connection(),
        &session,
        &msg_id
    )?);

    // Admitting the synthetic input WITHOUT promoting/starting (crash between
    // admit_synthetic and schedule_admitted) must STILL reclaim on restart.
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.input.admitted",
        1,
        json!({"inputID": msg_id.as_str(), "input": {"type": "synthetic", "data": {"text": "resume"}}}),
    )?;
    assert!(!synthetic_input_started(
        store.connection(),
        &session,
        &msg_id
    )?);

    // Once execution starts after admission, restart recognizes it as started.
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.execution.started",
        1,
        json!({"sessionID": session.as_str(), "executionID": "exec_r2"}),
    )?;
    assert!(synthetic_input_started(
        store.connection(),
        &session,
        &msg_id
    )?);
    Ok(())
}

#[test]
fn real_serialized_process_loss_step_failed_blocks_auto_resume() -> Result<(), String> {
    let (mut store, ids, clock) = test_store()?;
    let empty_forms = HashSet::new();
    let session: SessionId = "ses_auto_real_pl".parse().map_err(|e| format!("{e}"))?;
    let origin = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_pl1")?;

    let step1 = grapt_types::StepNumber::new(1).ok_or("step")?;
    let fact1 = grapt_event::session_step_failed::StepFailedFact::new(
        step1,
        grapt_types::StepTerminalReason::ProcessLoss {
            effect: grapt_types::ExternalEffectState::NoneInitiated,
        },
    )
    .map_err(|e| format!("{e:?}"))?;
    let encoded1 =
        grapt_event::session_step_failed::encode(&fact1).map_err(|e| format!("{e:?}"))?;
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.step.failed",
        2,
        encoded1,
    )?;

    enqueue_if_eligible(&mut store, &session, &empty_forms, &clock, &ids)?;
    let row = load_row(store.connection(), &session)?.ok_or("missing process-loss row")?;
    assert_eq!(row.state, ResumeState::Stopped);
    assert_eq!(row.reason.as_deref(), Some("ambiguous_provider_attempt"));

    // A step that already started a tool call before ProcessLoss is not an
    // ambiguous in-flight provider attempt, even though tool events carry no
    // `$.step` field.
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.tool.input.started",
        2,
        json!({"call": "call_1", "tool": "bash"}),
    )?;
    let step2 = grapt_types::StepNumber::new(2).ok_or("step2")?;
    let fact2 = grapt_event::session_step_failed::StepFailedFact::new(
        step2,
        grapt_types::StepTerminalReason::ProcessLoss {
            effect: grapt_types::ExternalEffectState::NoneInitiated,
        },
    )
    .map_err(|e| format!("{e:?}"))?;
    let encoded2 =
        grapt_event::session_step_failed::encode(&fact2).map_err(|e| format!("{e:?}"))?;
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.step.failed",
        2,
        encoded2,
    )?;
    assert!(!has_ambiguous_process_loss_step(
        store.connection(),
        &session,
        origin
    )?);

    // A subsequent step with no tool event of its own still blocks.
    let step3 = grapt_types::StepNumber::new(3).ok_or("step3")?;
    let fact3 = grapt_event::session_step_failed::StepFailedFact::new(
        step3,
        grapt_types::StepTerminalReason::ProcessLoss {
            effect: grapt_types::ExternalEffectState::NoneInitiated,
        },
    )
    .map_err(|e| format!("{e:?}"))?;
    let encoded3 =
        grapt_event::session_step_failed::encode(&fact3).map_err(|e| format!("{e:?}"))?;
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.step.failed",
        2,
        encoded3,
    )?;
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

#[test]
fn non_resume_synthetic_or_missing_start_never_inherits_finished_human_turn() -> Result<(), String>
{
    let (mut store, ids, clock) = test_store()?;
    let session: SessionId = "ses_auto_synth".parse().map_err(|e| format!("{e}"))?;
    let _origin = seed_active_turn(&mut store, &ids, &clock, &session, None, "exec_h1")?;

    // Finish the human execution, then start an unrelated non-resume synthetic execution.
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.execution.succeeded",
        1,
        json!({"sessionID": session.as_str(), "executionID": "exec_h1"}),
    )?;
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.input.admitted",
        1,
        json!({"inputID": "msg_bg", "input": {"type": "synthetic", "data": {"text": "background"}}}),
    )?;
    let _ = append_event(
        &mut store,
        &ids,
        &clock,
        &session,
        "session.execution.started",
        1,
        json!({"sessionID": session.as_str(), "executionID": "exec_bg"}),
    )?;

    let exec_bg = ExecutionId::new("exec_bg").ok_or("exec_bg")?;
    assert_eq!(
        interrupted_turn_origin_seq(store.connection(), &session, &exec_bg)?,
        None
    );

    // An execution with no matching session.execution.started.1 must also fail closed.
    let exec_unknown = ExecutionId::new("exec_missing").ok_or("exec_missing")?;
    assert_eq!(
        interrupted_turn_origin_seq(store.connection(), &session, &exec_unknown)?,
        None
    );
    Ok(())
}
