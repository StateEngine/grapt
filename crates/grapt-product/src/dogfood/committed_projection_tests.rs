use crate::commit_fault;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use grapt_event::session_message::AssistantMessageOpenedFact;
use grapt_event::session_step_ended::StepEndedFact;
use grapt_inbox::Inbox;
use grapt_runtime::SessionPorts;
use grapt_session::SessionSeed;
use grapt_store::{AggregateId, EventStore};
use grapt_types::{
    ClockAuthority, DeliveryMode, DeterministicAuthorities, EventId, IdAuthority, IdToken,
    MessageId, PhysicalAttemptId, ProjectId, ProviderCompletionDisposition, SessionId, StepNumber,
    Timestamp,
};

use super::execution::PublicationLedger;

#[derive(Debug)]
struct FixedClock;

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_786_060_800_000)
    }
}

#[derive(Debug, Default)]
struct CountingIds(AtomicU64);

impl IdAuthority for CountingIds {
    fn mint(&self) -> IdToken {
        let value = self.0.fetch_add(1, Ordering::SeqCst);
        match IdToken::new(format!("projection{value:04}")) {
            Some(token) => token,
            None => panic!("the deterministic identity is non-empty"),
        }
    }
}

fn event(value: &str) -> EventId {
    match value.parse() {
        Ok(event) => event,
        Err(_) => panic!("test event identity is valid"),
    }
}

fn message(value: &str) -> MessageId {
    match value.parse() {
        Ok(message) => message,
        Err(_) => panic!("test message identity is valid"),
    }
}

fn session(value: &str) -> SessionId {
    match value.parse() {
        Ok(session) => session,
        Err(_) => panic!("test Session identity is valid"),
    }
}

#[test]
fn publication_refusal_custody_is_per_session() {
    let ledger = Arc::new(PublicationLedger::new());
    let session_a = session("ses_publication_a");
    let session_b = session("ses_publication_b");
    let a_recorded = Arc::new(std::sync::Barrier::new(2));
    let b_checked = Arc::new(std::sync::Barrier::new(2));

    let a_ledger = Arc::clone(&ledger);
    let a_session = session_a.clone();
    let a_recorded_thread = Arc::clone(&a_recorded);
    let b_checked_thread = Arc::clone(&b_checked);
    let a = std::thread::spawn(move || {
        a_ledger.refuse(a_session.clone(), "A overflowed its public DTO".to_owned());
        let _ = a_recorded_thread.wait();
        let _ = b_checked_thread.wait();
        assert_eq!(
            a_ledger.take(&a_session).as_deref(),
            Some("A overflowed its public DTO")
        );
    });

    let _ = a_recorded.wait();
    assert_eq!(
        ledger.take(&session_b),
        None,
        "Session B consumed Session A's advisory refusal"
    );
    let _ = b_checked.wait();
    a.join()
        .unwrap_or_else(|_| panic!("Session A publication witness panicked"));
}

#[test]
fn step_opening_uses_its_committed_identity_and_rejects_missing_or_duplicate_steps(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let session = session("ses_opening");
    let step = StepNumber::new(1).ok_or("Step identity")?;
    let attempt = PhysicalAttemptId::new("pat_opening").ok_or("attempt identity")?;
    let assistant = message("msg_opening");
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let writer = store.fencing_token(&AggregateId::Session(session.clone()))?;
    let tx = store.governed_transaction()?;
    let committed = grapt_store::session_message::record_opened(
        &tx,
        &session,
        &event("evt_opening"),
        writer,
        12345,
        &AssistantMessageOpenedFact::new(attempt.clone(), step, assistant.clone()),
        &mut [],
    )?;
    tx.commit()?.discard();
    let seq = match committed {
        grapt_store::Appended::Committed { seq } => seq,
        _ => panic!("fresh opening reconciled"),
    };
    let mut inbox = Inbox::new(store)?;
    let agent: grapt_api::u3a::WireId<grapt_types::AgentId> =
        super::decode(serde_json::json!("agt_opening"))?;
    let model: grapt_api::u3a::ModelRefDto =
        super::decode(serde_json::json!({"providerID": "openai", "id": "gpt-5.6"}))?;
    let (identity, projected) =
        super::step_events::opening(&inbox, &session, step, agent.clone(), model.clone())?;
    assert_eq!(identity, assistant);
    let value = serde_json::to_value(projected)?;
    assert_eq!(value["type"], "session.step.started");
    assert_eq!(value["id"], "evt_opening");
    assert_eq!(value["created"], 12345.0);
    assert_eq!(value["durable"]["seq"], seq);
    assert_eq!(value["data"]["assistantMessageID"], assistant.as_str());
    assert!(super::step_events::opening(
        &inbox,
        &super::committed_projection_tests::session("ses_absent"),
        step,
        agent.clone(),
        model.clone()
    )
    .is_err());
    assert!(super::step_events::opening(
        &inbox,
        &session,
        StepNumber::new(2).ok_or("Step two")?,
        agent.clone(),
        model.clone()
    )
    .is_err());
    let writer = inbox
        .store()
        .fencing_token(&AggregateId::Session(session.clone()))?;
    let tx = inbox.store_mut().governed_transaction()?;
    grapt_store::session_message::record_opened(
        &tx,
        &session,
        &event("evt_duplicate_opening"),
        writer,
        12346,
        &AssistantMessageOpenedFact::new(attempt, step, message("msg_duplicate_opening")),
        &mut [],
    )?;
    tx.commit()?.discard();
    assert!(super::step_events::opening(&inbox, &session, step, agent, model).is_err());
    Ok(())
}

#[test]
fn input_events_use_exact_committed_rows_and_refuse_foreign_or_corrupt_cursors(
) -> Result<(), Box<dyn std::error::Error>> {
    let inbox = Arc::new(Mutex::new(Inbox::new(EventStore::open_in_memory()?)?));
    let clock = FixedClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let project = ProjectId::new("prj_inputs").ok_or("project identity")?;
    let ports = SessionPorts::new(Arc::clone(&inbox), &authorities);
    let created = ports.create(&SessionSeed::new(project, "E:/inputs", "inputs")?)?;
    let input = message("msg_input_event");
    let text = "first\n\tsecond";
    let admission = ports.prompt_with_id(
        &created.session,
        input.clone(),
        text,
        DeliveryMode::Queue,
        false,
    )?;
    let seq = match admission.outcome {
        grapt_inbox::AdmitOutcome::Admitted { seq, .. } => seq,
        other => panic!("fresh admission reconciled: {other:?}"),
    };
    let mut held = inbox.lock().map_err(|_| "inbox poisoned")?;
    let projected = super::input_events::project(&held, &created.session, seq)?;
    let value = serde_json::to_value(projected)?;
    assert_eq!(value["type"], "session.input.admitted");
    assert_eq!(value["durable"]["seq"], seq);
    assert_eq!(value["data"]["inputID"], input.as_str());
    assert_eq!(value["data"]["input"]["data"]["text"], text);
    assert!(super::input_events::project(&held, &session("ses_foreign"), seq).is_err());
    assert!(super::input_events::project(&held, &created.session, created.seq).is_err());
    let promoted = held
        .promote_next(
            &created.session,
            grapt_inbox::PromoteScope::InputAtIdle,
            &mut || event("evt_input_promoted"),
            clock.now().as_millis(),
        )?
        .ok_or("promotion absent")?;
    let value = serde_json::to_value(super::input_events::project(
        &held,
        &created.session,
        promoted.seq,
    )?)?;
    assert_eq!(value["type"], "session.input.promoted");
    assert_eq!(value["id"], "evt_input_promoted");
    assert_eq!(value["durable"]["seq"], promoted.seq);
    assert_eq!(value["data"]["inputID"], input.as_str());
    // Preserve append-only history. A hostile producer can present a malformed
    // manifest payload at the governed append door; the projection must refuse
    // that committed fixture without modifying a legitimate prior fact.
    let aggregate = AggregateId::Session(created.session.clone());
    let writer = held.store().fencing_token(&aggregate)?;
    let (corrupt, notices) = held.store_mut().append(
        grapt_store::AppendInput {
            id: &event("evt_input_corrupt"),
            aggregate,
            writer,
            kind_name: "session.input.promoted",
            version: grapt_types::EventVersion::new(1).ok_or("event version")?,
            created_ms: clock.now().as_millis(),
            data: &serde_json::json!({}),
        },
        &mut [],
    )?;
    notices.discard();
    let corrupt_seq = match corrupt {
        grapt_store::Appended::Committed { seq } => seq,
        _ => panic!("new hostile fixture reconciled"),
    };
    assert!(super::input_events::project(&held, &created.session, corrupt_seq).is_err());
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn selection_projection_uses_committed_identity_and_noop_has_no_cursor(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_session::selection::{select_agent_committed, select_model_committed};
    let inbox = Arc::new(Mutex::new(Inbox::new(EventStore::open_in_memory()?)?));
    let clock = FixedClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let ports = SessionPorts::new(Arc::clone(&inbox), &authorities);
    let created = ports.create(&SessionSeed::new(
        ProjectId::new("prj_selection").ok_or("project")?,
        if cfg!(windows) {
            "E:/selection"
        } else {
            "/selection"
        },
        "selection",
    )?)?;
    let mut held = inbox.lock().map_err(|_| "inbox poisoned")?;
    for provider in ["openrouter", "openai", "openrouter"] {
        let model = serde_json::json!({"providerID":provider,"id":"model"});
        let seq = select_model_committed(
            held.store_mut(),
            &authorities,
            &created.session,
            &model,
            &mut [],
        )?
        .ok_or("selection cursor absent")?;
        let value =
            serde_json::to_value(super::input_events::project(&held, &created.session, seq)?)?;
        let (id, time): (String, i64) = held.store().connection().query_row(
            "SELECT id, created FROM event WHERE aggregate_id=?1 AND seq=?2",
            (created.session.as_str(), i64::try_from(seq)?),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(value["type"], "session.model.selected");
        assert_eq!(value["id"], id);
        assert_eq!(value["created"].as_f64(), Some(time as f64));
        assert_eq!(value["data"]["model"], model);
        assert_eq!(value["durable"]["seq"], seq);
        assert_eq!(
            select_model_committed(
                held.store_mut(),
                &authorities,
                &created.session,
                &model,
                &mut []
            )?,
            None
        );
        assert!(super::input_events::project(&held, &session("ses_foreign"), seq).is_err());
    }
    let agent = grapt_types::AgentId::new("agt_selection").ok_or("agent identity")?;
    let seq = select_agent_committed(
        held.store_mut(),
        &authorities,
        &created.session,
        &agent,
        &mut [],
    )?
    .ok_or("agent cursor absent")?;
    let value = serde_json::to_value(super::input_events::project(&held, &created.session, seq)?)?;
    assert_eq!(value["type"], "session.agent.selected");
    assert_eq!(value["data"]["agent"], agent.as_str());
    assert_eq!(
        select_agent_committed(
            held.store_mut(),
            &authorities,
            &created.session,
            &agent,
            &mut []
        )?,
        None
    );
    assert!(select_model_committed(
        held.store_mut(),
        &authorities,
        &session("ses_missing"),
        &serde_json::json!({"providerID":"openai","id":"model"}),
        &mut []
    )
    .is_err());
    let _fault = commit_fault::CommitFault::install(
        held.store_mut(),
        rusqlite::hooks::Action::SQLITE_INSERT,
        "event",
        0,
    )?;
    let count = |held: &Inbox| {
        held.store()
            .connection()
            .query_row("SELECT count(*) FROM event", [], |row| row.get::<_, i64>(0))
    };
    let before = count(&held)?;
    assert!(select_model_committed(
        held.store_mut(),
        &authorities,
        &created.session,
        &serde_json::json!({"providerID":"openai","id":"model"}),
        &mut []
    )
    .is_err());
    assert_eq!(
        count(&held)?,
        before,
        "failed selection persisted a publishable event"
    );
    Ok(())
}

#[test]
fn terminal_projection_stays_bound_to_n_when_input_n_plus_one_admits_before_serialization() {
    let store = match EventStore::open_in_memory() {
        Ok(store) => store,
        Err(error) => panic!("in-memory store failed: {error}"),
    };
    let inbox = match Inbox::new(store) {
        Ok(inbox) => Arc::new(Mutex::new(inbox)),
        Err(error) => panic!("inbox failed: {error}"),
    };
    let clock = FixedClock;
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let project = ProjectId::new("prj_projection")
        .unwrap_or_else(|| panic!("the deterministic project identity is non-empty"));
    let seed = SessionSeed::new(project, "E:/projection", "projection witness")
        .unwrap_or_else(|error| panic!("session seed failed: {error}"));
    let ports = SessionPorts::new(Arc::clone(&inbox), &authorities);
    let created = ports
        .create(&seed)
        .unwrap_or_else(|error| panic!("session create failed: {error}"));
    let session: SessionId = created.session;
    let step = StepNumber::new(1).unwrap_or_else(|| panic!("step one is positive"));
    let attempt = PhysicalAttemptId::new("pat_projection")
        .unwrap_or_else(|| panic!("attempt identity is non-empty"));
    let assistant = message("msg_projection_assistant");

    let terminal_seq = {
        let mut guard = match inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let aggregate = AggregateId::Session(session.clone());
        let writer = guard
            .store()
            .fencing_token(&aggregate)
            .unwrap_or_else(|error| panic!("Session writer missing: {error}"));
        let tx = guard
            .store_mut()
            .governed_transaction()
            .unwrap_or_else(|error| panic!("transaction failed: {error}"));
        grapt_store::session_message::record_opened(
            &tx,
            &session,
            &event("evt_projection_opened"),
            writer,
            1_786_060_800_001,
            &AssistantMessageOpenedFact::new(attempt, step, assistant),
            &mut [],
        )
        .unwrap_or_else(|error| panic!("message opening failed: {error}"));
        let appended = grapt_store::step_ended::record(
            &tx,
            &session,
            &event("evt_projection_terminal"),
            writer,
            1_786_060_800_002,
            &StepEndedFact::new(step, ProviderCompletionDisposition::AssertedComplete),
            &mut [],
        )
        .unwrap_or_else(|error| panic!("terminal append failed: {error}"));
        tx.commit()
            .unwrap_or_else(|error| panic!("terminal transaction failed: {error}"))
            .discard();
        match appended {
            grapt_store::Appended::Committed { seq }
            | grapt_store::Appended::Idempotent { seq } => seq,
        }
    };

    let later = message("msg_projection_input_n_plus_one");
    let admission = ports
        .prompt_with_id(
            &session,
            later,
            "arrived after terminal N",
            DeliveryMode::Queue,
            true,
        )
        .unwrap_or_else(|error| panic!("later admission failed: {error}"));
    let later_seq = match admission.outcome {
        grapt_inbox::AdmitOutcome::Admitted { seq, .. } => seq,
        grapt_inbox::AdmitOutcome::Reconciled { .. } => {
            panic!("the later input must be freshly admitted")
        }
    };
    assert!(later_seq > terminal_seq);
    assert_eq!(
        grapt_session::SessionHistoryStore::over(
            inbox
                .lock()
                .unwrap_or_else(|error| panic!("fixture lock failed: {error}"))
                .store()
        )
        .fold(&session)
        .unwrap_or_else(|error| panic!("typed terminal projection failed: {error}"))
        .latest_terminal_committed()
        .unwrap_or_else(|| panic!("typed terminal projection is missing"))
        .seq,
        terminal_seq,
        "the Step projection must retain terminal N, never input N+1"
    );
}

#[path = "rewind_selection_tests.rs"]
mod rewind_selection;
