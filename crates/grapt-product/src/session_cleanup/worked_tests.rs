//! Public cleanup argv/run regression, using only a freshly created synthetic
//! service and owner APIs. No daily-driver database or provider is involved.
use super::tests::{initialize, seed, Clock, Ids};
use super::*;
use grapt_artifact::{
    ArtifactKind, ArtifactStore, CaptureBound, FilesystemArtifactStore, ReadBudget, Sensitivity,
};
use grapt_event::{
    session_message::{AssistantMessageOpenedFact, ContentAppendedFact},
    session_step_ended::StepEndedFact,
    session_tool_identity,
    session_tool_terminal::ToolTerminalFact,
};
use grapt_inbox::{AdmitRequest, Inbox, PromoteScope};
use grapt_session::{execution, SessionHistoryStore};
use grapt_types::{
    ContentKind, ContentOrdinal, DeliveryMode, EventId, EventVersion, ExecutionId, MessageId,
    NormalizedContentItem, PhysicalAttemptId, ProviderCompletionDisposition, StepNumber,
    ToolCallId, ToolDefinitionId, ToolExecution, ToolTerminalOutcome,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn public_cleanup_tombstones_completed_work_without_discarding_history_or_pending_work() -> Result {
    let directory =
        std::env::temp_dir().join(format!("grapt-cleanup-worked-{}", std::process::id()));
    // Refuse a collision; never clear a directory this fixture did not create.
    std::fs::create_dir(&directory)?;
    let fixture = Fixture(directory);
    let database = fixture.0.join("database.sqlite3");
    let ids = Ids::default();
    let mut owned = EventStore::open_owned_with_authorities(
        &database,
        "cleanup-regression",
        OwnedOpenAuthority::Initialize,
        &Clock(1),
        &ids,
    )?;
    initialize(owned.store_mut())?;
    let mut inbox = Inbox::new(owned.into_store())?;
    let current = seed(inbox.store_mut(), &ids, "ses_current", None, 10)?;
    let recent = seed(inbox.store_mut(), &ids, "ses_recent", None, 100)?;
    let active = seed(inbox.store_mut(), &ids, "ses_active", None, 10)?;
    let pending = seed(inbox.store_mut(), &ids, "ses_pending", None, 10)?;
    let parent = seed(inbox.store_mut(), &ids, "ses_parent", None, 10)?;
    let child = seed(inbox.store_mut(), &ids, "ses_child", Some(&parent), 10)?;
    let done = seed(inbox.store_mut(), &ids, "ses_worked", None, 10)?;
    let effect = seed(inbox.store_mut(), &ids, "ses_pending_effect", None, 10)?;
    let auth = DeterministicAuthorities::new(&Clock(20), &ids);
    execution::begin(
        inbox.store_mut(),
        &auth,
        &active,
        &ExecutionId::new("active").ok_or("execution")?,
    )?;
    let input: MessageId = "msg_worked_input".parse()?;
    inbox.admit(AdmitRequest {
        event_id: &"evt_worked_input".parse()?,
        session: &done,
        input: &input,
        text: "inspect the repository",
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 20,
    })?;
    let promoted: EventId = "evt_worked_promoted".parse()?;
    inbox
        .promote_next(
            &done,
            PromoteScope::InputAtIdle,
            &mut || promoted.clone(),
            20,
        )?
        .ok_or("input was not promoted")?;
    inbox.admit(AdmitRequest {
        event_id: &"evt_keep_input".parse()?,
        session: &pending,
        input: &"msg_keep_input".parse()?,
        text: "still queued",
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 20,
    })?;
    let artifacts = FilesystemArtifactStore::open(
        fixture.0.join("artifacts"),
        b"fixture-only".to_vec(),
        Box::new(Clock(20)),
    )?;
    let captured = artifacts.capture(
        "repository inspected".as_bytes(),
        CaptureBound::new(1024, 64).ok_or("bound")?,
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    )?;
    let tool_input = artifacts.capture(
        br#"{"path":"README.md"}"#.as_slice(),
        CaptureBound::new(1024, 64).ok_or("bound")?,
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    )?;
    for (session, label, pending_tool) in [(&done, "done", false), (&effect, "effect", true)] {
        let execution = ExecutionId::new(label).ok_or("execution")?;
        execution::begin(inbox.store_mut(), &auth, session, &execution)?;
        let attempt = PhysicalAttemptId::new(label).ok_or("attempt")?;
        let message: MessageId = format!("msg_{label}").parse()?;
        let call = ToolCallId::new(label).ok_or("call")?;
        let step = StepNumber::new(1).ok_or("step")?;
        let writer = inbox
            .store()
            .fencing_token(&grapt_store::AggregateId::Session(session.clone()))?;
        let tx = inbox.store_mut().governed_transaction()?;
        let mint = || -> Result<EventId> { Ok(format!("evt_{}", ids.mint().as_str()).parse()?) };
        grapt_store::session_message::record_opened(
            &tx,
            session,
            &mint()?,
            writer,
            20,
            &AssistantMessageOpenedFact::new(attempt.clone(), step, message.clone()),
            &mut [],
        )?;
        grapt_store::session_message::record_appended(
            &tx,
            session,
            &mint()?,
            writer,
            20,
            &ContentAppendedFact::new(
                message.clone(),
                NormalizedContentItem::new(
                    ContentOrdinal::FIRST,
                    ContentKind::Text,
                    captured.reference().clone(),
                ),
                captured.disposition(),
            )?,
            &mut [],
        )?;
        let identity = session_tool_identity::ToolInputIdentity {
            session: session.clone(),
            message: message.clone(),
            call: call.clone(),
            tool: ToolDefinitionId::new("read").ok_or("tool")?,
            execution: ToolExecution::Local,
        };
        EventStore::append_within(
            &tx,
            grapt_store::AppendInput {
                id: &mint()?,
                aggregate: grapt_store::AggregateId::Session(session.clone()),
                writer,
                kind_name: "session.tool.input.started",
                version: EventVersion::new(2).ok_or("version")?,
                created_ms: 20,
                data: &session_tool_identity::encode(&identity),
            },
            &mut [],
        )?;
        grapt_store::session_message::record_appended(
            &tx,
            session,
            &mint()?,
            writer,
            20,
            &ContentAppendedFact::new(
                message,
                NormalizedContentItem::new(
                    ContentOrdinal::new(1),
                    ContentKind::Tool { call: call.clone() },
                    tool_input.reference().clone(),
                ),
                tool_input.disposition(),
            )?,
            &mut [],
        )?;
        if !pending_tool {
            grapt_store::tool_terminal::record(
                &tx,
                session,
                &mint()?,
                writer,
                20,
                &ToolTerminalFact::new(
                    attempt,
                    call,
                    ToolTerminalOutcome::Completed { output: None },
                ),
                &mut [],
            )?;
        }
        grapt_store::step_ended::record(
            &tx,
            session,
            &mint()?,
            writer,
            20,
            &StepEndedFact::new(step, ProviderCompletionDisposition::AssertedComplete),
            &mut [],
        )?;
        tx.commit()?.discard();
        execution::settle(
            inbox.store_mut(),
            &auth,
            session,
            &execution,
            execution::Outcome::Succeeded,
        )?;
    }
    let before: i64 =
        inbox
            .store()
            .connection()
            .query_row("SELECT count(*) FROM event", [], |row| row.get(0))?;
    drop(inbox);
    let parse = |apply| -> Result<CleanupCommand> {
        let mut argv = vec![
            "--service",
            fixture.0.to_str().ok_or("UTF-8 path")?,
            "--cutoff",
            "100",
            "--current-session",
            current.as_str(),
        ];
        if apply {
            argv.push("--apply");
        }
        Ok(CleanupCommand::parse(argv)?)
    };
    let dry = parse(false)?.run("cleanup-regression", &Clock(200), &ids)?;
    assert!(!dry.applied);
    assert!(dry
        .sessions
        .iter()
        .any(|row| row.session == done && row.disposition == CleanupDisposition::Eligible));
    assert!(dry
        .sessions
        .iter()
        .filter(|row| row.session != done)
        .all(|row| matches!(row.disposition, CleanupDisposition::Retained(_))));
    let log = EventLog::open_read_only(&database)?;
    assert_eq!(
        log.with_read_snapshot(|snapshot| snapshot
            .prepare("SELECT count(*) FROM event")
            .map_err(|error| error.to_string())?
            .query_row([], |row| row.get::<_, i64>(0))
            .map_err(|error| error.to_string()))?,
        before
    );
    drop(log);
    // Offline election must refuse even an otherwise valid explicit apply.
    let guard = ElectedControllerAuthority::acquire(
        fixture.0.join("registration"),
        LockAttemptPolicy::new(1).ok_or("policy")?,
    )?;
    assert!(parse(true)?
        .run("cleanup-regression", &Clock(200), &ids)
        .is_err());
    drop(guard);
    let applied = parse(true)?.run("cleanup-regression", &Clock(200), &ids)?;
    assert!(applied.applied);
    assert!(applied
        .sessions
        .iter()
        .any(|row| row.session == done && row.disposition == CleanupDisposition::Tombstoned));
    for session in [
        &current, &recent, &active, &pending, &parent, &child, &effect,
    ] {
        assert!(applied.sessions.iter().any(|row| &row.session == session
            && matches!(row.disposition, CleanupDisposition::Retained(_))));
    }
    let log = EventLog::open_read_only(&database)?;
    log.with_read_snapshot(|snapshot| -> std::result::Result<(), String> {
        assert_eq!(snapshot.prepare("SELECT count(*) FROM event WHERE type NOT IN ('session.deleted.1','session.deleted.2')").map_err(|error| error.to_string())?
            .query_row([], |row| row.get::<_, i64>(0)).map_err(|error| error.to_string())?, before);
        Ok(())
    })?;
    drop(log);
    let read = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let messages = SessionHistoryStore::new(&read).fold(&done)?.messages()?;
    assert!(messages
        .iter()
        .any(|message| message.content.is_some() && message.terminal.is_some()));
    assert_eq!(
        artifacts
            .fetch(
                captured.reference(),
                0,
                ReadBudget::new(1024).ok_or("read budget")?
            )?
            .bytes(),
        b"repository inspected"
    );
    assert!(parse(false)?
        .run("cleanup-regression", &Clock(200), &ids)?
        .sessions
        .iter()
        .any(
            |row| row.session == done && matches!(row.disposition, CleanupDisposition::Retained(_))
        ));
    drop(read);
    // Simulate a later owner with unresolved session work: schema discovery
    // must refuse it, not infer that every unknown table is historical.
    let future = rusqlite::Connection::open(&database)?;
    future.execute_batch("CREATE TABLE future_pending_effect (session_id TEXT NOT NULL)")?;
    future.execute(
        "INSERT INTO future_pending_effect(session_id) VALUES (?1)",
        [pending.as_str()],
    )?;
    assert!(matches!(
        parse(true)?.run("cleanup-regression", &Clock(200), &ids),
        Err(CleanupError::Evidence(_))
    ));
    Ok(())
}

#[test]
fn public_cleanup_preserves_current_inbox_audit_and_its_sessions() -> Result {
    let directory =
        std::env::temp_dir().join(format!("grapt-cleanup-audit-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let fixture = Fixture(directory);
    let database = fixture.0.join("database.sqlite3");
    let ids = Ids::default();
    let mut owned = EventStore::open_owned_with_authorities(
        &database,
        "cleanup-regression",
        OwnedOpenAuthority::Initialize,
        &Clock(1),
        &ids,
    )?;
    initialize(owned.store_mut())?;
    let eligible = seed(owned.store_mut(), &ids, "ses_eligible", None, 10)?;
    let audited = seed(owned.store_mut(), &ids, "ses_audited", None, 10)?;
    let current = seed(owned.store_mut(), &ids, "ses_current", None, 10)?;
    // Model an already-migrated owner row. Its contents are not proof of
    // settled work; cleanup must retain both the bytes and the named session.
    let tx = owned.store_mut().governed_transaction()?;
    tx.execute(
        "INSERT INTO inbox_legacy_synthetic_audit (input_id, session_id, event_data)
         VALUES ('inp_audited', ?1, 'owner audit bytes')",
        [audited.as_str()],
    )?;
    tx.commit()?.discard();
    drop(owned);
    let mut argv = vec![
        "--service",
        fixture.0.to_str().ok_or("UTF-8 path")?,
        "--cutoff",
        "100",
        "--current-session",
        current.as_str(),
    ];
    let dry = CleanupCommand::parse(&argv)?.run("cleanup-regression", &Clock(200), &ids)?;
    assert!(dry
        .sessions
        .iter()
        .any(|row| row.session == eligible && row.disposition == CleanupDisposition::Eligible));
    argv.push("--apply");
    let applied = CleanupCommand::parse(argv)?.run("cleanup-regression", &Clock(200), &ids)?;
    assert!(applied
        .sessions
        .iter()
        .any(|row| row.session == eligible && row.disposition == CleanupDisposition::Tombstoned));
    assert!(applied.sessions.iter().any(|row| row.session == audited
        && row.disposition == CleanupDisposition::Retained(RetentionReason::OperationalEvidence)));
    let read = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    assert_eq!(
        read.query_row(
            "SELECT event_data FROM inbox_legacy_synthetic_audit WHERE input_id='inp_audited'",
            [],
            |row| row.get::<_, String>(0),
        )?,
        "owner audit bytes"
    );
    Ok(())
}
