use super::*;
use crate::selection::{select_agent, select_model};
use crate::{create_session, SessionSeed, SessionStoreError};
use grapt_store::EventStore;
use grapt_types::{ClockAuthority, IdAuthority, IdToken, ProjectId, Timestamp};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
struct FrozenClock(i64);

impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

#[derive(Debug, Default)]
struct CountingIds(AtomicU64);

impl IdAuthority for CountingIds {
    fn mint(&self) -> IdToken {
        let next = self.0.fetch_add(1, Ordering::SeqCst);
        match IdToken::new(format!("acp{next:012x}")) {
            Some(token) => token,
            None => panic!("the deterministic token must be non-empty"),
        }
    }
}

fn seed() -> Result<SessionSeed, Box<dyn std::error::Error>> {
    let project = ProjectId::new("global".to_owned()).ok_or("project identity is empty")?;
    Ok(SessionSeed::new(project, "/work/acp", "ACP parent")?
        .with_agent("build")?
        .with_model(serde_json::json!({"providerID":"p", "id":"m"}))?)
}

fn deletion_info(session: &SessionId) -> serde_json::Value {
    serde_json::json!({"sessionID":session.as_str(),"info":{
        "id":session.as_str(),"slug":"legacy","projectID":"global",
        "directory":"/work/acp","title":"legacy","version":"1",
        "time":{"created":42,"updated":42}}})
}

#[test]
fn legacy_v1_deletion_removes_exists_record_and_list() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let clock = FrozenClock(42);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut projector = SessionListProjector;
    let session =
        create_session(&mut store, &authorities, &seed()?, &mut [&mut projector])?.session;
    let event: EventId = format!("evt_{}", authorities.mint().as_str()).parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let data = deletion_info(&session);
    let (_appended, notice) = store.append(
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.deleted",
            version: EventVersion::new(1).ok_or("version")?,
            created_ms: 43,
            data: &data,
        },
        &mut [&mut projector],
    )?;
    notice.discard();
    assert!(
        !SessionStore::over(&store).exists(&session)?,
        "v1 deletion resurrected existence"
    );
    assert!(matches!(
        SessionStore::over(&store).record(&session),
        Err(SessionStoreError::Unknown { .. })
    ));
    let criteria = crate::SessionListCriteria::new(
        crate::SessionListScope::All,
        None,
        None,
        crate::ParentFilter::Any,
        crate::SessionListOrder::Descending,
        crate::SessionListDirection::Next,
        None,
        10,
        crate::SessionListLimits::production(),
    )?;
    assert!(
        crate::SessionListStore::over(&mut store)
            .list(&criteria)?
            .items()
            .is_empty(),
        "v1 deletion remained listed"
    );
    Ok(())
}

#[test]
fn malformed_v1_deletion_refuses_store_and_projector() -> Result<(), Box<dyn std::error::Error>> {
    for malformed in [
        serde_json::json!({"sessionID":"ses_placeholder"}),
        serde_json::json!({"sessionID":"ses_placeholder","info":{}}),
    ] {
        let mut store = EventStore::open_in_memory()?;
        let clock = FrozenClock(42);
        let ids = CountingIds::default();
        let authorities = DeterministicAuthorities::new(&clock, &ids);
        let mut projector = SessionListProjector;
        let session =
            create_session(&mut store, &authorities, &seed()?, &mut [&mut projector])?.session;
        let mut data = malformed;
        data["sessionID"] = serde_json::json!(session.as_str());
        append_row(
            &mut store,
            &authorities,
            &session,
            "session.deleted",
            1,
            data.clone(),
        )?;
        assert!(
            matches!(
                SessionStore::over(&store).exists(&session),
                Err(SessionStoreError::Corrupt { .. })
            ),
            "malformed v1 did not refuse existence"
        );
        assert!(
            matches!(
                SessionStore::over(&store).record(&session),
                Err(SessionStoreError::Corrupt { .. })
            ),
            "malformed v1 did not refuse record"
        );
        let event: EventId = format!("evt_{}", authorities.mint().as_str()).parse()?;
        let aggregate = AggregateId::Session(session.clone());
        let writer = store.fencing_token(&aggregate)?;
        assert!(
            store
                .append(
                    AppendInput {
                        id: &event,
                        aggregate,
                        writer,
                        kind_name: "session.deleted",
                        version: EventVersion::new(1).ok_or("version")?,
                        created_ms: 44,
                        data: &data
                    },
                    &mut [&mut projector]
                )
                .is_err(),
            "malformed v1 projected deletion"
        );
    }
    Ok(())
}

#[test]
fn fork_copies_owned_history_and_delete_cannot_resurrect() -> Result<(), Box<dyn std::error::Error>>
{
    let mut store = EventStore::open_in_memory()?;
    let clock = FrozenClock(42);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut projector = SessionListProjector;
    let parent = create_session(&mut store, &authorities, &seed()?, &mut [&mut projector])?.session;
    let agent = grapt_types::AgentId::new("agt_review").ok_or("agent identity is empty")?;
    select_agent(
        &mut store,
        &authorities,
        &parent,
        &agent,
        &mut [&mut projector],
    )?;
    select_model(
        &mut store,
        &authorities,
        &parent,
        &serde_json::json!({"providerID":"p", "id":"m2", "variant":"high"}),
        &mut [&mut projector],
    )?;

    let forked = fork_session(&mut store, &authorities, &parent, &mut [])?;
    assert_eq!(forked.parent, parent);
    assert_eq!(forked.copied_rows, 2);
    let child = SessionStore::over(&store).record(&forked.session)?;
    assert_eq!(child.parent.as_ref(), Some(&parent));
    assert_eq!(child.agent.as_deref(), Some("agt_review"));
    assert_eq!(
        child.model,
        Some(serde_json::json!({"providerID":"p", "id":"m2", "variant":"high"}))
    );
    let foreign_ids: i64 = store.connection().query_row(
        "SELECT count(*) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND json_extract(data, '$.sessionID') = ?2",
        rusqlite::params![forked.session.as_str(), parent.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(foreign_ids, 0, "forked rows retained the parent identity");

    delete_session(&mut store, &authorities, &forked.session)?;
    assert!(
        !SessionStore::over(&store).exists(&forked.session)?,
        "deleted session remained live in the durable fold"
    );
    assert!(matches!(
        SessionStore::over(&store).record(&forked.session),
        Err(SessionStoreError::Unknown { .. })
    ));
    let projected: i64 = store.connection().query_row(
        "SELECT count(*) FROM session_list_projection WHERE session_id = ?1",
        [forked.session.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(projected, 0, "delete left a list resurrection source");
    let before: i64 = store.connection().query_row(
        "SELECT count(*) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
        [forked.session.as_str()],
        |row| row.get(0),
    )?;
    assert!(delete_session(&mut store, &authorities, &forked.session).is_err());
    let after: i64 = store.connection().query_row(
        "SELECT count(*) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
        [forked.session.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(
        after, before,
        "a repeated delete appended a resurrection fact"
    );
    Ok(())
}

/// The frozen `session.forked` v2 shape names the parent sequence the fork
/// inherits through. Without it, every instruction read of the fork refuses
/// the event as corrupt and no prompt in the fork can execute.
#[test]
fn a_fork_records_the_parent_sequence_it_inherits_through() -> Result<(), Box<dyn std::error::Error>>
{
    let mut store = EventStore::open_in_memory()?;
    let clock = FrozenClock(42);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut projector = SessionListProjector;
    let parent = create_session(&mut store, &authorities, &seed()?, &mut [&mut projector])?.session;
    let agent = grapt_types::AgentId::new("agt_review").ok_or("agent identity is empty")?;
    select_agent(
        &mut store,
        &authorities,
        &parent,
        &agent,
        &mut [&mut projector],
    )?;
    let latest: i64 = store.connection().query_row(
        "SELECT MAX(seq) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
        [parent.as_str()],
        |row| row.get(0),
    )?;

    let forked = fork_session(&mut store, &authorities, &parent, &mut [])?;
    let data: String = store.connection().query_row(
        "SELECT data FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND type = 'session.forked.2'",
        [forked.session.as_str()],
        |row| row.get(0),
    )?;
    let data: serde_json::Value = serde_json::from_str(&data)?;
    assert_eq!(
        data["parentSeq"],
        serde_json::json!(latest),
        "FSE101 the fork does not name the parent sequence it inherits through"
    );
    assert_eq!(data["parentID"], serde_json::json!(parent.as_str()));
    Ok(())
}

/// Append one raw event row to `session`, shaped exactly as the product
/// writes it, so the fork witness exercises the durable rows themselves.
fn append_row(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    kind_name: &str,
    version: u32,
    data: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let event: EventId = format!("{}_{}", EventId::PREFIX, authorities.mint().as_str()).parse()?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let (appended, _notices) = store.append(
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name,
            version: EventVersion::new(version).ok_or("test event version is invalid")?,
            created_ms: authorities.now().as_millis(),
            data: &data,
        },
        &mut [],
    )?;
    match appended {
        Appended::Committed { .. } => Ok(()),
        Appended::Idempotent { .. } => Err("test appended the same event twice".into()),
    }
}

/// The durable rows of one session aggregate, oldest first.
fn rows_of(
    store: &EventStore,
    session: &SessionId,
    versioned: &str,
) -> Result<Vec<(i64, serde_json::Value)>, Box<dyn std::error::Error>> {
    let mut statement = store.connection().prepare(
        "SELECT seq, data FROM event
         WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND type = ?2
         ORDER BY seq",
    )?;
    let mapped = statement.query_map(rusqlite::params![session.as_str(), versioned], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut rows = Vec::new();
    for item in mapped {
        let (seq, raw) = item?;
        rows.push((seq, serde_json::from_str(&raw)?));
    }
    Ok(rows)
}

/// A fork carries the tool history its copied conversation claims: every
/// successful tool call keeps the identity manifest it declared before its
/// effect and its durable terminal outcome, re-bound to the fork's own
/// session, so the fork's first turn is not refused the parent's work. The
/// parent's live authority is never carried: its permission rows stay with
/// the parent.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_fork_carries_the_tool_history_its_conversation_claims(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let clock = FrozenClock(42);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut projector = SessionListProjector;
    let parent = create_session(&mut store, &authorities, &seed()?, &mut [&mut projector])?.session;

    let message: MessageId = "msg_848269f60ef0b8d224acfeeec0ad182cx23".parse()?;
    let call = "call_3f5e697c0c4582bc9cb4919940be0929x0";
    let attempt = "6202e858f9f0e530c620a1dbff7c04e2x9";

    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.input.admitted",
        1,
        json!({
            "input": {"data": {"text": "parent tool task"}, "delivery": "queue", "type": "user"},
            "inputID": message.as_str(),
            "sessionID": parent.as_str()
        }),
    )?;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.input.promoted",
        1,
        json!({"inputID": message.as_str(), "sessionID": parent.as_str()}),
    )?;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.assistant-message.opened",
        2,
        json!({
            "attempt": attempt,
            "message": message.as_str(),
            "selection": {"agent": "agt_live_headless1", "model": "fixture-model", "provider": "fixture-model"},
            "step": 1
        }),
    )?;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.content-block.opened",
        1,
        json!({"attempt": attempt, "block": 0, "kind": "tool", "message": message.as_str()}),
    )?;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.content.appended",
        1,
        json!({
            "item": {
                "call": call,
                "disposition": "complete",
                "kind": "tool",
                "ordinal": 0,
                "payload": {
                    "digest": "05745d70dfd2f0eb84dd1036e04122df76013f5d0d809fe2ad2eae4ec9a5d9c0",
                    "id": "04e1022847f0c1afc0762d973b61888455753c7df316c4eb7e11f304c8ce69fa",
                    "length": 19
                }
            },
            "message": message.as_str()
        }),
    )?;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.tool.input.started",
        1,
        json!({"assistantMessageID": message.as_str(), "callID": call, "name": "read", "sessionID": parent.as_str()}),
    )?;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.step.ended",
        3,
        json!({"completedAt": 42, "completion": "asserted-continue", "step": 1}),
    )?;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.tool.terminal",
        1,
        json!({
            "attempt": attempt,
            "call": call,
            "outcome": {
                "kind": "completed",
                "output": {
                    "digest": "a98523e4a5b2e191c71263a357183451965558aa6a32ad5683113da77fd8079d",
                    "disposition": "complete",
                    "id": "ffc3198cb4040907dba16b0cb06fb8cef29712212dd53872102d04a5c3231aa4",
                    "length": 54
                }
            }
        }),
    )?;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "permission.asked",
        2,
        json!({"action": "read", "id": "per_6202e858f9f0e530c620a1dbff7c04e2x14", "source": {"callID": call}}),
    )?;

    let parent_rows: i64 = store.connection().query_row(
        "SELECT count(*) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
        [parent.as_str()],
        |row| row.get(0),
    )?;

    let forked = fork_session(&mut store, &authorities, &parent, &mut [])?;
    let child = forked.session;

    let identities = rows_of(&store, &child, "session.tool.input.started.1")?;
    assert_eq!(
        identities.len(),
        1,
        "CPK102 the fork lost the tool identity manifest"
    );
    let identity = &identities[0].1;
    assert_eq!(
        identity["sessionID"],
        json!(child.as_str()),
        "CPK102 the identity manifest kept the parent session"
    );
    assert_eq!(identity["callID"], json!(call));
    assert_eq!(identity["name"], json!("read"));
    let assistant = rows_of(&store, &child, "session.assistant-message.opened.2")?;
    assert_eq!(assistant.len(), 1);
    assert_eq!(
        identity["assistantMessageID"], assistant[0].1["message"],
        "CPK102 the identity manifest names a message the fork does not carry"
    );

    let terminals = rows_of(&store, &child, "session.tool.terminal.1")?;
    assert_eq!(
        terminals.len(),
        1,
        "CPK101 the fork lost the durable terminal outcome"
    );
    let fact = grapt_event::session_tool_terminal::decode(&terminals[0].1)
        .map_err(|error| format!("CPK101 the copied terminal does not decode: {error}"))?;
    assert_eq!(
        fact.call().as_str(),
        call,
        "CPK101 the copied terminal names a foreign call"
    );
    assert!(
        terminals[0].0 > identities[0].0,
        "CPK101 the copied terminal precedes its identity manifest"
    );
    let parent_terminal = rows_of(&store, &parent, "session.tool.terminal.1")?;
    let parent_fact = grapt_event::session_tool_terminal::decode(&parent_terminal[0].1)?;
    assert_eq!(
        fact.outcome(),
        parent_fact.outcome(),
        "CPK101 the copied terminal outcome changed"
    );

    assert!(
        rows_of(&store, &child, "permission.asked.2")?.is_empty(),
        "the fork inherited the parent's permission"
    );
    let parent_after: i64 = store.connection().query_row(
        "SELECT count(*) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
        [parent.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(
        parent_after, parent_rows,
        "forking changed the parent's history"
    );
    Ok(())
}

/// The top-level tool terminals a continuation hands the model: the
/// predicate of `grapt-product`'s `retained_facts::model_terminals`, which
/// excludes a terminal when the SAME aggregate classifies its call as nested.
fn model_terminal_calls(
    store: &EventStore,
    session: &SessionId,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut statement = store.connection().prepare(
        "SELECT json_extract(e.data, '$.call') FROM event e
         WHERE e.aggregate_family = 'session' AND e.aggregate_id = ?1
           AND e.type = 'session.tool.terminal.1'
           AND NOT EXISTS (SELECT 1 FROM event n WHERE n.aggregate_family = 'session'
               AND n.aggregate_id = e.aggregate_id AND n.type = 'session.tool.nested.started.1'
               AND json_valid(n.data) AND json_valid(e.data)
               AND json_extract(n.data, '$.attempt') = json_extract(e.data, '$.attempt')
               AND json_extract(n.data, '$.call') = json_extract(e.data, '$.call'))
         ORDER BY e.seq",
    )?;
    let calls = statement
        .query_map([session.as_str()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(calls)
}

/// One top-level call and one nested call it made, as the runtime records
/// them: the nested call is classified by `session.tool.nested.started` and
/// both calls end in a `session.tool.terminal`.
fn append_tool_calls(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    attempt: &str,
    top: &str,
    nested: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let artifact = |id: &str| {
        json!({
            "digest": "05745d70dfd2f0eb84dd1036e04122df76013f5d0d809fe2ad2eae4ec9a5d9c0",
            "disposition": "complete",
            "id": id,
            "length": 19
        })
    };
    let terminal = |call: &str| {
        json!({
            "attempt": attempt,
            "call": call,
            "outcome": {"kind": "completed", "output": artifact("ffc3198cb4040907dba16b0cb06fb8cef29712212dd53872102d04a5c3231aa4")}
        })
    };
    let classification = json!({
        "attempt": attempt,
        "parent": top,
        "call": nested,
        "tool": "read",
        "catalog": "catalog-1",
        "input": artifact("04e1022847f0c1afc0762d973b61888455753c7df316c4eb7e11f304c8ce69fa")
    });
    grapt_event::session_tool_nested::decode(&classification)
        .map_err(|error| format!("test nested row is not product-shaped: {error}"))?;
    append_row(
        store,
        authorities,
        session,
        "session.tool.input.started",
        1,
        json!({"callID": top, "name": "codemode", "sessionID": session.as_str()}),
    )?;
    append_row(
        store,
        authorities,
        session,
        "session.tool.nested.started",
        1,
        classification,
    )?;
    append_row(
        store,
        authorities,
        session,
        "session.tool.terminal",
        1,
        terminal(nested),
    )?;
    append_row(
        store,
        authorities,
        session,
        "session.tool.terminal",
        1,
        terminal(top),
    )?;
    Ok(())
}

/// A fork keeps its copied calls classified exactly as the parent's: a
/// nested call's terminal stays nested, so the continuation hands the model
/// the same top-level results and finds every nested start resolved. Nothing
/// crosses from another session, even one reusing the same call ids, and no
/// permission row is carried. All of it holds after the store reopens.
#[test]
fn a_fork_keeps_nested_calls_nested_and_takes_nothing_from_other_sessions(
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!(
        "grapt-fork-nested-{}-{}",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir(&directory)?;
    let result = fork_keeps_nested_calls_nested(&directory.join("events.sqlite3"));
    let _ = std::fs::remove_dir_all(&directory);
    result
}

fn fork_keeps_nested_calls_nested(
    database: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open(database, "fork-nested-test")?;
    let clock = FrozenClock(42);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut projector = SessionListProjector;
    let parent = create_session(&mut store, &authorities, &seed()?, &mut [&mut projector])?.session;
    let foreign =
        create_session(&mut store, &authorities, &seed()?, &mut [&mut projector])?.session;
    let attempt = "6202e858f9f0e530c620a1dbff7c04e2x9";
    let top = "call_3f5e697c0c4582bc9cb4919940be0929x0";
    let nested = "call_3f5e697c0c4582bc9cb4919940be0929x1";

    append_tool_calls(&mut store, &authorities, &parent, attempt, top, nested)?;
    // Another session with the same call ids and its own live authority.
    append_tool_calls(&mut store, &authorities, &foreign, attempt, top, nested)?;
    append_row(
        &mut store,
        &authorities,
        &foreign,
        "permission.asked",
        2,
        json!({"action": "read", "id": "per_6202e858f9f0e530c620a1dbff7c04e2x14", "source": {"callID": top}}),
    )?;
    assert_eq!(model_terminal_calls(&store, &parent)?, vec![top.to_owned()]);

    let child = fork_session(&mut store, &authorities, &parent, &mut [])?.session;

    assert_eq!(
        model_terminal_calls(&store, &child)?,
        vec![top.to_owned()],
        "the fork hands the model a nested result as a top-level call"
    );
    let classified = rows_of(&store, &child, "session.tool.nested.started.1")?;
    assert_eq!(
        classified.len(),
        1,
        "the fork lost or duplicated the nested classification"
    );
    let fact = grapt_event::session_tool_nested::decode(&classified[0].1)?;
    assert_eq!(
        (fact.parent().as_str(), fact.call().as_str()),
        (top, nested)
    );
    let terminals = rows_of(&store, &child, "session.tool.terminal.1")?;
    assert_eq!(
        terminals.len(),
        2,
        "the fork copied terminals from another session"
    );
    assert!(
        terminals
            .iter()
            .any(|(seq, data)| { data["call"] == json!(nested) && *seq > classified[0].0 }),
        "the nested start is unresolved in the fork"
    );
    assert_eq!(
        rows_of(&store, &child, "session.tool.input.started.1")?.len(),
        1
    );
    assert!(rows_of(&store, &child, "permission.asked.2")?.is_empty());
    for kind in ["session.tool.nested.started.1", "session.tool.terminal.1"] {
        assert_eq!(
            rows_of(&store, &foreign, kind)?.len(),
            rows_of(&store, &parent, kind)?.len(),
            "forking changed another session's {kind} rows"
        );
    }

    drop(store);
    let store = EventStore::open(database, "fork-nested-test")?;
    assert_eq!(
        model_terminal_calls(&store, &child)?,
        vec![top.to_owned()],
        "after a restart the fork hands the model a nested result"
    );
    assert_eq!(
        rows_of(&store, &child, "session.tool.nested.started.1")?.len(),
        1
    );
    assert_eq!(rows_of(&store, &child, "session.tool.terminal.1")?.len(), 2);
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn selected_fork_refuses_unfinished_hidden_and_foreign_prefixes_without_writing(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let clock = FrozenClock(42);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let parent = create_session(&mut store, &authorities, &seed()?, &mut [])?.session;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.input.admitted",
        1,
        json!({"sessionID":parent.as_str(),"inputID":"msg_cutoff_prompt", "input":{"type":"user","data":{"text":"cutoff prompt"},"delivery":"queue"}}),
    )?;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.input.promoted",
        1,
        json!({"sessionID":parent.as_str(),"inputID":"msg_cutoff_prompt"}),
    )?;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.assistant-message.opened",
        1,
        json!({"message":"msg_cutoff", "attempt":"attempt_cutoff", "step":1}),
    )?;
    let opened = rows_of(&store, &parent, "session.assistant-message.opened.1")?[0].0;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.step.ended",
        2,
        json!({"step":1,"completion":"asserted-continue"}),
    )?;
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.tool.input.started",
        1,
        json!({"sessionID":parent.as_str(),"assistantMessageID":"msg_cutoff", "callID":"call_cutoff", "name":"shell"}),
    )?;
    let started = rows_of(&store, &parent, "session.tool.input.started.1")?[0].0;
    let before: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    for cutoff in [opened, started] {
        assert!(matches!(
            fork_session_at_seq(
                &mut store,
                &authorities,
                &parent,
                EventSeq::new(u64::try_from(cutoff)?),
                &mut []
            ),
            Err(SessionLifecycleError::UnfinishedCutoff)
        ));
    }
    assert!(matches!(
        fork_session_at_seq(
            &mut store,
            &authorities,
            &parent,
            EventSeq::new(u64::MAX),
            &mut []
        ),
        Err(SessionLifecycleError::InvalidCutoff)
    ));
    let other = create_session(&mut store, &authorities, &seed()?, &mut [])?.session;
    assert!(matches!(
        fork_session_at_seq(
            &mut store,
            &authorities,
            &other,
            EventSeq::new(u64::try_from(opened)?),
            &mut []
        ),
        Err(SessionLifecycleError::InvalidCutoff)
    ));
    // The failed requests created no child rows: only the explicit other seed
    // and its selections account for the difference.
    let count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_id != ?1",
        [other.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(count, before);
    append_row(
        &mut store,
        &authorities,
        &parent,
        "session.revert.committed",
        1,
        json!({"sessionID":parent.as_str(),"to":"msg_cutoff_prompt"}),
    )?;
    assert!(matches!(
        fork_session_at_seq(
            &mut store,
            &authorities,
            &parent,
            EventSeq::new(u64::try_from(opened)?),
            &mut []
        ),
        Err(SessionLifecycleError::InvalidCutoff)
    ));
    let count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_id != ?1",
        [other.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(count, before + 1);

    Ok(())
}
