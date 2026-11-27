//! Fork instruction history uses each aggregate's own sequence domain.
use grapt_instruction::{
    InstructionChange, InstructionHistory, InstructionProjector, InstructionService,
    InstructionValue, RefreshInput, SourceObservation,
};
use grapt_store::{AggregateId, AppendInput, EventStore};
use grapt_types::{EventSeq, EventVersion, InstructionKey, SessionId};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn refresh(
    store: &mut EventStore,
    session: &SessionId,
    event: &str,
    values: &[(&str, &str)],
) -> TestResult {
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    InstructionService::refresh(
        store,
        RefreshInput {
            event: event.parse()?,
            session: session.clone(),
            created_ms: 1_786_147_200_000,
            required: Vec::new(),
            observations: values
                .iter()
                .map(|(key, value)| {
                    Ok(SourceObservation::Available {
                        key: InstructionKey::new(*key).ok_or("instruction key")?,
                        value: InstructionValue::Text((*value).into()),
                    })
                })
                .collect::<Result<_, Box<dyn std::error::Error>>>()?,
        },
    )?;
    Ok(())
}

fn append(
    store: &mut EventStore,
    session: &SessionId,
    event: &str,
    kind: &str,
    version: u32,
    data: &Value,
) -> TestResult {
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let mut projector = InstructionProjector;
    let (_, notices) = store.append(
        AppendInput {
            id: &event.parse()?,
            aggregate,
            writer,
            kind_name: kind,
            version: EventVersion::new(version).ok_or("event version")?,
            created_ms: 1_786_147_200_001,
            data,
        },
        &mut [&mut projector],
    )?;
    notices.discard();
    Ok(())
}

fn compact(store: &mut EventStore, session: &SessionId, event: &str) -> TestResult {
    append(
        store,
        session,
        event,
        "session.compaction.ended",
        1,
        &json!({"sessionID":session.as_str(),"reason":"auto","text":"summary","recent":"tail"}),
    )
}

fn fork(
    store: &mut EventStore,
    parent: &SessionId,
    child: &SessionId,
    event: &str,
    through: u64,
) -> TestResult {
    append(
        store,
        child,
        event,
        "session.forked",
        2,
        &json!({"sessionID":child.as_str(),"parentID":parent.as_str(),"parentSeq":through}),
    )
}

fn current(history: &InstructionHistory) -> BTreeMap<String, String> {
    let mut values: BTreeMap<_, _> = history
        .initial
        .iter()
        .map(|value| (value.key.as_str().to_owned(), value.value.clone()))
        .collect();
    for update in &history.updates {
        for change in &update.changes {
            match change {
                InstructionChange::Value(value) => {
                    values.insert(value.key.as_str().to_owned(), value.value.clone());
                }
                InstructionChange::Removed { key } => {
                    values.remove(key.as_str());
                }
            }
        }
    }
    values
}

fn parent(store: &mut EventStore) -> Result<SessionId, Box<dyn std::error::Error>> {
    let parent: SessionId = "ses_rel15_parent".parse()?;
    refresh(
        store,
        &parent,
        "evt_rel15_initial",
        &[("rules", "first"), ("session/id", "parent")],
    )?;
    compact(store, &parent, "evt_rel15_compaction_one")?;
    refresh(
        store,
        &parent,
        "evt_rel15_update_two",
        &[("rules", "second")],
    )?;
    compact(store, &parent, "evt_rel15_compaction_two")?;
    refresh(
        store,
        &parent,
        "evt_rel15_update_three",
        &[("rules", "third"), ("ancestor-only", "at-fork")],
    )?;
    let history = InstructionService::history(store, &parent)?.ok_or("parent history")?;
    assert_eq!(history.through, EventSeq::new(4));
    Ok(parent)
}

fn matching_history(
    store: &mut EventStore,
    session: &SessionId,
) -> Result<InstructionHistory, Box<dyn std::error::Error>> {
    let history = InstructionService::history(store, session)?.ok_or("child history")?;
    let cached: (i64, i64) = store.connection().query_row(
        "SELECT epoch_start,through_seq FROM instruction_state WHERE session_id=?1",
        [session.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        cached,
        (
            i64::try_from(history.epoch.value())?,
            i64::try_from(history.through.value())?
        )
    );
    assert_eq!(
        Some(history.clone()),
        InstructionService::request_history(store, session)?
    );
    store.connection().execute(
        "DELETE FROM instruction_state WHERE session_id=?1",
        [session.as_str()],
    )?;
    assert_eq!(
        Some(history.clone()),
        InstructionService::rebuild(store, session)?
    );
    Ok(history)
}

#[test]
fn rel15_a_compacted_parent_does_not_refuse_a_childs_first_instruction_update() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    InstructionService::ensure_tables(&mut store)?;
    let parent = parent(&mut store)?;
    let child: SessionId = "ses_rel15_child".parse()?;
    fork(&mut store, &parent, &child, "evt_rel15_fork", 4)?;
    // No cursor edit: the child's ordinary first update is at 1, below the
    // parent's instruction/compaction positions 3 and 4.
    refresh(
        &mut store,
        &child,
        "evt_rel15_child_identity",
        &[("session/id", "child")],
    )?;
    refresh(
        &mut store,
        &parent,
        "evt_rel15_later_parent",
        &[("ancestor-only", "after-fork")],
    )?;
    let history = matching_history(&mut store, &child)?;
    assert_eq!(history.through, EventSeq::new(1));
    assert_eq!(
        current(&history),
        BTreeMap::from([
            ("ancestor-only".into(), "at-fork".into()),
            ("rules".into(), "third".into()),
            ("session/id".into(), "child".into()),
        ])
    );
    compact(&mut store, &child, "evt_rel15_child_compaction")?;
    refresh(
        &mut store,
        &child,
        "evt_rel15_child_rules",
        &[("rules", "child-rules")],
    )?;
    let history = matching_history(&mut store, &child)?;
    assert_eq!(history.epoch.value(), 2);
    assert_eq!(history.through, EventSeq::new(3));
    assert_eq!(current(&history)["rules"], "child-rules");
    Ok(())
}

#[test]
fn rel15_nested_forks_keep_the_frozen_ancestor_state_when_sequences_restart() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    InstructionService::ensure_tables(&mut store)?;
    let parent = parent(&mut store)?;
    let child: SessionId = "ses_rel15_child".parse()?;
    fork(&mut store, &parent, &child, "evt_rel15_fork", 4)?;
    refresh(
        &mut store,
        &child,
        "evt_rel15_child_identity",
        &[("session/id", "child")],
    )?;
    compact(&mut store, &child, "evt_rel15_child_compaction")?;
    refresh(
        &mut store,
        &child,
        "evt_rel15_child_rules",
        &[("rules", "child-rules")],
    )?;
    refresh(
        &mut store,
        &parent,
        "evt_rel15_later_parent",
        &[("ancestor-only", "after-fork")],
    )?;
    let grandchild: SessionId = "ses_rel15_grandchild".parse()?;
    fork(&mut store, &child, &grandchild, "evt_rel15_nested_fork", 3)?;
    refresh(
        &mut store,
        &grandchild,
        "evt_rel15_grandchild_identity",
        &[("session/id", "grandchild")],
    )?;
    let history = matching_history(&mut store, &grandchild)?;
    assert_eq!(
        current(&history),
        BTreeMap::from([
            ("ancestor-only".into(), "at-fork".into()),
            ("rules".into(), "child-rules".into()),
            ("session/id".into(), "grandchild".into()),
        ])
    );
    Ok(())
}

fn upgrade_store(path: &Path, emulate_old_cache: bool) -> TestResult {
    assert!(!path.exists(), "upgrade fixture already exists");
    let mut store = EventStore::open(path, "rel15-upgrade")?;
    InstructionService::ensure_tables(&mut store)?;
    let parent = parent(&mut store)?;
    let middle: SessionId = "ses_rel15_middle".parse()?;
    let leaf: SessionId = "ses_rel15_upgrade_leaf".parse()?;
    fork(&mut store, &parent, &middle, "evt_rel15_middle", 4)?;
    fork(&mut store, &middle, &leaf, "evt_rel15_leaf", 0)?;
    refresh(
        &mut store,
        &parent,
        "evt_rel15_parent_after_upgrade_fork",
        &[("ancestor-only", "after-fork")],
    )?;
    if emulate_old_cache {
        // Old nested narrowing read root0 instead of the frozen root4. Stage
        // exactly that cache from its immutable first delta, not current truth.
        assert_eq!(store.connection().execute(
            "UPDATE instruction_state SET epoch_start=0,through_seq=0,
             initial_values=(SELECT json_extract(data,'$.delta') FROM event WHERE id='evt_rel15_initial'),
             current_values=(SELECT json_extract(data,'$.delta') FROM event WHERE id='evt_rel15_initial')
             WHERE session_id=?1", [leaf.as_str()])?, 1);
    }
    let through: i64 = store.connection().query_row(
        "SELECT through_seq FROM instruction_state WHERE session_id=?1",
        [leaf.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(
        through, 0,
        "fixture was not written with old fork semantics"
    );
    Ok(())
}

#[test]
#[ignore = "old-code child fixture; native upgrade controller supplies a fresh owned database"]
fn rel15_write_old_nested_fork_store() -> TestResult {
    let path = std::env::var_os("GRAPT_REL15_UPGRADE_DB").ok_or("upgrade database path absent")?;
    upgrade_store(&PathBuf::from(path), false)
}

struct UpgradeCleanup(Option<PathBuf>);

impl Drop for UpgradeCleanup {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[test]
fn rel15_a_persisted_legacy_fork_cache_upgrades_without_hiding_corruption() -> TestResult {
    let supplied = std::env::var_os("GRAPT_REL15_UPGRADE_DB").map(PathBuf::from);
    let owned = if supplied.is_none() {
        let root = std::env::temp_dir().join(format!("grapt-rel15-upgrade-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        Some(root)
    } else {
        None
    };
    let cleanup = UpgradeCleanup(owned);
    let path = match supplied {
        Some(path) => {
            assert!(path.is_file(), "old-code database absent");
            path
        }
        None => {
            let path = cleanup
                .0
                .as_ref()
                .ok_or("owned fixture root absent")?
                .join("store.sqlite3");
            upgrade_store(&path, true)?;
            path
        }
    };
    let leaf: SessionId = "ses_rel15_upgrade_leaf".parse()?;
    let mut store = EventStore::open(&path, "rel15-upgrade")?;
    let history = InstructionService::history(&store, &leaf)?.ok_or("upgraded history")?;
    let request = InstructionService::request_history(&store, &leaf)?.ok_or("request history")?;
    assert_eq!(current(&request), current(&history));
    assert!(
        request.updates.is_empty(),
        "request inherited foreign positions"
    );
    assert_eq!(current(&history)["ancestor-only"], "at-fork");
    assert_eq!(current(&history)["rules"], "third");
    let through: i64 = store.connection().query_row(
        "SELECT through_seq FROM instruction_state WHERE session_id=?1",
        [leaf.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(through, 0, "read-only recovery changed the stored cache");
    refresh(
        &mut store,
        &leaf,
        "evt_rel15_upgraded_identity",
        &[("session/id", "upgraded")],
    )?;
    drop(store);

    let mut store = EventStore::open(&path, "rel15-upgrade")?;
    let history = InstructionService::history(&store, &leaf)?.ok_or("reopened history")?;
    assert_eq!(current(&history)["session/id"], "upgraded");
    assert_eq!(current(&history)["ancestor-only"], "at-fork");
    let through: i64 = store.connection().query_row(
        "SELECT through_seq FROM instruction_state WHERE session_id=?1",
        [leaf.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(through, 1, "refresh did not persist the corrected cache");
    let count: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    store.connection().execute(
        "UPDATE instruction_state SET through_seq=123456 WHERE session_id=?1",
        [leaf.as_str()],
    )?;
    assert!(matches!(
        InstructionService::history(&store, &leaf),
        Err(grapt_instruction::InstructionError::Corruption { .. })
    ));
    assert!(matches!(
        InstructionService::request_history(&store, &leaf),
        Err(grapt_instruction::InstructionError::Corruption { .. })
    ));
    assert!(refresh(
        &mut store,
        &leaf,
        "evt_rel15_refused_corrupt_cache",
        &[("rules", "must-not-commit")]
    )
    .is_err());
    let after: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    assert_eq!(count, after, "corrupt-cache recovery appended an event");
    Ok(())
}
