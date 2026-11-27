//! A journal row this build cannot decode is skipped and named, a field a
//! later writer adds is read, and this build still writes the exact shape.
use grapt_instruction::{
    InstructionProjector, InstructionService, InstructionValue, RefreshInput, RefreshOutcome,
    SourceObservation,
};
use grapt_store::{AggregateId, AppendInput, EventStore};
use grapt_types::{EventVersion, InstructionKey, SessionId};
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn refresh(
    store: &mut EventStore,
    event: &str,
    session: &SessionId,
    value: &str,
) -> Result<RefreshOutcome, Box<dyn std::error::Error>> {
    let key = InstructionKey::new("core/base").ok_or("instruction key")?;
    Ok(InstructionService::refresh(
        store,
        RefreshInput {
            event: event.parse()?,
            session: session.clone(),
            created_ms: 1786147200000,
            required: vec![key.clone()],
            observations: vec![SourceObservation::Available {
                key,
                value: InstructionValue::Text(value.into()),
            }],
        },
    )?)
}

/// Append an instruction row. Without this build's projector it lands as
/// another writer would have left it.
fn append(
    store: &mut EventStore,
    event: &str,
    session: &SessionId,
    data: &Value,
    projected: bool,
) -> TestResult {
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let mut projector = InstructionProjector;
    let input = AppendInput {
        id: &event.parse()?,
        aggregate,
        writer,
        kind_name: "session.instructions.updated",
        version: EventVersion::new(2).ok_or("version")?,
        created_ms: 1786147200001,
        data,
    };
    let (_, notices) = if projected {
        store.append(input, &mut [&mut projector])?
    } else {
        store.append(input, &mut [])?
    };
    notices.discard();
    Ok(())
}

#[test]
fn an_undecodable_row_is_skipped_and_the_session_keeps_working() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    InstructionService::ensure_tables(&mut store)?;
    let session: SessionId = "ses_degraded_row".parse()?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    refresh(&mut store, "evt_degraded_first", &session, "one")?;
    let bad = json!({"sessionID": session.as_str(), "delta": {"core/base": 7}});
    append(&mut store, "evt_degraded_row", &session, &bad, false)?;

    let current = InstructionService::current(&store, &session)?;
    assert_eq!(current.skipped.len(), 1, "the bad row is named, not hidden");
    assert_eq!(current.values.ok_or("values")?[0].value, "one");
    assert!(matches!(
        refresh(&mut store, "evt_degraded_second", &session, "two")?,
        RefreshOutcome::Updated { .. }
    ));
    let history = InstructionService::history(&store, &session)?.ok_or("history")?;
    assert_eq!(history.skipped.len(), 1);
    let request = InstructionService::request_history(&store, &session)?.ok_or("request")?;
    assert_eq!(request.skipped.len(), 1);
    let values = InstructionService::current(&store, &session)?
        .values
        .ok_or("values")?;
    assert_eq!(values[0].value, "two");

    let later =
        json!({"sessionID": session.as_str(), "delta": {"core/base": "removed"}, "later": 1});
    assert!(
        append(&mut store, "evt_strict_write", &session, &later, true).is_err(),
        "this build writes exactly sessionID and delta"
    );
    append(&mut store, "evt_later_field", &session, &later, false)?;
    let current = InstructionService::current(&store, &session)?;
    assert_eq!(
        current.skipped.len(),
        1,
        "a later field is read, not skipped"
    );
    assert_eq!(current.values.ok_or("values")?.len(), 0);
    Ok(())
}

/// A store file with one Session, refreshed once per value, and a second raw
/// connection that can damage it the way another writer or a disk could.
fn file_store(
    label: &str,
    values: &[&str],
) -> Result<
    (
        EventStore,
        rusqlite::Connection,
        SessionId,
        std::path::PathBuf,
    ),
    Box<dyn std::error::Error>,
> {
    let path = std::env::temp_dir().join(format!("grapt-{label}-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut store = EventStore::open(&path, label)?;
    InstructionService::ensure_tables(&mut store)?;
    let session: SessionId = format!("ses_{}", label.replace('-', "_")).parse()?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    for (index, value) in values.iter().enumerate() {
        refresh(
            &mut store,
            &format!("evt_{}_{index}", label.replace('-', "_")),
            &session,
            value,
        )?;
    }
    let raw = rusqlite::Connection::open(&path)?;
    Ok((store, raw, session, path))
}

#[test]
fn rows_sqlite_cannot_read_as_text_are_skipped_and_named_even_when_none_remain() -> TestResult {
    let (store, raw, session, path) = file_store("unreadable-text", &["one"])?;
    raw.execute(
        "UPDATE event SET data = X'FF00FE' WHERE aggregate_id = ?1
           AND type LIKE 'session.instructions.updated.%'",
        [session.as_str()],
    )?;
    let history = InstructionService::history(&store, &session)?.ok_or("warnings were dropped")?;
    assert_eq!(history.skipped.len(), 1);
    assert!(history.initial.is_empty() && history.updates.is_empty());
    let request = InstructionService::request_history(&store, &session)?.ok_or("request")?;
    assert_eq!(request.skipped.len(), 1);
    drop((store, raw));
    std::fs::remove_file(&path)?;
    Ok(())
}

#[test]
fn a_cache_at_the_latest_sequence_with_stale_values_refuses_the_next_append() -> TestResult {
    let (mut store, raw, session, path) = file_store("stale-cache", &["one", "two"])?;
    let current: String = raw.query_row(
        "SELECT current_values FROM instruction_state WHERE session_id = ?1",
        [session.as_str()],
        |row| row.get(0),
    )?;
    let hashes: Vec<String> = raw
        .prepare("SELECT hash FROM instruction_blob")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let live = hashes
        .iter()
        .find(|hash| current.contains(hash.as_str()))
        .ok_or("live")?;
    let stale = hashes
        .iter()
        .find(|hash| !current.contains(hash.as_str()))
        .ok_or("stale")?;
    raw.execute(
        "UPDATE instruction_state SET current_values = replace(current_values, ?1, ?2)
           WHERE session_id = ?3",
        (live, stale, session.as_str()),
    )?;
    // Any writer's append runs the projector, not only refresh.
    let removal = json!({"sessionID": session.as_str(), "delta": {"core/base": "removed"}});
    assert!(
        append(&mut store, "evt_stale_cache_next", &session, &removal, true).is_err(),
        "an append trusted a cache whose values drifted"
    );
    drop((store, raw));
    std::fs::remove_file(&path)?;
    Ok(())
}

#[test]
fn updates_whose_blobs_are_missing_or_damaged_are_skipped_and_named() -> TestResult {
    let (store, raw, session, path) = file_store("damaged-blob", &["one", "two", "three"])?;
    raw.execute("DELETE FROM instruction_blob WHERE value = 'two'", [])?;
    raw.execute(
        "UPDATE instruction_blob SET value = X'FF00FE' WHERE value = 'three'",
        [],
    )?;
    let current = InstructionService::current(&store, &session)?;
    assert_eq!(current.skipped.len(), 2, "both damaged updates are named");
    assert_eq!(current.values.ok_or("values")?[0].value, "one");
    let history = InstructionService::history(&store, &session)?.ok_or("history")?;
    assert_eq!(history.skipped.len(), 2);
    let request = InstructionService::request_history(&store, &session)?.ok_or("request")?;
    assert_eq!(request.skipped.len(), 2);
    drop((store, raw));
    std::fs::remove_file(&path)?;
    Ok(())
}

#[test]
fn skipped_damaged_rows_still_spend_the_request_read_budget() -> TestResult {
    let (mut store, raw, session, path) = file_store("damaged-budget", &["one"])?;
    let hash: String = raw.query_row("SELECT hash FROM instruction_blob", [], |row| row.get(0))?;
    // One damaged blob of the largest readable size, named by many rows.
    raw.execute(
        "UPDATE instruction_blob SET value = printf('%.*c', 1048576, 'x') WHERE hash = ?1",
        [&hash],
    )?;
    for index in 0..70 {
        let data = json!({"sessionID": session.as_str(), "delta": {"core/base": hash}});
        append(
            &mut store,
            &format!("evt_budget_row_{index}"),
            &session,
            &data,
            false,
        )?;
    }
    let request = InstructionService::request_history(&store, &session);
    assert!(
        matches!(
            request,
            Err(grapt_instruction::InstructionError::EntryBound { .. })
        ),
        "70 MiB of damaged blobs were read past the 64 MiB budget: {request:?}"
    );
    drop((store, raw));
    std::fs::remove_file(&path)?;
    Ok(())
}
