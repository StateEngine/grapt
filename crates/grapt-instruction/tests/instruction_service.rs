use grapt_instruction::{
    EntryOutcome, InstructionBoundary, InstructionChange, InstructionError, InstructionProjector,
    InstructionService, InstructionValue, RefreshInput, RefreshOutcome, SourceObservation,
};
use grapt_store::{AggregateId, AppendInput, Appended, EventStore};
use grapt_types::{EventId, EventSeq, EventVersion, InstructionKey, SessionId};
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn entry_bounds_reject_oversize_persisted_values_before_allocating_or_replacing() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_entry_bound".parse()?;
    let key = key("context")?;
    InstructionService::ensure_tables(&mut store)?;
    InstructionService::put_entry(&mut store, &session, &key, "first", 1)?;
    assert!(matches!(
        InstructionService::put_entry(&mut store, &session, &key, &"x".repeat(65_537), 2),
        Err(InstructionError::EntryBound { .. })
    ));
    assert_eq!(
        InstructionService::entries(&store, &session)?[0].value,
        "first"
    );
    let tx = store.governed_transaction()?;
    tx.execute(
        "UPDATE instruction_entry SET value = ?1 WHERE session_id = ?2",
        rusqlite::params!["x".repeat(65_537), session.as_str()],
    )?;
    tx.commit()?.discard();
    assert!(matches!(
        InstructionService::entries(&store, &session),
        Err(InstructionError::EntryBound { .. })
    ));
    assert!(matches!(
        InstructionService::put_entry(&mut store, &session, &key, "hide-corruption", 3),
        Err(InstructionError::EntryBound { .. })
    ));
    Ok(())
}

fn key(value: &str) -> Result<InstructionKey, Box<dyn std::error::Error>> {
    InstructionKey::new(value).ok_or_else(|| "instruction key is empty".into())
}

fn available(key: &InstructionKey, value: &str) -> SourceObservation {
    SourceObservation::Available {
        key: key.clone(),
        value: InstructionValue::Text(value.into()),
    }
}

/// Elects this store instance as the writer for `session`.
///
/// Election is explicit because the append path never elects on a caller's
/// behalf: a fixture that skipped this would be refused as an unelected writer
/// rather than silently granted the aggregate's first epoch. Re-electing the
/// same session on the same store is retained, so calling this once per session
/// in setup is enough however many appends follow.
fn elect(store: &mut EventStore, session: &SessionId) -> Result<(), Box<dyn std::error::Error>> {
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    Ok(())
}

fn refresh(
    store: &mut EventStore,
    event: &str,
    session: &SessionId,
    required: Vec<InstructionKey>,
    observations: Vec<SourceObservation>,
) -> Result<RefreshOutcome, InstructionError> {
    InstructionService::refresh(
        store,
        RefreshInput {
            event: event
                .parse()
                .map_err(|source| InstructionError::Corruption {
                    reason: format!("test event id: {source}"),
                })?,
            session: session.clone(),
            created_ms: 1786147200000,
            required,
            observations,
        },
    )
}

fn append(
    store: &mut EventStore,
    event: &str,
    session: &SessionId,
    kind: &str,
    data_session: &SessionId,
) -> Result<EventSeq, Box<dyn std::error::Error>> {
    let data = match kind {
        "session.compaction.ended" => json!({
            "sessionID": data_session.as_str(),
            "reason": "auto",
            "text": "compacted",
            "recent": "recent"
        }),
        "session.moved" => json!({
            "sessionID": data_session.as_str(),
            "location": {"directory": "C:/workspace"}
        }),
        "session.revert.committed" => json!({
            "sessionID": data_session.as_str(),
            "to": "msg_revert_boundary"
        }),
        _ => json!({"sessionID": data_session.as_str()}),
    };
    append_versioned(store, event, session, kind, 1, &data)
}

fn append_versioned(
    store: &mut EventStore,
    event: &str,
    session: &SessionId,
    kind: &str,
    version: u32,
    data: &Value,
) -> Result<EventSeq, Box<dyn std::error::Error>> {
    let id: EventId = event.parse()?;
    let version = EventVersion::new(version).ok_or("version")?;
    let mut projector = InstructionProjector;
    // Test setup elects explicitly, exactly as a runtime owner would. Nothing
    // in the append path does it, so a fixture that skipped this would be
    // refused rather than silently self-elected.
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let (appended, notices) = store.append(
        AppendInput {
            id: &id,
            aggregate,
            writer,
            kind_name: kind,
            version,
            created_ms: 1786147200001,
            data,
        },
        &mut [&mut projector],
    )?;
    // This fixture asserts durable projection state, not delivery.
    notices.discard();
    Ok(match appended {
        Appended::Committed { seq } | Appended::Idempotent { seq } => EventSeq::new(seq),
    })
}

fn count(store: &EventStore, table: &str) -> Result<i64, rusqlite::Error> {
    store
        .connection()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn initial_unavailable_blocks_then_updates_retain_unavailable_sources() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    InstructionService::ensure_tables(&mut store)?;
    let session: SessionId = "ses_instruction_a".parse()?;
    elect(&mut store, &session)?;
    let global = key("global/agents")?;
    let project = key("project/agents")?;
    let guidance = key("session/guidance")?;

    let blocked = refresh(
        &mut store,
        "evt_instruction_blocked",
        &session,
        vec![global.clone(), project.clone()],
        vec![
            available(&global, "global-v1"),
            SourceObservation::Unavailable {
                key: project.clone(),
            },
        ],
    );
    assert!(matches!(
        blocked,
        Err(InstructionError::InitialIncomplete { .. })
    ));
    assert_eq!(count(&store, "event")?, 0);
    assert_eq!(count(&store, "instruction_blob")?, 0);
    assert_eq!(count(&store, "instruction_state")?, 0);

    assert_eq!(
        refresh(
            &mut store,
            "evt_instruction_initial",
            &session,
            vec![global.clone(), project.clone()],
            vec![
                available(&global, "global-v1"),
                SourceObservation::Available {
                    key: project.clone(),
                    value: InstructionValue::Structured(json!({"b": 2, "a": 1})),
                },
            ],
        )?,
        RefreshOutcome::Updated {
            seq: EventSeq::FIRST,
            changed: 2
        }
    );
    assert_eq!(count(&store, "instruction_blob")?, 2);

    assert_eq!(
        refresh(
            &mut store,
            "evt_instruction_update",
            &session,
            vec![],
            vec![
                SourceObservation::Unavailable {
                    key: global.clone(),
                },
                available(&project, "project-v2"),
                available(&guidance, "guidance-v1"),
            ],
        )?,
        RefreshOutcome::Updated {
            seq: EventSeq::new(1),
            changed: 2
        }
    );
    assert_eq!(
        refresh(
            &mut store,
            "evt_instruction_noop",
            &session,
            vec![],
            vec![
                SourceObservation::Unavailable {
                    key: global.clone(),
                },
                available(&project, "project-v2"),
            ],
        )?,
        RefreshOutcome::Unchanged
    );
    assert_eq!(count(&store, "event")?, 2, "no-op emits no durable delta");

    let history = InstructionService::history(&store, &session)?.ok_or("history")?;
    assert_eq!(history.initial.len(), 2);
    assert!(history
        .initial
        .iter()
        .any(|instruction| instruction.key == global && instruction.value == "global-v1"));
    assert_eq!(history.updates.len(), 1);
    assert_eq!(history.updates[0].changes.len(), 2);
    assert!(history.updates[0]
        .changes
        .iter()
        .all(|change| match change {
            InstructionChange::Value(instruction) => instruction.key != global,
            InstructionChange::Removed { .. } => false,
        }));
    let delta: String = store.connection().query_row(
        "SELECT data FROM event WHERE id = 'evt_instruction_update'",
        [],
        |row| row.get(0),
    )?;
    assert!(
        !delta.contains(global.as_str()),
        "unavailable source is retained, not repeated"
    );
    Ok(())
}

#[test]
fn compaction_moves_the_epoch_and_move_or_committed_revert_clear_it() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    InstructionService::ensure_tables(&mut store)?;
    let session: SessionId = "ses_instruction_epoch".parse()?;
    elect(&mut store, &session)?;
    let source = key("project/agents")?;

    refresh(
        &mut store,
        "evt_epoch_initial",
        &session,
        vec![source.clone()],
        vec![available(&source, "v1")],
    )?;
    refresh(
        &mut store,
        "evt_epoch_update",
        &session,
        vec![],
        vec![available(&source, "v2")],
    )?;
    let compaction = append(
        &mut store,
        "evt_epoch_compaction",
        &session,
        InstructionBoundary::CompactionEnded.kind(),
        &session,
    )?;
    let compacted = InstructionService::history(&store, &session)?.ok_or("compacted")?;
    assert_eq!(compacted.epoch.value(), compaction.value());
    assert_eq!(compacted.initial[0].value, "v2");
    assert!(compacted.updates.is_empty());

    refresh(
        &mut store,
        "evt_epoch_after",
        &session,
        vec![],
        vec![available(&source, "v3")],
    )?;
    let before_rebuild = InstructionService::history(&store, &session)?.ok_or("history")?;
    assert_eq!(before_rebuild.updates.len(), 1);
    store.connection().execute(
        "DELETE FROM instruction_state WHERE session_id = ?1",
        [session.as_str()],
    )?;
    assert_eq!(
        InstructionService::history(&store, &session)?.ok_or("history from log")?,
        before_rebuild,
        "history derives from the durable log even when its cache is absent"
    );
    assert_eq!(
        InstructionService::rebuild(&mut store, &session)?.ok_or("rebuilt")?,
        before_rebuild
    );

    append(
        &mut store,
        "evt_epoch_revert_staged",
        &session,
        "session.revert.staged",
        &session,
    )?;
    assert!(InstructionService::history(&store, &session)?.is_some());
    append(
        &mut store,
        "evt_epoch_revert_committed",
        &session,
        InstructionBoundary::RevertCommitted.kind(),
        &session,
    )?;
    assert!(InstructionService::history(&store, &session)?.is_none());
    assert!(matches!(
        refresh(
            &mut store,
            "evt_epoch_incomplete",
            &session,
            vec![source.clone()],
            vec![SourceObservation::Unavailable {
                key: source.clone()
            }],
        ),
        Err(InstructionError::InitialIncomplete { .. })
    ));
    refresh(
        &mut store,
        "evt_epoch_new_initial",
        &session,
        vec![source.clone()],
        vec![available(&source, "v4")],
    )?;
    append(
        &mut store,
        "evt_epoch_moved",
        &session,
        InstructionBoundary::SessionMoved.kind(),
        &session,
    )?;
    assert!(InstructionService::history(&store, &session)?.is_none());
    Ok(())
}

#[test]
fn cache_corruption_fails_closed_and_blob_damage_is_named() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    InstructionService::ensure_tables(&mut store)?;
    let session: SessionId = "ses_instruction_rebuild".parse()?;
    elect(&mut store, &session)?;
    let source = key("core/environment")?;
    refresh(
        &mut store,
        "evt_rebuild_initial",
        &session,
        vec![source.clone()],
        vec![available(&source, "clean")],
    )?;

    store.connection().execute(
        "UPDATE instruction_state SET current_values = '{\"bad\":7}' WHERE session_id = ?1",
        [session.as_str()],
    )?;
    assert!(InstructionService::history(&store, &session).is_err());
    let rebuilt = InstructionService::rebuild(&mut store, &session)?.ok_or("rebuilt")?;
    assert_eq!(rebuilt.initial[0].value, "clean");

    store.connection().execute(
        "UPDATE instruction_state SET current_values = '{}' WHERE session_id = ?1",
        [session.as_str()],
    )?;
    assert!(
        InstructionService::history(&store, &session).is_err(),
        "a valid hash-map shape that diverges from epoch history is corrupt"
    );
    InstructionService::rebuild(&mut store, &session)?.ok_or("rebuilt divergence")?;

    store.connection().execute(
        "UPDATE instruction_state SET initial_values = '{}', current_values = '{}'
         WHERE session_id = ?1",
        [session.as_str()],
    )?;
    assert!(
        InstructionService::history(&store, &session).is_err(),
        "coherent cache corruption is compared with an independent durable fold"
    );
    let events_before_corruption = count(&store, "event")?;
    assert!(refresh(
        &mut store,
        "evt_rebuild_corrupt_refresh",
        &session,
        vec![],
        vec![available(&source, "clean")],
    )
    .is_err());
    assert_eq!(count(&store, "event")?, events_before_corruption);
    InstructionService::rebuild(&mut store, &session)?.ok_or("rebuilt coherent corruption")?;

    store.connection().execute(
        "DELETE FROM instruction_state WHERE session_id = ?1",
        [session.as_str()],
    )?;
    assert_eq!(
        refresh(
            &mut store,
            "evt_rebuild_new_noop",
            &session,
            vec![source.clone()],
            vec![SourceObservation::Unavailable {
                key: source.clone()
            }],
        )?,
        RefreshOutcome::Unchanged,
        "a new command rebuilds missing cache and retains unavailable durable truth"
    );
    assert_eq!(count(&store, "event")?, events_before_corruption);
    assert_eq!(count(&store, "instruction_state")?, 1);
    assert!(InstructionService::history(&store, &session)?.is_some());

    // A blob that is not its content damages the rows naming it, not the
    // Session: they are skipped and named, never folded as truth.
    store
        .connection()
        .execute("UPDATE instruction_blob SET value = 'tampered'", [])?;
    let history = InstructionService::history(&store, &session)?.ok_or("named")?;
    assert!(!history.skipped.is_empty(), "the tampered rows are named");
    assert!(history
        .initial
        .iter()
        .all(|instruction| instruction.value != "tampered"));
    Ok(())
}

#[test]
fn entries_are_session_owned_and_strictly_decoded_before_mutation() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    InstructionService::ensure_tables(&mut store)?;
    let session: SessionId = "ses_instruction_entry".parse()?;
    let source = key("user/guidance")?;
    assert_eq!(
        InstructionService::put_entry(&mut store, &session, &source, "first", 10)?,
        EntryOutcome::Applied
    );
    assert_eq!(
        InstructionService::put_entry(&mut store, &session, &source, "first", 11)?,
        EntryOutcome::Unchanged
    );
    assert_eq!(
        InstructionService::put_entry(&mut store, &session, &source, "second", 12)?,
        EntryOutcome::Applied
    );
    assert!(matches!(
        InstructionService::put_entry(&mut store, &session, &source, "regressed", 11),
        Err(InstructionError::TimeRegression { .. })
    ));
    assert_eq!(
        InstructionService::entries(&store, &session)?[0].value,
        "second"
    );
    assert_eq!(
        InstructionService::remove_entry(&mut store, &session, &source, 13)?,
        EntryOutcome::Applied
    );
    assert!(InstructionService::entries(&store, &session)?.is_empty());

    store
        .connection()
        .execute_batch("PRAGMA ignore_check_constraints = ON")?;
    store.connection().execute(
        "UPDATE instruction_entry
         SET value = 'impossible', removed = 2
         WHERE session_id = ?1 AND key = ?2",
        rusqlite::params![session.as_str(), source.as_str()],
    )?;
    assert!(InstructionService::entries(&store, &session).is_err());
    assert!(InstructionService::put_entry(&mut store, &session, &source, "third", 14).is_err());
    let row: (String, i64, i64) = store.connection().query_row(
        "SELECT value, removed, time_updated FROM instruction_entry
         WHERE session_id = ?1 AND key = ?2",
        rusqlite::params![session.as_str(), source.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(row, ("impossible".into(), 2, 13), "refusal writes nothing");
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn fork_history_is_capped_at_parent_sequence_then_owned_by_the_child() -> TestResult {
    // File-backed because this fixture must rewind the child's cursor, and
    // `event_sequence` is a GOVERNED table: the store's own connection carries
    // the write chokepoint's denial and refuses that UPDATE at statement
    // preparation. The rewind is staged from a second connection instead, which
    // is the only place such a write can now come from.
    let dir = std::env::temp_dir().join(format!("grapt-instruction-fork-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("instruction.db");
    let mut store = EventStore::open(&db, "test")?;
    InstructionService::ensure_tables(&mut store)?;
    let parent: SessionId = "ses_instruction_parent".parse()?;
    let child: SessionId = "ses_instruction_child".parse()?;
    // Each aggregate is elected on its own: electing the parent grants nothing
    // over the child, and both of them are written here.
    elect(&mut store, &parent)?;
    elect(&mut store, &child)?;
    let source = key("project/agents")?;

    refresh(
        &mut store,
        "evt_parent_initial",
        &parent,
        vec![source.clone()],
        vec![available(&source, "parent-v1")],
    )?;
    refresh(
        &mut store,
        "evt_parent_second",
        &parent,
        vec![],
        vec![available(&source, "parent-v2")],
    )?;
    refresh(
        &mut store,
        "evt_parent_after_fork_boundary",
        &parent,
        vec![],
        vec![available(&source, "parent-v3")],
    )?;

    append_versioned(
        &mut store,
        "evt_child_forked",
        &child,
        "session.forked",
        2,
        &json!({
            "sessionID": child.as_str(),
            "parentID": parent.as_str(),
            "parentSeq": 1,
            "from": "msg_fork_boundary"
        }),
    )?;
    rusqlite::Connection::open(&db)?.execute(
        "UPDATE event_sequence SET seq = 1 WHERE aggregate_id = ?1",
        [child.as_str()],
    )?;

    let inherited = InstructionService::history(&store, &child)?.ok_or("inherited history")?;
    let request = InstructionService::request_history(&store, &child)?.ok_or("request history")?;
    assert_eq!(request.initial[0].value, "parent-v2");
    assert!(
        request.updates.is_empty(),
        "request inherited foreign positions"
    );
    assert_eq!(inherited.initial[0].value, "parent-v1");
    assert_eq!(inherited.updates.len(), 1);
    assert!(matches!(
        &inherited.updates[0].changes[0],
        InstructionChange::Value(value) if value.value == "parent-v2"
    ));

    refresh(
        &mut store,
        "evt_child_owned_update",
        &child,
        vec![],
        vec![available(&source, "child-v1")],
    )?;
    refresh(
        &mut store,
        "evt_parent_later",
        &parent,
        vec![],
        vec![available(&source, "parent-v4")],
    )?;

    let child_history = InstructionService::history(&store, &child)?.ok_or("child history")?;
    assert_eq!(child_history.initial[0].value, "parent-v1");
    assert_eq!(child_history.updates.len(), 2);
    assert!(matches!(
        &child_history.updates[1].changes[0],
        InstructionChange::Value(value) if value.value == "child-v1"
    ));
    assert!(child_history.updates.iter().all(|update| {
        update.changes.iter().all(|change| match change {
            InstructionChange::Value(value) => {
                value.value != "parent-v3" && value.value != "parent-v4"
            }
            InstructionChange::Removed { .. } => true,
        })
    }));

    store.connection().execute(
        "DELETE FROM instruction_state WHERE session_id = ?1",
        [child.as_str()],
    )?;
    assert_eq!(
        InstructionService::rebuild(&mut store, &child)?.ok_or("rebuilt child")?,
        child_history
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn malformed_or_unsupported_boundaries_fail_before_cache_mutation() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    InstructionService::ensure_tables(&mut store)?;
    let session: SessionId = "ses_instruction_strict_boundary".parse()?;
    let child: SessionId = "ses_instruction_bad_fork".parse()?;
    // Both aggregates are elected so that every refusal below comes from the
    // boundary decoder rather than from an unelected writer.
    elect(&mut store, &session)?;
    elect(&mut store, &child)?;
    let source = key("core/date")?;
    refresh(
        &mut store,
        "evt_strict_initial",
        &session,
        vec![source.clone()],
        vec![available(&source, "today")],
    )?;
    let history = InstructionService::history(&store, &session)?.ok_or("history")?;
    let events = count(&store, "event")?;

    assert!(append_versioned(
        &mut store,
        "evt_bad_compaction",
        &session,
        "session.compaction.ended",
        1,
        &json!({"sessionID": session.as_str()}),
    )
    .is_err());
    assert!(append_versioned(
        &mut store,
        "evt_bad_move_version",
        &session,
        "session.moved",
        2,
        &json!({
            "sessionID": session.as_str(),
            "location": {"directory": "C:/workspace"}
        }),
    )
    .is_err());
    assert!(append_versioned(
        &mut store,
        "evt_bad_revert",
        &session,
        "session.revert.committed",
        1,
        &json!({"sessionID": session.as_str()}),
    )
    .is_err());
    assert!(append_versioned(
        &mut store,
        "evt_bad_fork_shape",
        &child,
        "session.forked",
        2,
        &json!({
            "sessionID": child.as_str(),
            "parentID": session.as_str()
        }),
    )
    .is_err());
    assert!(append_versioned(
        &mut store,
        "evt_bad_fork_version",
        &child,
        "session.forked",
        1,
        &json!({
            "sessionID": child.as_str(),
            "parentID": session.as_str(),
            "parentSeq": 0
        }),
    )
    .is_err());

    assert_eq!(count(&store, "event")?, events);
    assert_eq!(
        InstructionService::history(&store, &session)?.ok_or("unchanged history")?,
        history
    );
    assert!(InstructionService::history(&store, &child)?.is_none());
    Ok(())
}

#[test]
fn wrong_session_boundary_rolls_back_without_clearing_the_epoch() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    InstructionService::ensure_tables(&mut store)?;
    let session: SessionId = "ses_instruction_owner".parse()?;
    let other: SessionId = "ses_instruction_other".parse()?;
    // The written aggregate is elected, so the refusal below is attributable to
    // the wrong-session boundary payload and not to a missing election. `other`
    // is never appended to — it only names a foreign session inside the data.
    elect(&mut store, &session)?;
    let source = key("core/date")?;
    refresh(
        &mut store,
        "evt_boundary_initial",
        &session,
        vec![source.clone()],
        vec![available(&source, "today")],
    )?;
    let events_before = count(&store, "event")?;
    assert!(append(
        &mut store,
        "evt_boundary_wrong",
        &session,
        InstructionBoundary::SessionMoved.kind(),
        &other,
    )
    .is_err());
    assert_eq!(count(&store, "event")?, events_before);
    assert!(InstructionService::history(&store, &session)?.is_some());
    Ok(())
}

#[test]
fn bounded_request_history_matches_truth_without_writes_and_resets_at_compaction() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_request_epoch".parse()?;
    let source = key("project/agents")?;
    elect(&mut store, &session)?;
    refresh(
        &mut store,
        "evt_request_initial",
        &session,
        vec![source.clone()],
        vec![available(&source, "first")],
    )?;
    refresh(
        &mut store,
        "evt_request_changed",
        &session,
        vec![],
        vec![available(&source, "second")],
    )?;
    let before = store.connection().total_changes();
    let bounded =
        InstructionService::request_history(&store, &session)?.ok_or("bounded history missing")?;
    assert_eq!(
        bounded,
        InstructionService::history(&store, &session)?.ok_or("truth missing")?
    );
    assert_eq!(bounded.initial[0].value, "first");
    assert_eq!(bounded.updates.len(), 1);
    assert_eq!(
        store.connection().total_changes(),
        before,
        "request projection must be read-only"
    );
    append(
        &mut store,
        "evt_request_compact",
        &session,
        InstructionBoundary::CompactionEnded.kind(),
        &session,
    )?;
    let reset = InstructionService::request_history(&store, &session)?
        .ok_or("compacted history missing")?;
    assert_eq!(reset.initial[0].value, "second");
    assert!(reset.updates.is_empty());
    let tx = store.governed_transaction()?;
    tx.execute(
        "UPDATE instruction_blob SET value = ?1",
        ["x".repeat(1_048_577)],
    )?;
    tx.commit()?.discard();
    assert!(matches!(
        InstructionService::request_history(&store, &session),
        Err(InstructionError::EntryBound { .. })
    ));
    Ok(())
}

#[test]
fn an_oversized_resolved_epoch_becomes_readable_after_ordinary_compaction() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_request_bound_reset".parse()?;
    let source = key("project/large")?;
    elect(&mut store, &session)?;
    for index in 0..5 {
        let value = format!("v{index}{}", "x".repeat(949_998));
        refresh(
            &mut store,
            &format!("evt_request_bound_{index}"),
            &session,
            vec![source.clone()],
            vec![available(&source, &value)],
        )?;
    }
    assert!(matches!(
        InstructionService::request_history(&store, &session),
        Err(InstructionError::EntryBound {
            bound: "instruction epoch request bytes"
        })
    ));
    append(
        &mut store,
        "evt_request_bound_compact",
        &session,
        InstructionBoundary::CompactionEnded.kind(),
        &session,
    )?;
    let compacted = InstructionService::request_history(&store, &session)?
        .ok_or("compacted request history missing")?;
    assert!(compacted.initial[0].value.starts_with("v4"));
    assert!(compacted.updates.is_empty());
    Ok(())
}

#[test]
fn a_first_value_source_survives_compaction_rebuild_and_failed_refresh() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_first_value".parse()?;
    elect(&mut store, &session)?;
    let source = key("core/frozen-catalog")?;
    let observed = |value: &str| SourceObservation::AvailableOnce {
        key: source.clone(),
        value: InstructionValue::Text(value.into()),
    };
    refresh(
        &mut store,
        "evt_first_value_a",
        &session,
        vec![],
        vec![observed("original")],
    )?;
    let initial = InstructionService::history(&store, &session)?.ok_or("history")?;
    refresh(
        &mut store,
        "evt_first_value_b",
        &session,
        vec![],
        vec![observed("replacement")],
    )?;
    assert_eq!(
        InstructionService::history(&store, &session)?.ok_or("history")?,
        initial
    );
    append(
        &mut store,
        "evt_first_value_compact",
        &session,
        "session.compaction.ended",
        &session,
    )?;
    InstructionService::rebuild(&mut store, &session)?;
    refresh(
        &mut store,
        "evt_first_value_c",
        &session,
        vec![],
        vec![observed("replacement")],
    )?;
    refresh(
        &mut store,
        "evt_first_value_d",
        &session,
        vec![],
        vec![SourceObservation::Unavailable { key: source }],
    )?;
    let compacted = InstructionService::history(&store, &session)?.ok_or("history")?;
    assert_eq!(compacted.initial[0].value, "original");
    assert!(compacted.updates.is_empty());
    Ok(())
}
