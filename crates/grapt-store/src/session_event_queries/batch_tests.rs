//! BR-H: exact-call selection, refusal and bundled-SQLite work witnesses.
use super::batch::Selection;
use super::*;
use rusqlite::StatementStatus;
type Rows = Vec<(u64, String)>;
struct Probe {
    pages: Vec<Result<Rows, String>>,
    vm_steps: Option<u64>,
    statements: usize,
}
fn batch(
    store: &EventStore,
    session: &SessionId,
    kind: &str,
    targets: &[ToolCallId],
    selection: Selection,
    bound: usize,
) -> Result<Probe, String> {
    match super::batch::read(store, session, kind, targets, selection, bound)? {
        Some(pages) => Ok(Probe {
            pages: pages.rows.into_iter().map(Ok).collect(),
            vm_steps: Some(pages.steps),
            statements: pages.statements,
        }),
        None => Ok(Probe {
            pages: targets
                .iter()
                .map(|target| {
                    scoped_rows(
                        store,
                        session,
                        kind,
                        None,
                        None,
                        selection.scope(target),
                        bound,
                    )
                })
                .collect(),
            vm_steps: None,
            statements: 1 + targets.len(),
        }),
    }
}

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0)
            .unwrap_or_else(|error| panic!("batch fixture cleanup failed: {error}"));
    }
}
struct Fixture {
    store: EventStore,
    session: SessionId,
    raw: rusqlite::Connection,
    _scratch: Scratch,
}
fn fixture() -> Result<Fixture, String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ORDINAL: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "grapt-history-batch-{}-{}",
        std::process::id(),
        ORDINAL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).map_err(|error| error.to_string())?;
    let scratch = Scratch(root.clone());
    let database = root.join("events.sqlite3");
    let store = EventStore::open(&database, "history-batch").map_err(|error| error.to_string())?;
    let raw = rusqlite::Connection::open(&database).map_err(|error| error.to_string())?;
    let session = "ses_history_batch"
        .parse::<SessionId>()
        .map_err(|error| error.to_string())?;
    raw.execute("INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES ('session', ?1, 0)", [session.as_str()]).map_err(|error| error.to_string())?;
    Ok(Fixture {
        store,
        session,
        raw,
        _scratch: scratch,
    })
}

fn insert(
    conn: &rusqlite::Connection,
    session: &SessionId,
    seq: i64,
    kind: &str,
    data: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES (?1, 'session', ?2, ?3, 0, ?4, ?5, 1)",
        (format!("evt_history_batch_{seq}"), session.as_str(), seq, kind, data),
    ).map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn br_h_bounded_batch_preserves_exact_target_rows_and_query_refusals() -> Result<(), String> {
    let fixture = fixture()?;
    let (store, session) = (&fixture.store, &fixture.session);
    let targets = ["call_a", "call_b", "call_absent"]
        .into_iter()
        .map(|raw| ToolCallId::new(raw).ok_or("target"))
        .collect::<Result<Vec<_>, _>>()?;
    for (index, data) in [
        r#"{"call":"call_a","attempt":"pat_a"}"#,
        r#"{"call":"call_b","attempt":"pat_b"}"#,
        r#"{"call":"call_other","attempt":"pat_a"}"#,
        r#"{"call":42,"attempt":"pat_a"}"#,
        r#"{"call":"call_a","attempt":"pat_nested"}"#,
        r#"{"call":"call_a","attempt":"pat_a","future":"vocabulary"}"#,
        r#"{"attempt":"pat_a"}"#,
        "{not JSON",
    ]
    .into_iter()
    .enumerate()
    {
        insert(
            &fixture.raw,
            session,
            index as i64 + 1,
            "session.tool.terminal.1",
            data,
        )?;
    }
    insert(
        &fixture.raw,
        session,
        20,
        "session.tool.nested.started.1",
        r#"{"call":"call_a","attempt":"pat_nested","parent":"call_other"}"#,
    )?;
    fixture.raw.execute("INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES ('operation', ?1, 0), ('session', 'ses_foreign_batch', 0)", [session.as_str()]).map_err(|error| error.to_string())?;
    for (id, family, owner, seq, kind, data) in [
        (
            "evt_foreign_family",
            "operation",
            session.as_str(),
            30,
            "session.tool.terminal.1",
            "{bad",
        ),
        (
            "evt_foreign_session",
            "session",
            "ses_foreign_batch",
            30,
            "session.tool.terminal.1",
            "{bad",
        ),
        (
            "evt_foreign_nested_family",
            "operation",
            session.as_str(),
            31,
            "session.tool.nested.started.1",
            r#"{"call":"call_b","attempt":"pat_b","parent":"foreign"}"#,
        ),
        (
            "evt_foreign_nested_session",
            "session",
            "ses_foreign_batch",
            31,
            "session.tool.nested.started.1",
            r#"{"call":"call_a","attempt":"pat_a","parent":"foreign"}"#,
        ),
    ] {
        fixture.raw.execute("INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, 1)", (id, family, owner, seq, kind, data)).map_err(|error| error.to_string())?;
    }
    for selection in [Selection::Call("$.call"), Selection::Model] {
        let Probe {
            pages: full,
            statements: queries,
            ..
        } = batch(
            store,
            session,
            "session.tool.terminal.1",
            &targets,
            selection,
            128,
        )?;
        assert_eq!(queries, 1);
        // Row 8 is a terminal that is not JSON: evidence for no call.
        let expected_a: &[u64] = match selection {
            Selection::Call(_) => &[1, 4, 5, 6, 7],
            Selection::Model => &[1, 4, 6, 7],
        };
        assert_eq!(
            full[0]
                .as_ref()
                .map_err(Clone::clone)?
                .iter()
                .map(|row| row.0)
                .collect::<Vec<_>>(),
            expected_a
        );
        assert_eq!(
            full[1]
                .as_ref()
                .map_err(Clone::clone)?
                .iter()
                .map(|row| row.0)
                .collect::<Vec<_>>(),
            [2, 4, 7],
            "foreign nested identity suppressed a model call"
        );
        assert_eq!(
            full[2]
                .as_ref()
                .map_err(Clone::clone)?
                .iter()
                .map(|row| row.0)
                .collect::<Vec<_>>(),
            [4, 7],
            "ambiguous rows vanished from an absent target"
        );
        for bound in [1, 3, 128] {
            let Probe {
                pages,
                statements: queries,
                ..
            } = batch(
                store,
                session,
                "session.tool.terminal.1",
                &targets,
                selection,
                bound,
            )?;
            assert!(queries <= 1 + targets.len());
            for (target, actual) in targets.iter().zip(pages) {
                assert_eq!(
                    actual,
                    scoped_rows(
                        store,
                        session,
                        "session.tool.terminal.1",
                        None,
                        None,
                        selection.scope(target),
                        bound
                    ),
                    "{target}, bound={bound}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn br_h_batch_vm_work_improves_selected_calls_without_a_new_index() -> Result<(), String> {
    let fixture = fixture()?;
    let (store, session) = (&fixture.store, &fixture.session);
    let targets = (0..8)
        .map(|index| ToolCallId::new(format!("call_batch_{index}")).ok_or("target"))
        .collect::<Result<Vec<_>, _>>()?;
    for index in 0..512 {
        insert(
            &fixture.raw,
            session,
            index + 1,
            "session.tool.terminal.1",
            &format!(r#"{{"call":"call_batch_{index}","attempt":"pat_batch"}}"#),
        )?;
    }
    for index in 0..5002 {
        insert(
            &fixture.raw,
            session,
            index + 1000,
            "session.execution.started.1",
            "{}",
        )?;
    }
    for selection in [Selection::Call("$.call"), Selection::Model] {
        let mut single_steps = 0u64;
        for target in &targets {
            let (_, path, predicate) = scope_selection(selection.scope(target));
            let mut statement = store
                .connection()
                .prepare(&scoped_sql("e.data", &predicate, "", "1"))
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map(
                    (
                        session.as_str(),
                        "session.tool.terminal.1",
                        129i64,
                        i64::MIN,
                        target.as_str(),
                        path,
                        Option::<i64>::None,
                    ),
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            assert_eq!(rows.len(), 1);
            single_steps += u64::try_from(statement.get_status(StatementStatus::VmStep))
                .map_err(|error| error.to_string())?;
        }
        let Probe {
            pages,
            vm_steps: batch_steps,
            statements: queries,
        } = batch(
            store,
            session,
            "session.tool.terminal.1",
            &targets,
            selection,
            128,
        )?;
        assert_eq!(queries, 1);
        let batch_steps = batch_steps.ok_or("shared query VM measurement is missing")?;
        assert!(
            batch_steps < single_steps / 2,
            "single={single_steps}, batch={batch_steps}"
        );
        assert!(pages
            .iter()
            .all(|page| page.as_ref().is_ok_and(|rows| rows.len() == 1)));
        eprintln!("J01_BATCH_VM single={single_steps} batch={batch_steps}");
    }
    Ok(())
}

#[test]
fn br_h_nested_terminals_cannot_resurrect_model_outcomes() -> Result<(), String> {
    let fixture = fixture()?;
    let (store, session) = (&fixture.store, &fixture.session);
    let targets = [
        ToolCallId::new("call_nested").ok_or("call")?,
        ToolCallId::new("call_model").ok_or("call")?,
    ];
    insert(
        &fixture.raw,
        session,
        1,
        "session.tool.terminal.1",
        r#"{"call":"call_nested","attempt":"pat_nested"}"#,
    )?;
    insert(
        &fixture.raw,
        session,
        2,
        "session.tool.terminal.1",
        r#"{"call":"call_model","attempt":"pat_model"}"#,
    )?;
    insert(
        &fixture.raw,
        session,
        3,
        "session.tool.nested.started.1",
        r#"{"call":"call_nested","attempt":"pat_nested","parent":"call_parent"}"#,
    )?;
    let Probe { pages, .. } = batch(
        store,
        session,
        "session.tool.terminal.1",
        &targets,
        Selection::Model,
        128,
    )?;
    assert!(
        pages[0].as_ref().map_err(Clone::clone)?.is_empty(),
        "a nested terminal resurrected a model outcome"
    );
    assert_eq!(
        pages[1]
            .as_ref()
            .map_err(Clone::clone)?
            .iter()
            .map(|row| row.0)
            .collect::<Vec<_>>(),
        [2]
    );
    fixture
        .raw
        .execute_batch("DROP TABLE event")
        .map_err(|error| error.to_string())?;
    assert!(
        super::batch::read(
            store,
            session,
            "session.tool.terminal.1",
            &targets,
            Selection::Model,
            128
        )
        .is_err(),
        "a failed shared read became fallback or absence"
    );
    Ok(())
}

fn materialization_probe(
    padding: usize,
    count: usize,
    copied_rows: usize,
    fallback: bool,
) -> Result<(), String> {
    let fixture = fixture()?;
    let mut targets = Vec::new();
    let mut expected_copied = 0usize;
    for seq in 1..=count {
        let target = ToolCallId::new(format!("call_precopy_{seq}")).ok_or("target")?;
        let data = serde_json::json!({
            "callID": target.as_str(), "padding": "x".repeat(padding)
        })
        .to_string();
        if seq <= copied_rows {
            expected_copied += data.len();
        }
        targets.push(target);
        insert(
            &fixture.raw,
            &fixture.session,
            i64::try_from(seq).map_err(|error| error.to_string())?,
            "session.tool.input.started.1",
            &data,
        )?;
    }
    super::batch::take_materialized_bytes();
    let result = super::batch::read(
        &fixture.store,
        &fixture.session,
        "session.tool.input.started.1",
        &targets,
        Selection::Call("$.callID"),
        128,
    )?;
    let copied = super::batch::take_materialized_bytes();
    assert_eq!(result.is_none(), fallback);
    assert_eq!(
        copied, expected_copied,
        "BR-H continuation probe copied data before its budget check"
    );
    assert!(copied <= 512 * 1024);
    Ok(())
}

#[test]
fn br_h_continuation_probe_refuses_one_oversized_row_before_copying() -> Result<(), String> {
    materialization_probe(600 * 1024, 1, 0, true)
}

#[test]
fn br_h_continuation_probe_stops_cumulative_copies_before_the_budget() -> Result<(), String> {
    materialization_probe(200 * 1024, 2, 2, false)?;
    materialization_probe(200 * 1024, 3, 2, true)
}

/// Terminals past the stored-terminal bound are read as their call hint:
/// the batch copies none of their payload and needs no fallback.
#[test]
fn br_h_batched_terminals_never_copy_an_oversized_row() -> Result<(), String> {
    let fixture = fixture()?;
    let mut targets = Vec::new();
    for seq in 1..=8 {
        let target = ToolCallId::new(format!("call_oversized_{seq}")).ok_or("target")?;
        let data = serde_json::json!({"call": target.as_str(), "padding": "x".repeat(300 * 1024)});
        insert(
            &fixture.raw,
            &fixture.session,
            seq,
            "session.tool.terminal.1",
            &data.to_string(),
        )?;
        targets.push(target);
    }
    super::batch::take_materialized_bytes();
    let pages = super::batch::read(
        &fixture.store,
        &fixture.session,
        "session.tool.terminal.1",
        &targets,
        Selection::Model,
        128,
    )?
    .ok_or("oversized terminals fell back to the individual path")?;
    assert!(super::batch::take_materialized_bytes() < 1024);
    for (target, rows) in targets.iter().zip(pages.rows) {
        let [(_, data)] = rows.as_slice() else {
            return Err(format!("{target}: expected one row"));
        };
        assert_eq!(
            data,
            &serde_json::json!({"call": target.as_str()}).to_string()
        );
    }
    Ok(())
}

/// A non-terminal row stored as a blob is refused by its strict individual
/// reader, so the batch hands it back to that reader instead of reading it.
#[test]
fn br_h_a_non_terminal_blob_row_takes_the_individual_refusal() -> Result<(), String> {
    let fixture = fixture()?;
    let target = ToolCallId::new("call_blob").ok_or("target")?;
    insert(
        &fixture.raw,
        &fixture.session,
        1,
        "session.tool.input.started.1",
        "{}",
    )?;
    fixture
        .raw
        .execute(
            "UPDATE event SET data = CAST('{\"callID\":\"call_blob\"}' AS BLOB)",
            [],
        )
        .map_err(|error| error.to_string())?;
    let (store, session) = (&fixture.store, &fixture.session);
    let kind = "session.tool.input.started.1";
    let selection = Selection::Call("$.callID");
    let batched = super::batch::read(
        store,
        session,
        kind,
        std::slice::from_ref(&target),
        selection,
        128,
    )?;
    assert!(
        batched.is_none(),
        "the batch read a blob its reader refuses"
    );
    let individual = scoped_rows(
        store,
        session,
        kind,
        None,
        None,
        selection.scope(&target),
        128,
    );
    assert!(individual.is_err());
    Ok(())
}

/// Oversized terminals still name the call they were selected for, at any
/// length, even when the `{"call": ...}` hint alone is past the bound; no
/// row copies the target to say so.
#[test]
fn an_oversized_terminal_keeps_a_long_call_identity() -> Result<(), String> {
    for (length, count) in [(257, 1), (70 * 1024, 128)] {
        let fixture = fixture()?;
        let target = ToolCallId::new("c".repeat(length)).ok_or("target")?;
        let data = serde_json::json!({"call": target.as_str(), "padding": "x".repeat(70 * 1024)});
        let data = data.to_string();
        for seq in 1..=count {
            insert(
                &fixture.raw,
                &fixture.session,
                seq,
                "session.tool.terminal.1",
                &data,
            )?;
        }
        let (store, session) = (&fixture.store, &fixture.session);
        let scope = Scope::Model(Some(&target));
        let rows = scoped_terminals(store, session, None, None, scope, 128)?;
        assert_eq!(
            rows.len(),
            usize::try_from(count).map_err(|e| e.to_string())?
        );
        for row in &rows {
            assert!(row.names_target, "{length}");
            let StoredTerminal::Unreadable { call } = &row.terminal else {
                return Err(format!("{length}: an oversized row decoded"));
            };
            // Only a hint that fits the bound is kept; the long one is not copied.
            assert_eq!(call.is_some(), length == 257);
        }
        // The batch matches it by ordinal and copies only the bounded column.
        let other = ToolCallId::new("call_other").ok_or("other")?;
        super::batch::take_materialized_bytes();
        let targets = [other, target];
        let terminal = "session.tool.terminal.1";
        let pages = super::batch::read(store, session, terminal, &targets, Selection::Model, 128)?
            .ok_or("long identity fell back to the individual path")?;
        assert!(super::batch::take_materialized_bytes() <= 512 * 1024);
        let expected = [0, usize::try_from(count).map_err(|e| e.to_string())?];
        assert_eq!(
            pages.rows.iter().map(Vec::len).collect::<Vec<_>>(),
            expected
        );
    }
    Ok(())
}

#[test]
fn br_h_continuation_step_failure_stays_unreadable() -> Result<(), String> {
    let fixture = fixture()?;
    fixture.raw.execute_batch("DROP TABLE event; CREATE VIEW event AS SELECT 'session' AS aggregate_family, 'ses_history_batch' AS aggregate_id, 1 AS seq, 'session.tool.terminal.1' AS type, abs(-9223372036854775808) AS data").map_err(|error| error.to_string())?;
    let target = ToolCallId::new("call_step").ok_or("target")?;
    let error = super::batch::read(
        &fixture.store,
        &fixture.session,
        "session.tool.terminal.1",
        &[target],
        Selection::Model,
        128,
    )
    .err()
    .ok_or("SQLite step failure became fallback or absence")?;
    assert!(error.contains("integer overflow"));
    Ok(())
}

/// SC-HIST-01: the nested pairs are an uncorrelated set now. A pair with a
/// missing half matches nothing, exactly as the correlated `=` probe did: a
/// terminal without an attempt stays a model outcome, and a nested start
/// without one hides no terminal.
#[test]
fn model_selection_keeps_null_pair_semantics_of_the_correlated_probe() -> Result<(), String> {
    let fixture = fixture()?;
    let (store, session) = (&fixture.store, &fixture.session);
    let targets = [
        ToolCallId::new("call_unpaired").ok_or("call")?,
        ToolCallId::new("call_shadowed").ok_or("call")?,
        ToolCallId::new("call_nested").ok_or("call")?,
    ];
    for (seq, kind, data) in [
        (1, "session.tool.terminal.1", r#"{"call":"call_unpaired"}"#),
        (
            2,
            "session.tool.terminal.1",
            r#"{"call":"call_shadowed","attempt":"pat_a"}"#,
        ),
        (
            3,
            "session.tool.terminal.1",
            r#"{"call":"call_nested","attempt":"pat_n"}"#,
        ),
        (
            4,
            "session.tool.nested.started.1",
            r#"{"call":"call_unpaired","parent":"call_parent"}"#,
        ),
        (
            5,
            "session.tool.nested.started.1",
            r#"{"call":"call_shadowed","parent":"call_parent"}"#,
        ),
        (
            6,
            "session.tool.nested.started.1",
            r#"{"call":"call_nested","attempt":"pat_n","parent":"call_parent"}"#,
        ),
    ] {
        insert(&fixture.raw, session, seq, kind, data)?;
    }
    let Probe { pages, .. } = batch(
        store,
        session,
        "session.tool.terminal.1",
        &targets,
        Selection::Model,
        128,
    )?;
    let seqs = |index: usize| -> Result<Vec<u64>, String> {
        Ok(pages[index]
            .as_ref()
            .map_err(Clone::clone)?
            .iter()
            .map(|row| row.0)
            .collect())
    };
    assert_eq!(
        seqs(0)?,
        [1],
        "a terminal without an attempt left the model view"
    );
    assert_eq!(
        seqs(1)?,
        [2],
        "a nested start without an attempt hid a terminal"
    );
    assert!(
        seqs(2)?.is_empty(),
        "a nested terminal resurrected a model outcome"
    );
    Ok(())
}
