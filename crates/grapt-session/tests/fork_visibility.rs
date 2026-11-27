use grapt_event::session_revert::{encode, RevertCommittedFact};
use grapt_session::{
    create_session, fork_session, fork_session_before_call, selection, SessionLifecycleError,
    SessionListProjector, SessionSeed, SessionStore,
};
use grapt_store::{AggregateId, AppendInput, Appended, EventStore};
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, EventVersion, IdAuthority, IdToken, MessageId,
    ProjectId, SessionId, Timestamp,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Default)]
struct Sources(AtomicU64);

impl ClockAuthority for Sources {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1234)
    }
}
impl IdAuthority for Sources {
    fn mint(&self) -> IdToken {
        IdToken::new(format!("fork{}", self.0.fetch_add(1, Ordering::SeqCst)))
            .unwrap_or_else(|| panic!("fixture identity refused"))
    }
}

fn parent(
    store: &mut EventStore,
    source: &Sources,
) -> Result<SessionId, Box<dyn std::error::Error>> {
    let seed = SessionSeed::new(
        ProjectId::new("prj_fork").ok_or("project")?,
        "/work/fork",
        "Fork visibility",
    )?;
    Ok(create_session(
        store,
        &DeterministicAuthorities::new(source, source),
        &seed,
        &mut [&mut SessionListProjector],
    )?
    .session)
}

fn append(
    store: &mut EventStore,
    source: &Sources,
    session: &SessionId,
    kind: &str,
    data: Value,
) -> TestResult {
    if kind == "session.subagent-mail.presented" {
        return capture_mail(store, source, data);
    }
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let id = format!("evt_{}", source.mint().as_str()).parse()?;
    let (outcome, notices) = store.append(
        AppendInput {
            id: &id,
            aggregate,
            writer,
            kind_name: kind,
            version: EventVersion::new(1).ok_or("version")?,
            created_ms: 1234,
            data: &data,
        },
        &mut [],
    )?;
    notices.discard();
    assert!(matches!(outcome, Appended::Committed { .. }));
    Ok(())
}

fn capture_mail(store: &mut EventStore, source: &Sources, data: Value) -> TestResult {
    use grapt_store::{AttemptRecordCeiling, ReserveInput, TerminalCoordinates};
    use grapt_types::{
        BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, PhysicalAttemptId,
        RequestSentMarker, ReservedMarker, StepNumber,
    };
    let mut capture = grapt_store::operation_mail::OperationMailPresented::decode(&data)?;
    capture.attempt =
        PhysicalAttemptId::new(format!("attempt_{}", source.mint().as_str())).ok_or("attempt")?;
    let ordinal = source
        .0
        .fetch_add(1, Ordering::SeqCst)
        .checked_add(1)
        .ok_or("step overflow")?;
    capture.step = StepNumber::new(u32::try_from(ordinal)?).ok_or("step")?;
    let aggregate = AggregateId::Session(capture.session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let maximum = std::num::NonZeroU32::new(1024 * 1024).ok_or("record bound")?;
    let ceiling = AttemptRecordCeiling::new(maximum);
    let receipt = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(capture.attempt.clone(), maximum)
            .map_err(|error| format!("{error:?}"))?,
        BoundedSessionId::new(capture.session.clone(), maximum)
            .map_err(|error| format!("{error:?}"))?,
        capture.step,
        grapt_types::AttemptOrdinal::new(1).ok_or("ordinal")?,
        ReservedMarker::new(Timestamp::from_millis(1200)),
    );
    let terminal =
        TerminalCoordinates::new(format!("evt_{}", source.mint().as_str()).parse()?, 1300);
    let tx = store.governed_transaction()?;
    EventStore::reserve_attempt_receipt_within(
        &tx,
        &ReserveInput {
            receipt: &receipt,
            writer,
            terminal: &terminal,
            ceiling,
        },
    )?;
    EventStore::permit_attempt_send_within(
        &tx,
        &capture.attempt,
        writer,
        ceiling,
        RequestSentMarker::new(Timestamp::from_millis(1230)),
    )?;
    let id = format!("evt_agent_mail_{}", capture.input.as_str()).parse()?;
    EventStore::append_operation_mail_presentation_within(
        &tx,
        AppendInput {
            id: &id,
            aggregate,
            writer,
            kind_name: "session.subagent-mail.presented",
            version: EventVersion::new(1).ok_or("version")?,
            created_ms: 1234,
            data: &capture.encode(),
        },
        ceiling,
    )?;
    tx.commit()?.discard();
    Ok(())
}

fn input(
    store: &mut EventStore,
    source: &Sources,
    session: &SessionId,
    text: &str,
) -> Result<MessageId, Box<dyn std::error::Error>> {
    let id: MessageId = format!("msg_{}", source.mint().as_str()).parse()?;
    append(
        store,
        source,
        session,
        "session.input.admitted",
        json!({"sessionID":session.as_str(), "inputID":id.as_str(), "input":{"type":"user", "data":{"text":text}, "delivery":"queue"}}),
    )?;
    append(
        store,
        source,
        session,
        "session.input.promoted",
        json!({"sessionID":session.as_str(), "inputID":id.as_str()}),
    )?;
    Ok(id)
}

fn rewind(
    store: &mut EventStore,
    source: &Sources,
    session: &SessionId,
    before: MessageId,
) -> TestResult {
    // The reader consumes the same typed canonical fact as the rewind owner.
    append(
        store,
        source,
        session,
        "session.revert.committed",
        encode(&RevertCommittedFact::new(session.clone(), before)),
    )
}

fn texts(
    store: &EventStore,
    session: &SessionId,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut query = store.connection().prepare("SELECT json_extract(data,'$.input.data.text') FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND type='session.input.admitted.1' ORDER BY seq")?;
    let texts = query
        .query_map([session.as_str()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(texts)
}

#[test]
fn rewound_conversation_does_not_resurrect_in_a_fork() -> TestResult {
    let source = Sources::default();
    let mut store = EventStore::open_in_memory()?;
    let parent = parent(&mut store, &source)?;
    input(&mut store, &source, &parent, "retained")?;
    let removed = input(&mut store, &source, &parent, "removed by rewind")?;
    rewind(&mut store, &source, &parent, removed)?;
    input(&mut store, &source, &parent, "replacement")?;
    let fork = fork_session(
        &mut store,
        &DeterministicAuthorities::new(&source, &source),
        &parent,
        &mut [],
    )?;
    assert_eq!(
        texts(&store, &fork.session)?,
        ["retained", "replacement"],
        "fork resurrected rewound input"
    );
    Ok(())
}

#[test]
fn fork_preserves_operational_selections_inside_a_rewound_conversation() -> TestResult {
    let source = Sources::default();
    let mut store = EventStore::open_in_memory()?;
    let parent = parent(&mut store, &source)?;
    let authorities = DeterministicAuthorities::new(&source, &source);
    selection::select_preset_committed(
        &mut store,
        &authorities,
        &parent,
        Some("full-access"),
        &mut [],
    )?;
    selection::select_model_committed(
        &mut store,
        &authorities,
        &parent,
        &json!({"id":"before", "providerID":"openai"}),
        &mut [],
    )?;
    input(&mut store, &source, &parent, "retained")?;
    let removed = input(&mut store, &source, &parent, "removed")?;
    selection::select_preset_committed(&mut store, &authorities, &parent, Some("safe"), &mut [])?;
    let model = json!({"id":"after", "providerID":"openai"});
    selection::select_model_committed(&mut store, &authorities, &parent, &model, &mut [])?;
    rewind(&mut store, &source, &parent, removed)?;
    input(&mut store, &source, &parent, "replacement")?;
    let fork = fork_session(&mut store, &authorities, &parent, &mut [])?;
    let record = SessionStore::over(&store).record(&fork.session)?;
    assert_eq!(
        record.preset.as_deref(),
        Some("safe"),
        "fork restored an obsolete permission preset"
    );
    assert_eq!(record.model, Some(model));
    assert_eq!(texts(&store, &fork.session)?, ["retained", "replacement"]);
    Ok(())
}

#[test]
fn corrupt_rewind_refuses_before_a_fork_is_written() -> TestResult {
    let source = Sources::default();
    let mut store = EventStore::open_in_memory()?;
    let parent = parent(&mut store, &source)?;
    input(&mut store, &source, &parent, "retained")?;
    rewind(&mut store, &source, &parent, "msg_missing".parse()?)?;
    assert!(matches!(
        fork_session(
            &mut store,
            &DeterministicAuthorities::new(&source, &source),
            &parent,
            &mut []
        ),
        Err(SessionLifecycleError::Visibility(_))
    ));
    let written: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type='session.forked.2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(written, 0, "an unreadable rewind became an empty fork");
    Ok(())
}

#[test]
fn hidden_rows_do_not_spend_the_fork_row_budget() -> TestResult {
    let source = Sources::default();
    let mut store = EventStore::open_in_memory()?;
    let parent = parent(&mut store, &source)?;
    input(&mut store, &source, &parent, "retained")?;
    let first = input(&mut store, &source, &parent, "first removed")?;
    for _ in 0..(grapt_session::MAX_HISTORY_ROWS + 513) / 2 {
        input(&mut store, &source, &parent, "removed")?;
    }
    rewind(&mut store, &source, &parent, first)?;
    input(&mut store, &source, &parent, "replacement")?;
    let fork = fork_session(
        &mut store,
        &DeterministicAuthorities::new(&source, &source),
        &parent,
        &mut [],
    )?;
    assert_eq!(texts(&store, &fork.session)?, ["retained", "replacement"]);
    Ok(())
}

#[test]
fn a_child_fork_excludes_its_generating_step_and_later_parent_work() -> TestResult {
    let source = Sources::default();
    let mut store = EventStore::open_in_memory()?;
    let parent = parent(&mut store, &source)?;
    input(&mut store, &source, &parent, "completed instruction")?;
    input(&mut store, &source, &parent, "current instruction")?;
    let through: i64 = store.connection().query_row(
        "SELECT MAX(seq) FROM event WHERE aggregate_id=?1",
        [parent.as_str()],
        |row| row.get(0),
    )?;
    let message: MessageId = "msg_spawning".parse()?;
    let call = grapt_types::ToolCallId::new("call_spawn").ok_or("call")?;
    append(
        &mut store,
        &source,
        &parent,
        "session.assistant-message.opened",
        json!({"message":message.as_str(), "attempt":"attempt_spawn", "step":1}),
    )?;
    append(
        &mut store,
        &source,
        &parent,
        "session.tool.input.started",
        json!({"sessionID":parent.as_str(), "assistantMessageID":message.as_str(),
               "callID":call.as_str(), "name":"spawn_agent"}),
    )?;
    input(&mut store, &source, &parent, "later parent work")?;
    let queued: MessageId = "msg_still_queued".parse()?;
    append(
        &mut store,
        &source,
        &parent,
        "session.input.admitted",
        json!({"sessionID":parent.as_str(), "inputID":queued.as_str(),
               "input":{"type":"user", "data":{"text":"queued only"}, "delivery":"queue"}}),
    )?;
    let before: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_id=?1",
        [parent.as_str()],
        |row| row.get(0),
    )?;
    let fork = fork_session_before_call(
        &mut store,
        &DeterministicAuthorities::new(&source, &source),
        &parent,
        &call,
        &mut [],
    )?;
    assert_eq!(
        texts(&store, &fork.session)?,
        ["completed instruction", "current instruction"],
        "child fork copied later parent work"
    );
    let unfinished: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_id=?1 AND type IN ('session.assistant-message.opened.1','session.tool.input.started.1')",
        [fork.session.as_str()], |row| row.get(0))?;
    assert_eq!(
        unfinished, 0,
        "child inherited its unfinished spawning call"
    );
    let inherited: i64 = store.connection().query_row(
        "SELECT json_extract(data,'$.parentSeq') FROM event WHERE aggregate_id=?1 AND type='session.forked.2'",
        [fork.session.as_str()], |row| row.get(0))?;
    assert_eq!(inherited, through);
    let after: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_id=?1",
        [parent.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(before, after, "fork changed its parent's work");
    Ok(())
}

#[test]
fn absent_foreign_and_repeated_call_sources_never_create_a_fork() -> TestResult {
    for manifests in 0..=2 {
        let source = Sources::default();
        let mut store = EventStore::open_in_memory()?;
        let parent = parent(&mut store, &source)?;
        input(&mut store, &source, &parent, "instruction")?;
        append(
            &mut store,
            &source,
            &parent,
            "session.assistant-message.opened",
            json!({"message":"msg_spawning", "attempt":"attempt_spawn", "step":1}),
        )?;
        for _ in 0..manifests {
            append(
                &mut store,
                &source,
                &parent,
                "session.tool.input.started",
                json!({"sessionID":"ses_foreign", "assistantMessageID":"msg_spawning",
                       "callID":"call_spawn", "name":"spawn_agent"}),
            )?;
        }
        let result = fork_session_before_call(
            &mut store,
            &DeterministicAuthorities::new(&source, &source),
            &parent,
            &grapt_types::ToolCallId::new("call_spawn").ok_or("call")?,
            &mut [],
        );
        assert!(
            matches!(result, Err(SessionLifecycleError::Corrupt(_))),
            "invalid source created a child: {result:?}"
        );
        let forks: i64 = store.connection().query_row(
            "SELECT COUNT(*) FROM event WHERE type='session.forked.2'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(forks, 0);
    }
    Ok(())
}

#[test]
fn fork_rewrites_envelope_identities_without_changing_quoted_data() -> TestResult {
    let source = Sources::default();
    let mut store = EventStore::open_in_memory()?;
    let parent = parent(&mut store, &source)?;
    let quoted = "msg_quoted";
    let payload =
        json!({"text":quoted, "metadata":{"sessionID":parent.as_str(), "message":quoted}});
    append(
        &mut store,
        &source,
        &parent,
        "session.input.admitted",
        json!({"sessionID":parent.as_str(), "inputID":quoted,
               "input":{"type":"user", "data":payload, "delivery":"queue"}}),
    )?;
    append(
        &mut store,
        &source,
        &parent,
        "session.input.promoted",
        json!({"sessionID":parent.as_str(), "inputID":quoted}),
    )?;
    let model = json!({"id":quoted, "providerID":"openai"});
    selection::select_model_committed(
        &mut store,
        &DeterministicAuthorities::new(&source, &source),
        &parent,
        &model,
        &mut [],
    )?;
    let fork = fork_session(
        &mut store,
        &DeterministicAuthorities::new(&source, &source),
        &parent,
        &mut [],
    )?;
    let raw: String = store.connection().query_row(
        "SELECT data FROM event WHERE aggregate_id=?1 AND type='session.input.admitted.1'",
        [fork.session.as_str()],
        |row| row.get(0),
    )?;
    let copied: Value = serde_json::from_str(&raw)?;
    assert_ne!(copied["inputID"], quoted);
    assert_eq!(copied["sessionID"], fork.session.as_str());
    assert_eq!(
        copied["input"]["data"], payload,
        "fork rewrote quoted user data"
    );
    assert_eq!(
        SessionStore::over(&store).record(&fork.session)?.model,
        Some(model)
    );
    Ok(())
}

#[test]
fn oversized_fork_data_refuses_before_any_child_is_written() -> TestResult {
    for (rows, bytes) in [(1, 1024 * 1024), (17, 1024 * 1024 - 1024)] {
        let source = Sources::default();
        let mut store = EventStore::open_in_memory()?;
        let parent = parent(&mut store, &source)?;
        let text = "x".repeat(bytes);
        for _ in 0..rows {
            input(&mut store, &source, &parent, &text)?;
        }
        assert!(
            matches!(
                fork_session(
                    &mut store,
                    &DeterministicAuthorities::new(&source, &source),
                    &parent,
                    &mut []
                ),
                Err(SessionLifecycleError::ForkDataTooLong)
            ),
            "oversized fork was accepted"
        );
        let forks: i64 = store.connection().query_row(
            "SELECT COUNT(*) FROM event WHERE type='session.forked.2'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(forks, 0);
    }
    malformed_oversized_input_refuses_before_json_decode()?;
    Ok(())
}

#[test]
fn oversized_lifecycle_data_is_bounded_before_the_parent_record_fold() -> TestResult {
    let source = Sources::default();
    let mut store = EventStore::open_in_memory()?;
    let parent = parent(&mut store, &source)?;
    append(
        &mut store,
        &source,
        &parent,
        "session.renamed",
        json!({"sessionID":parent.as_str(), "title":"x".repeat(1024 * 1024)}),
    )?;
    assert!(
        matches!(
            fork_session(
                &mut store,
                &DeterministicAuthorities::new(&source, &source),
                &parent,
                &mut []
            ),
            Err(SessionLifecycleError::ForkDataTooLong)
        ),
        "fork folded oversized lifecycle data"
    );
    let forks: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type='session.forked.2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(forks, 0);
    Ok(())
}

#[test]
fn a_prepared_fork_location_changes_only_the_child_placement_before_creation() -> TestResult {
    use grapt_types::{LocationRef, Platform, WorkspaceId};
    let source = Sources::default();
    let mut store = EventStore::open_in_memory()?;
    let authorities = DeterministicAuthorities::new(&source, &source);
    let seed = SessionSeed::new(
        ProjectId::new("prj_location").ok_or("project")?,
        "/work/parent",
        "Placement parent",
    )?
    .with_workspace(WorkspaceId::new("ws_parent").ok_or("workspace")?)?;
    let parent = create_session(
        &mut store,
        &authorities,
        &seed,
        &mut [&mut SessionListProjector],
    )?
    .session;
    input(
        &mut store,
        &source,
        &parent,
        "inherited placement instruction",
    )?;
    let call = grapt_types::ToolCallId::new("call_location_spawn").ok_or("call")?;
    append(
        &mut store,
        &source,
        &parent,
        "session.assistant-message.opened",
        json!({"message":"msg_location_spawn","attempt":"attempt_location","step":1}),
    )?;
    append(
        &mut store,
        &source,
        &parent,
        "session.tool.input.started",
        json!({
        "sessionID":parent.as_str(),"assistantMessageID":"msg_location_spawn",
        "callID":call.as_str(),"name":"spawn_agent"}),
    )?;
    let original = SessionStore::over(&store).record(&parent)?;
    let inherited = fork_session_before_call(&mut store, &authorities, &parent, &call, &mut [])?;
    assert_eq!(
        SessionStore::over(&store)
            .record(&inherited.session)?
            .placement,
        original.placement
    );
    for workspace in [WorkspaceId::new("ws_child"), None] {
        let placement = LocationRef::on(Platform::Posix, "/work/child", workspace)?;
        let fork = grapt_session::fork_session_before_call_at_location(
            &mut store,
            &authorities,
            &parent,
            &call,
            &placement,
            &mut [],
        )?;
        let child = SessionStore::over(&store).record(&fork.session)?;
        assert_eq!(child.placement, placement);
        assert_eq!(child.project, original.project);
        assert_eq!(child.parent, Some(parent.clone()));
        assert_eq!(
            texts(&store, &fork.session)?,
            ["inherited placement instruction"]
        );
    }
    assert_eq!(
        SessionStore::over(&store).record(&parent)?,
        original,
        "fork moved its parent"
    );
    let before: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    let oversized = LocationRef::on(Platform::Posix, &format!("/{}", "x".repeat(65536)), None)?;
    assert!(grapt_session::fork_session_before_call_at_location(
        &mut store,
        &authorities,
        &parent,
        &call,
        &oversized,
        &mut []
    )
    .is_err());
    let after: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    assert_eq!(before, after, "invalid placement wrote a partial fork");
    Ok(())
}

fn mail(
    store: &mut EventStore,
    source: &Sources,
    parent: &SessionId,
    id: &str,
    marker: &str,
    presented: bool,
) -> TestResult {
    append(
        store,
        source,
        parent,
        "session.input.admitted",
        json!({
        "sessionID":parent.as_str(),"inputID":id,"input":{"type":"operation-delivery",
            "operationID":"opr_mail","subagentOperationID":"sub_mail",
            "claimID":"claim_mail","generation":1,"settlement":"failed","projection":marker}}),
    )?;
    let seq: i64 = store.connection().query_row(
        "SELECT MAX(seq) FROM event WHERE aggregate_family='session' AND aggregate_id=?1",
        [parent.as_str()],
        |row| row.get(0),
    )?;
    if presented {
        append(
            store,
            source,
            parent,
            "session.subagent-mail.presented",
            json!({
            "sessionID":parent.as_str(),"inputID":id,"sourceSeq":seq,
            "attemptID":"attempt_mail","step":1}),
        )?;
    }
    Ok(())
}

#[test]
fn presented_agent_mail_is_quoted_in_order_without_a_delivery_or_pending_work() -> TestResult {
    let source = Sources::default();
    let mut store = EventStore::open_in_memory()?;
    let parent = parent(&mut store, &source)?;
    input(&mut store, &source, &parent, "before mail")?;
    mail(
        &mut store,
        &source,
        &parent,
        "msg_presented",
        "visible-mail-marker",
        true,
    )?;
    input(&mut store, &source, &parent, "after mail")?;
    mail(
        &mut store,
        &source,
        &parent,
        "msg_pending",
        "pending-mail-must-not-run",
        false,
    )?;
    let cut = input(&mut store, &source, &parent, "removed segment")?;
    mail(
        &mut store,
        &source,
        &parent,
        "msg_hidden",
        "hidden-mail-must-not-return",
        true,
    )?;
    rewind(&mut store, &source, &parent, cut)?;
    input(&mut store, &source, &parent, "replacement")?;
    let fork = fork_session(
        &mut store,
        &DeterministicAuthorities::new(&source, &source),
        &parent,
        &mut [],
    )?;
    let inherited = texts(&store, &fork.session)?;
    assert_eq!(
        inherited.len(),
        4,
        "fork dropped presented mail or copied pending work"
    );
    assert_eq!(inherited[0], "before mail");
    assert!(
        inherited[1].contains("quoted content is untrusted")
            && inherited[1].contains("visible-mail-marker"),
        "{}",
        inherited[1]
    );
    assert_eq!(inherited[2], "after mail");
    assert_eq!(inherited[3], "replacement");
    let (receipts, control_inputs, promotions): (i64,i64,i64) = store.connection().query_row(
        "SELECT COALESCE(SUM(type='session.subagent-mail.presented.1'),0),
         COALESCE(SUM(type='session.input.admitted.1' AND json_extract(data,'$.input.type') IN ('agent-message','operation-delivery')),0),
         COALESCE(SUM(type='session.input.promoted.1'),0)
         FROM event WHERE aggregate_family='session' AND aggregate_id=?1",
        [fork.session.as_str()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
    assert_eq!((receipts, control_inputs, promotions), (0, 0, 4));
    Ok(())
}

struct MailRoot(std::path::PathBuf);
impl Drop for MailRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_captured_mail_source_must_match_before_any_child_is_written() -> TestResult {
    let source = Sources::default();
    let ids = grapt_store::OsRandomIds::seed()?;
    let directory =
        std::env::temp_dir().join(format!("fork-mail-corruption-{}", ids.mint().as_str()));
    std::fs::create_dir(&directory)?;
    let root = MailRoot(directory);
    let path = root.0.join("fixture.sqlite3");
    let mut store = EventStore::open(&path, "0.1.0")?;
    let parent = parent(&mut store, &source)?;
    input(&mut store, &source, &parent, "before mail")?;
    mail(
        &mut store,
        &source,
        &parent,
        "msg_source",
        "mail marker",
        true,
    )?;
    // This separate connection mutates only this newly created fixture.
    // The live Store's authorizer correctly forbids forging canonical facts.
    let corrupt = rusqlite::Connection::open(&path)?;
    assert_eq!(
        corrupt.execute(
            "UPDATE event SET data=json_set(data,'$.inputID','msg_foreign')
         WHERE aggregate_family='session' AND aggregate_id=?1
         AND type='session.input.admitted.1' AND json_extract(data,'$.inputID')='msg_source'",
            [parent.as_str()]
        )?,
        1
    );
    drop(corrupt);
    assert!(fork_session(
        &mut store,
        &DeterministicAuthorities::new(&source, &source),
        &parent,
        &mut []
    )
    .is_err());
    let forks: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type='session.forked.2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(forks, 0, "invalid mail source wrote a partial fork");
    Ok(())
}

fn malformed_oversized_input_refuses_before_json_decode() -> TestResult {
    let source = Sources::default();
    let ids = grapt_store::OsRandomIds::seed()?;
    let directory =
        std::env::temp_dir().join(format!("fork-preallocation-{}", ids.mint().as_str()));
    std::fs::create_dir(&directory)?;
    let root = MailRoot(directory);
    let path = root.0.join("fixture.sqlite3");
    let mut store = EventStore::open(&path, "0.1.0")?;
    let parent = parent(&mut store, &source)?;
    input(&mut store, &source, &parent, "the source becomes oversized")?;
    // Only this fresh fixture is changed: invalid JSON proves that the SQL
    // byte ceiling fires before parsing, independently of later size checks.
    let corrupt = rusqlite::Connection::open(&path)?;
    let oversized = format!("{{{}", " ".repeat(1024 * 1024));
    assert_eq!(corrupt.execute(
        "UPDATE event SET data=?1 WHERE aggregate_family='session' AND aggregate_id=?2 AND type='session.input.admitted.1'",
        rusqlite::params![oversized,parent.as_str()])?,1);
    drop(corrupt);
    let error = fork_session(
        &mut store,
        &DeterministicAuthorities::new(&source, &source),
        &parent,
        &mut [],
    )
    .err()
    .ok_or("oversized malformed input was accepted")?;
    assert!(
        matches!(&error, SessionLifecycleError::ForkDataTooLong),
        "oversized data reached JSON decoding: {error}"
    );
    let forks: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type='session.forked.2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(forks, 0, "oversized source wrote a partial fork");
    Ok(())
}
