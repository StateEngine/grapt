//! Lifecycle-hook results are durable advisory notices, not permission grants.
//! Exercise the public API and its actual JSON wire through governed appends.
use grapt_session::{create_session, notice, CommittedPosition, SessionSeed, SessionStore};
use grapt_store::{AggregateId, AppendInput, Appended, EventStore};
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, EventId, EventSeq, EventVersion, ExecutionId,
    IdAuthority, IdToken, MessageId, ProjectId, SessionId, Timestamp,
};
use serde_json::{json, Value};
use std::cell::Cell;
use std::path::PathBuf;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Default)]
struct Source(Cell<u64>);
impl ClockAuthority for Source {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1234)
    }
}
impl IdAuthority for Source {
    fn mint(&self) -> IdToken {
        self.0.set(self.0.get() + 1);
        IdToken::new(format!("lifecyclehook{}", self.0.get()))
            .unwrap_or_else(|| panic!("fixture ID refused"))
    }
}

fn setup(store: &mut EventStore, source: &Source) -> Result<SessionId> {
    let seed = SessionSeed::new(
        ProjectId::new("prj_lifecycle_hooks").ok_or("project ID refused")?,
        "/work/lifecycle-hooks",
        "lifecycle hooks",
    )?;
    Ok(create_session(
        store,
        &DeterministicAuthorities::new(source, source),
        &seed,
        &mut [],
    )?
    .session)
}

fn execution() -> Result<ExecutionId> {
    // ExecutionId is opaque, not a prefixed SessionId. Do not accidentally
    // impose a new prefix law when serializing or decoding the notice.
    Ok(ExecutionId::new("opaque hook turn #1").ok_or("execution ID refused")?)
}

fn wire(session: &SessionId) -> Value {
    json!({
        "sessionID": session.as_str(),
        "executionID": "opaque hook turn #1",
        "kind": "lifecycle-hook",
        "event": "PreToolUse",
        "text": "The hook denied the tool: project policy.",
    })
}

/// Corruption witnesses use the public governed event append, never writable
/// access to an existing real database or a direct insert around store custody.
fn append_wire(
    store: &mut EventStore,
    source: &Source,
    session: &SessionId,
    data: &Value,
) -> Result<CommittedPosition> {
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let tx = store.governed_transaction()?;
    SessionStore::new(&tx).record(session)?;
    let event: EventId = format!("evt_{}", source.mint().as_str()).parse()?;
    let appended = EventStore::append_within(
        &tx,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.notice.recorded",
            version: EventVersion::new(1).ok_or("event version refused")?,
            created_ms: 1234,
            data,
        },
        &mut [],
    )?;
    let Appended::Committed { seq } = appended else {
        return Err("fixture event was not newly committed".into());
    };
    tx.commit()?.discard();
    Ok(CommittedPosition {
        event,
        seq,
        created_ms: 1234,
    })
}

fn notice_count(store: &EventStore, session: &SessionId) -> Result<i64> {
    Ok(store.connection().query_row(
        "SELECT count(*) FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND type='session.notice.recorded.1'",
        [session.as_str()],
        |row| row.get(0),
    )?)
}

fn public_at(store: &EventStore, session: &SessionId, seq: u64) -> Result<bool> {
    Ok(store.connection().query_row(
        &format!(
            "SELECT ({}) FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND seq=?2 AND type='session.notice.recorded.1'",
            notice::public_notice_predicate("data"),
        ),
        (session.as_str(), i64::try_from(seq)?),
        |row| row.get(0),
    )?)
}

struct Database(PathBuf);
impl Database {
    fn new() -> Result<Self> {
        let ids = grapt_store::OsRandomIds::seed()?;
        let path = std::env::temp_dir().join(format!(
            "grapt-lifecycle-hook-notices-{}.sqlite",
            ids.mint().as_str()
        ));
        assert!(!path.exists(), "refusing existing witness data");
        Ok(Self(path))
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        // The fixture is declared before its store, so handles close first.
        let _ = std::fs::remove_file(&self.0);
        for suffix in ["-wal", "-shm"] {
            let mut path = self.0.as_os_str().to_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(path));
        }
    }
}

#[test]
fn every_typed_lifecycle_event_roundtrips_through_durable_json_and_public_reads() -> Result {
    let source = Source::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory()?;
    let session = setup(&mut store, &source)?;
    let execution = execution()?;
    let mut recorded = Vec::new();
    let events = [
        ("SessionStart", notice::LifecycleEvent::SessionStart),
        ("UserPromptSubmit", notice::LifecycleEvent::UserPromptSubmit),
        ("PreToolUse", notice::LifecycleEvent::PreToolUse),
        ("PostToolUse", notice::LifecycleEvent::PostToolUse),
        ("Stop", notice::LifecycleEvent::Stop),
        ("Notification", notice::LifecycleEvent::Notification),
        ("SessionEnd", notice::LifecycleEvent::SessionEnd),
        ("PreCompact", notice::LifecycleEvent::PreCompact),
        ("PostCompact", notice::LifecycleEvent::PostCompact),
        ("SubagentStart", notice::LifecycleEvent::SubagentStart),
        ("SubagentStop", notice::LifecycleEvent::SubagentStop),
        (
            "PermissionRequest",
            notice::LifecycleEvent::PermissionRequest,
        ),
        ("WaitingForInput", notice::LifecycleEvent::WaitingForInput),
    ];
    assert_eq!(notice::LifecycleEvent::ALL.len(), events.len());
    for (name, event) in events {
        assert_eq!(event.as_str(), name);
        assert_eq!(notice::LifecycleEvent::parse(name)?, event);
        let text = format!("{name}: non-secret policy context é");
        let entry = notice::record_lifecycle_hook(
            &mut store,
            &authorities,
            &session,
            &execution,
            event,
            &text,
        )?;
        assert_eq!(
            entry.scope,
            notice::LifecycleHookScope::Execution(execution.clone())
        );
        assert_eq!(entry.event, event);
        assert_eq!(entry.text, text);
        assert_eq!(entry.position.created_ms, 1234);
        let raw: String = store.connection().query_row(
            "SELECT data FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND seq=?2",
            (session.as_str(), i64::try_from(entry.position.seq)?),
            |row| row.get(0),
        )?;
        assert_eq!(
            serde_json::from_str::<Value>(&raw)?,
            json!({"sessionID":session.as_str(), "executionID":execution.as_str(),
                "kind":"lifecycle-hook", "event":name, "text":text}),
        );
        assert!(public_at(&store, &session, entry.position.seq)?);
        let at = notice::read_notice_at(
            store.connection(),
            &session,
            EventSeq::new(entry.position.seq),
        )?;
        assert_eq!(at.lifecycle_hook, std::slice::from_ref(&entry));
        assert!(at.mcp.is_empty());
        assert!(at.skipped.is_empty());
        recorded.push(entry);
    }
    assert_eq!(
        notice::lifecycle_hook_notices(store.connection(), &session)?,
        recorded,
    );
    let read = notice::read_notices(store.connection(), &session)?;
    assert_eq!(read.lifecycle_hook, recorded);
    assert!(read.skipped.is_empty());
    assert!(notice::skipped_kinds(store.connection(), &session)?.is_empty());
    for name in ["sessionstart", "pre_tool_use", "SessionEnded", "", "Stop "] {
        assert!(notice::LifecycleEvent::parse(name).is_err());
    }
    Ok(())
}

#[test]
fn actual_notice_wire_decodes_into_owning_types_without_requiring_execution_prefixes() -> Result {
    let source = Source::default();
    let mut store = EventStore::open_in_memory()?;
    let session = setup(&mut store, &source)?;
    let position = append_wire(&mut store, &source, &session, &wire(&session))?;
    let entries = notice::lifecycle_hook_notices(store.connection(), &session)?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].position, position);
    assert_eq!(
        entries[0].scope,
        notice::LifecycleHookScope::Execution(execution()?)
    );
    assert_eq!(entries[0].event, notice::LifecycleEvent::PreToolUse);
    assert_eq!(
        entries[0].text,
        wire(&session)["text"]
            .as_str()
            .ok_or("fixture text missing")?
    );
    assert_eq!(
        notice::read_notice_at(store.connection(), &session, EventSeq::new(position.seq))?
            .lifecycle_hook,
        entries,
    );
    Ok(())
}

#[test]
fn input_scoped_hook_notices_roundtrip_without_fabricating_an_execution() -> Result {
    let source = Source::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory()?;
    let session = setup(&mut store, &source)?;
    let message: MessageId = "msg_hook_pending_input".parse()?;
    let mut recorded = Vec::new();
    for event in [
        notice::LifecycleEvent::PreToolUse,
        notice::LifecycleEvent::PostToolUse,
    ] {
        let entry = notice::record_input_lifecycle_hook(
            &mut store,
            &authorities,
            &session,
            &message,
            event,
            "input interpolation hook outcome",
        )?;
        assert_eq!(
            entry.scope,
            notice::LifecycleHookScope::Input(message.clone())
        );
        let raw: String = store.connection().query_row(
            "SELECT data FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND seq=?2",
            (session.as_str(), i64::try_from(entry.position.seq)?),
            |row| row.get(0),
        )?;
        let data: Value = serde_json::from_str(&raw)?;
        assert_eq!(data["sessionID"], session.as_str());
        assert_eq!(data["messageID"], message.as_str());
        assert!(
            data.get("executionID").is_none_or(Value::is_null),
            "input-scoped notice invented an execution coordinate"
        );
        assert_eq!(data["kind"], "lifecycle-hook");
        assert_eq!(data["event"], event.as_str());
        let at = notice::read_notice_at(
            store.connection(),
            &session,
            EventSeq::new(entry.position.seq),
        )?;
        assert_eq!(at.lifecycle_hook, std::slice::from_ref(&entry));
        assert!(public_at(&store, &session, entry.position.seq)?);
        recorded.push(entry);
    }
    // Exercise the wire decoder independently of the input-scope writer too.
    let mut data = wire(&session);
    data.as_object_mut()
        .ok_or("fixture is not an object")?
        .remove("executionID");
    data["messageID"] = json!(message.as_str());
    let position = append_wire(&mut store, &source, &session, &data)?;
    let at = notice::read_notice_at(store.connection(), &session, EventSeq::new(position.seq))?;
    assert_eq!(at.lifecycle_hook.len(), 1);
    assert_eq!(
        at.lifecycle_hook[0].scope,
        notice::LifecycleHookScope::Input(message)
    );
    recorded.extend(at.lifecycle_hook);
    let all = notice::read_notices(store.connection(), &session)?;
    assert_eq!(all.lifecycle_hook, recorded);
    assert!(all.skipped.is_empty());
    Ok(())
}

#[test]
fn wire_coordinates_require_exactly_one_valid_execution_or_input_identity() -> Result {
    for (execution, message) in [
        (None, None),
        (Some(Value::Null), Some(Value::Null)),
        (Some(json!("opaque turn")), Some(json!("msg_both"))),
        (Some(json!("")), Some(json!("msg_both"))),
        (None, Some(json!(""))),
        (None, Some(json!("not-a-message"))),
        (None, Some(json!("ses_wrong_type"))),
        (None, Some(json!("msg_"))),
        (None, Some(json!("msg_bad-byte"))),
        (None, Some(json!(42))),
        (None, Some(json!(["msg_wrong_shape"]))),
    ] {
        let source = Source::default();
        let mut store = EventStore::open_in_memory()?;
        let session = setup(&mut store, &source)?;
        let mut data = wire(&session);
        let object = data.as_object_mut().ok_or("fixture is not an object")?;
        object.remove("executionID");
        if let Some(value) = execution {
            object.insert("executionID".into(), value);
        }
        if let Some(value) = message {
            object.insert("messageID".into(), value);
        }
        let position = append_wire(&mut store, &source, &session, &data)?;
        assert!(public_at(&store, &session, position.seq)?);
        assert!(
            notice::lifecycle_hook_notices(store.connection(), &session).is_err(),
            "invalid scope coordinates decoded: {data}"
        );
        assert!(notice::read_notices(store.connection(), &session).is_err());
        assert!(
            notice::read_notice_at(store.connection(), &session, EventSeq::new(position.seq))
                .is_err()
        );
        assert!(notice::skipped_kinds(store.connection(), &session)?.is_empty());
    }
    Ok(())
}

#[test]
fn malformed_known_hook_wire_refuses_all_decoders_instead_of_becoming_skipped() -> Result {
    let cases = [
        ("event", Some(json!("pre_tool_use"))),
        ("event", Some(json!("SessionEnded"))),
        ("event", Some(json!(false))),
        ("event", Some(Value::Null)),
        ("event", None),
        ("sessionID", Some(json!("ses_someoneelse"))),
        ("sessionID", Some(json!("not a session"))),
        ("sessionID", Some(json!(42))),
        ("sessionID", None),
        ("executionID", Some(json!(""))),
        ("executionID", Some(json!(false))),
        ("executionID", Some(Value::Null)),
        ("executionID", None),
        ("text", Some(json!(""))),
        ("text", Some(json!("contains\0nul"))),
        (
            "text",
            Some(json!("x".repeat(notice::MAX_HOOK_RESULT_BYTES + 1))),
        ),
        (
            "text",
            Some(json!("é".repeat(notice::MAX_HOOK_RESULT_BYTES / 2 + 1))),
        ),
        ("text", Some(json!(["not text"]))),
        ("text", None),
        ("unknownField", Some(json!(true))),
    ];
    for (field, replacement) in cases {
        let source = Source::default();
        let mut store = EventStore::open_in_memory()?;
        let session = setup(&mut store, &source)?;
        let mut data = wire(&session);
        let object = data.as_object_mut().ok_or("fixture is not an object")?;
        if let Some(value) = replacement {
            object.insert(field.to_owned(), value);
        } else {
            object.remove(field);
        }
        let position = append_wire(&mut store, &source, &session, &data)?;
        assert!(
            public_at(&store, &session, position.seq)?,
            "malformed {field} was hidden from public decoding"
        );
        assert!(
            notice::lifecycle_hook_notices(store.connection(), &session).is_err(),
            "malformed {field} was accepted"
        );
        assert!(
            notice::read_notices(store.connection(), &session).is_err(),
            "malformed {field} was accepted by the combined reader"
        );
        assert!(
            notice::read_notice_at(store.connection(), &session, EventSeq::new(position.seq))
                .is_err(),
            "malformed {field} was accepted by the exact-row reader"
        );
        assert!(
            notice::skipped_kinds(store.connection(), &session)?.is_empty(),
            "malformed known kind was counted as a future kind"
        );
    }
    Ok(())
}

#[test]
fn writes_enforce_byte_bounds_before_appending_and_read_back_maximum_utf8_and_escaped_text(
) -> Result {
    let source = Source::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory()?;
    let session = setup(&mut store, &source)?;
    let execution = execution()?;
    for text in [
        String::new(),
        "nul\0inside".to_owned(),
        "x".repeat(notice::MAX_HOOK_RESULT_BYTES + 1),
        "é".repeat(notice::MAX_HOOK_RESULT_BYTES / 2 + 1),
    ] {
        assert!(notice::record_lifecycle_hook(
            &mut store,
            &authorities,
            &session,
            &execution,
            notice::LifecycleEvent::PreToolUse,
            &text
        )
        .is_err());
        assert_eq!(
            notice_count(&store, &session)?,
            0,
            "invalid payload left a durable notice"
        );
    }
    let mut recorded = Vec::new();
    for text in [
        "é".repeat(notice::MAX_HOOK_RESULT_BYTES / 2),
        // Worst-case JSON escaping must fit the notice owner's outer data cap.
        "\u{1}".repeat(notice::MAX_HOOK_RESULT_BYTES),
    ] {
        assert_eq!(text.len(), notice::MAX_HOOK_RESULT_BYTES);
        recorded.push(notice::record_lifecycle_hook(
            &mut store,
            &authorities,
            &session,
            &execution,
            notice::LifecycleEvent::PostToolUse,
            &text,
        )?);
    }
    assert_eq!(
        notice::lifecycle_hook_notices(store.connection(), &session)?,
        recorded
    );
    Ok(())
}

#[test]
fn lifecycle_results_survive_reopen_but_writes_require_the_already_elected_session_writer() -> Result
{
    let database = Database::new()?;
    let source = Source::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&database.0, "lifecycle-hook-witness")?;
    let session = setup(&mut store, &source)?;
    let execution = execution()?;
    let first = notice::record_lifecycle_hook(
        &mut store,
        &authorities,
        &session,
        &execution,
        notice::LifecycleEvent::SessionStart,
        "session constraints loaded",
    )?;
    drop(store);
    let mut store = EventStore::open(&database.0, "lifecycle-hook-witness")?;
    assert_eq!(
        notice::lifecycle_hook_notices(store.connection(), &session)?,
        std::slice::from_ref(&first)
    );
    assert!(
        notice::record_lifecycle_hook(
            &mut store,
            &authorities,
            &session,
            &execution,
            notice::LifecycleEvent::Stop,
            "completed"
        )
        .is_err(),
        "notice writer elected ownership for itself"
    );
    assert_eq!(notice_count(&store, &session)?, 1);
    let message: MessageId = "msg_hook_reopened_input".parse()?;
    assert!(
        notice::record_input_lifecycle_hook(
            &mut store,
            &authorities,
            &session,
            &message,
            notice::LifecycleEvent::PostToolUse,
            "input hook completed"
        )
        .is_err(),
        "input-scoped notice elected its own writer"
    );
    assert_eq!(notice_count(&store, &session)?, 1);
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let second = notice::record_lifecycle_hook(
        &mut store,
        &authorities,
        &session,
        &execution,
        notice::LifecycleEvent::Stop,
        "completed",
    )?;
    assert!(second.position.seq > first.position.seq);
    let input_notice = notice::record_input_lifecycle_hook(
        &mut store,
        &authorities,
        &session,
        &message,
        notice::LifecycleEvent::PostToolUse,
        "input hook completed",
    )?;
    assert_eq!(
        input_notice.scope,
        notice::LifecycleHookScope::Input(message)
    );
    drop(store);
    let store = EventStore::open(&database.0, "lifecycle-hook-witness")?;
    assert_eq!(
        notice::read_notices(store.connection(), &session)?.lifecycle_hook,
        [first, second, input_notice]
    );
    Ok(())
}

#[test]
fn hook_rows_are_public_and_future_kinds_are_skipped_and_counted_independently() -> Result {
    let source = Source::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory()?;
    let session = setup(&mut store, &source)?;
    let execution = execution()?;
    let first = notice::record_lifecycle_hook(
        &mut store,
        &authorities,
        &session,
        &execution,
        notice::LifecycleEvent::PreToolUse,
        "first call refused",
    )?;
    let mut unknowns = Vec::new();
    for _ in 0..2 {
        unknowns.push(append_wire(
            &mut store,
            &source,
            &session,
            &json!({
                "sessionID":session.as_str(), "executionID":execution.as_str(),
                "kind":"future-hook-metadata", "arbitraryFutureShape":{"allowed":true},
            }),
        )?);
    }
    let second = notice::record_lifecycle_hook(
        &mut store,
        &authorities,
        &session,
        &execution,
        notice::LifecycleEvent::PreToolUse,
        "second call refused",
    )?;
    let mcp = notice::record_mcp_unavailable(
        &mut store,
        &authorities,
        &session,
        &execution,
        vec![notice::Unavailable::new("fixture", "offline")],
    )?
    .ok_or("MCP notice missing")?;
    let expected_skips = vec![notice::SkippedKind {
        kind: "future-hook-metadata".into(),
        count: 2,
    }];
    let combined = notice::read_notices(store.connection(), &session)?;
    assert_eq!(combined.lifecycle_hook, [first.clone(), second.clone()]);
    assert_eq!(combined.mcp, std::slice::from_ref(&mcp));
    assert_eq!(combined.skipped, expected_skips);
    assert_eq!(
        notice::skipped_kinds(store.connection(), &session)?,
        expected_skips
    );
    for entry in [&first, &second] {
        assert!(public_at(&store, &session, entry.position.seq)?);
        let at = notice::read_notice_at(
            store.connection(),
            &session,
            EventSeq::new(entry.position.seq),
        )?;
        assert_eq!(at.lifecycle_hook, std::slice::from_ref(entry));
        assert!(at.mcp.is_empty());
        assert!(at.skipped.is_empty());
    }
    for position in unknowns {
        assert!(!public_at(&store, &session, position.seq)?);
        assert_eq!(
            notice::read_notice_at(store.connection(), &session, EventSeq::new(position.seq))?,
            notice::NoticeRead::default()
        );
    }
    let mcp_at = notice::read_notice_at(
        store.connection(),
        &session,
        EventSeq::new(mcp.position.seq),
    )?;
    assert_eq!(mcp_at.mcp, [mcp]);
    assert!(mcp_at.lifecycle_hook.is_empty());
    Ok(())
}

#[test]
fn exact_row_hook_reads_do_not_decode_another_row_or_another_session() -> Result {
    let source = Source::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory()?;
    let session = setup(&mut store, &source)?;
    let other = setup(&mut store, &source)?;
    let entry = notice::record_lifecycle_hook(
        &mut store,
        &authorities,
        &session,
        &execution()?,
        notice::LifecycleEvent::Notification,
        "waiting for user input",
    )?;
    let mut corrupt = wire(&session);
    corrupt["event"] = json!("UnknownFutureEvent");
    let corrupt = append_wire(&mut store, &source, &session, &corrupt)?;
    let mut other_wire = wire(&other);
    other_wire["event"] = json!("Stop");
    let other_position = append_wire(&mut store, &source, &other, &other_wire)?;
    assert!(notice::read_notices(store.connection(), &session).is_err());
    assert!(
        notice::read_notice_at(store.connection(), &session, EventSeq::new(corrupt.seq)).is_err()
    );
    assert_eq!(
        notice::read_notice_at(
            store.connection(),
            &session,
            EventSeq::new(entry.position.seq)
        )?
        .lifecycle_hook,
        [entry]
    );
    let other_read = notice::read_notice_at(
        store.connection(),
        &other,
        EventSeq::new(other_position.seq),
    )?;
    assert_eq!(other_read.lifecycle_hook.len(), 1);
    assert_eq!(
        other_read.lifecycle_hook[0].event,
        notice::LifecycleEvent::Stop
    );
    assert!(notice::read_notice_at(store.connection(), &session, EventSeq::new(u64::MAX)).is_err());
    assert!(notice::read_notice_at(
        store.connection(),
        &session,
        EventSeq::new(corrupt.seq + 100)
    )
    .is_err());
    Ok(())
}

#[test]
fn newest_hook_notices_are_bounded_but_exact_row_reads_can_retrieve_an_older_result() -> Result {
    let source = Source::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory()?;
    let session = setup(&mut store, &source)?;
    let execution = execution()?;
    let mut entries = Vec::new();
    for index in 0..notice::MAX_NOTICES + 2 {
        entries.push(notice::record_lifecycle_hook(
            &mut store,
            &authorities,
            &session,
            &execution,
            notice::LifecycleEvent::PreToolUse,
            &format!("call {index} refused"),
        )?);
    }
    let read = notice::lifecycle_hook_notices(store.connection(), &session)?;
    assert_eq!(read, entries[2..]);
    assert_eq!(
        notice::read_notices(store.connection(), &session)?.lifecycle_hook,
        read
    );
    assert_eq!(
        notice::read_notice_at(
            store.connection(),
            &session,
            EventSeq::new(entries[0].position.seq)
        )?
        .lifecycle_hook,
        [entries[0].clone()]
    );
    assert!(notice::skipped_kinds(store.connection(), &session)?.is_empty());
    Ok(())
}

#[test]
fn session_scoped_wait_survives_reopen_but_cannot_mask_execution_coordinates() -> Result {
    let database = Database::new()?;
    let source = Source::default();
    let mut store = EventStore::open(&database.0, "lifecycle-hook-witness")?;
    let session = setup(&mut store, &source)?;
    let recorded = notice::record_session_lifecycle_hook(
        &mut store,
        &DeterministicAuthorities::new(&source, &source),
        &session,
        notice::LifecycleEvent::WaitingForInput,
        "waiting for a Session form",
    )?;
    assert_eq!(recorded.scope, notice::LifecycleHookScope::Session);
    drop(store);
    let mut store = EventStore::open(&database.0, "lifecycle-hook-witness")?;
    assert_eq!(
        notice::lifecycle_hook_notices(store.connection(), &session)?,
        [recorded]
    );
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let mut contradictory = wire(&session);
    contradictory["sessionOnly"] = json!(true);
    let position = append_wire(&mut store, &source, &session, &contradictory)?;
    assert!(notice::lifecycle_hook_notices(store.connection(), &session).is_err());
    assert!(
        notice::read_notice_at(store.connection(), &session, EventSeq::new(position.seq)).is_err()
    );
    Ok(())
}
