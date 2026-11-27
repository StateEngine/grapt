#[test]
fn migrates_a_typescript_era_database_and_a_real_store_opens_it() -> TestResult {
    let dir = scratch("open")?;
    let db = dir.join("scratch.db");
    typescript_era_database(&db)?;

    let outcome = migrated(&db, &dir);
    let MigrationOutcome::Applied { applied, backup } = outcome else {
        panic!("expected Applied, got {outcome:?}");
    };
    assert_eq!(
        applied.len(),
        grapt_store::ProductionMigrations::load()?.as_slice().len()
    );
    assert!(backup.backup.exists(), "a verified rollback point exists");

    // THE evidence. A real open runs verify_columns x4, the four-arm
    // target-shape probe and the index shape verification against the migrated
    // tables. `Applied` alone proves none of that.
    let store = opened(&db);
    let conn = store.connection();

    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(version, 2, "the migrated database is Rust-owned");
    let attribution: i64 = conn.query_row("SELECT COUNT(*) FROM engine_epoch", [], |r| r.get(0))?;
    assert_eq!(attribution, 1, "exactly one attribution row");

    let families: Vec<String> = conn
        .prepare("SELECT DISTINCT aggregate_family FROM event")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        families,
        vec!["session".to_owned()],
        "every legacy aggregate is back-filled to the session family"
    );
    let unfenced: i64 = conn.query_row(
        "SELECT COUNT(*) FROM event WHERE writer_epoch IS NULL",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        unfenced, 3,
        "imported rows stay LegacyUnfenced: a migration may not mint an epoch for history it did not write"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn every_row_leaves_the_old_table_before_the_old_parent_is_dropped() -> TestResult {
    // The cascade law, witnessed by COUNT rather than by bytes. It is a separate
    // witness from the payload comparison on purpose: dropping the old parent
    // too early and re-encoding a payload are different defects, and if both
    // reddened the same assertion with the same message the harness would score
    // two laws as one record — which is what its duplicate-evidence guard
    // refuses, and rightly.
    let dir = scratch("cascade")?;
    let db = dir.join("scratch.db");
    typescript_era_database(&db)?;

    let before: i64 = {
        let conn = Connection::open(&db)?;
        conn.query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?
    };
    assert_eq!(before, 3, "the source carries three events");

    migrated(&db, &dir);
    let store = opened(&db);
    let after: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    assert_eq!(
        after, before,
        "rows were LOST between the old table and the new one. With foreign keys on,          DROP TABLE event_sequence performs an implicit DELETE that CASCADES into event,          so a rebuild that drops the old parent before copying the child out migrates an          empty log and commits without raising anything"
    );
    let cursors: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM event_sequence", [], |r| r.get(0))?;
    assert_eq!(cursors, 1, "and the aggregate cursor survives with it");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn legacy_payload_bytes_and_sequence_gaps_survive_the_rebuild() -> TestResult {
    let dir = scratch("bytes")?;
    let db = dir.join("scratch.db");
    typescript_era_database(&db)?;

    let before = {
        let conn = Connection::open(&db)?;
        legacy_rows(&conn)?
    };

    migrated(&db, &dir);
    let store = opened(&db);
    let after = legacy_rows(store.connection())?;

    assert_eq!(
        before, after,
        "id, seq, type and data are carried VERBATIM: nothing is re-encoded, \
         re-hashed or re-chained, because imported v1 events keep their original \
         canonical bytes and hashes"
    );
    let seqs: Vec<i64> = after.iter().map(|(_, seq, _, _)| *seq).collect();
    assert_eq!(
        seqs,
        vec![0, 4, 9],
        "gaps are carried, not compacted: sequences are monotonic-with-gaps in the real corpus"
    );
    let cursor: i64 = store.connection().query_row(
        "SELECT seq FROM event_sequence WHERE aggregate_family = 'session' AND aggregate_id = 'ses_alpha'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(cursor, 9, "the aggregate cursor is carried unchanged");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn the_rebuild_destroys_the_family_blind_indexes_and_recreates_the_target_shapes() -> TestResult {
    let dir = scratch("indexes")?;
    let db = dir.join("scratch.db");
    typescript_era_database(&db)?;

    // Precondition, asserted rather than assumed: the source really does carry
    // the target NAMES over the wrong COLUMNS. Without this the test could pass
    // against a source that never had the collision.
    {
        let conn = Connection::open(&db)?;
        assert_eq!(
            index_columns(&conn, "event_aggregate_seq_idx")?,
            vec!["aggregate_id".to_owned(), "seq".to_owned()],
            "the TypeScript era's index is family-blind under the target name"
        );
        assert_eq!(
            index_columns(&conn, "event_aggregate_type_seq_idx")?,
            vec![
                "aggregate_id".to_owned(),
                "type".to_owned(),
                "seq".to_owned()
            ]
        );
    }

    migrated(&db, &dir);
    let store = opened(&db);
    let conn = store.connection();

    assert_eq!(
        index_columns(conn, "event_aggregate_seq_idx")?,
        vec![
            "aggregate_family".to_owned(),
            "aggregate_id".to_owned(),
            "seq".to_owned()
        ],
        "the surviving UNIQUE index must be family-scoped, or it enforces the wrong law"
    );
    assert!(index_is_unique(conn, "event_aggregate_seq_idx")?);
    assert_eq!(
        index_columns(conn, "event_aggregate_type_seq_idx")?,
        vec![
            "aggregate_family".to_owned(),
            "aggregate_id".to_owned(),
            "type".to_owned(),
            "seq".to_owned()
        ]
    );
    assert!(!index_is_unique(conn, "event_aggregate_type_seq_idx")?);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn an_open_refuses_a_same_named_index_over_the_wrong_columns() -> TestResult {
    let dir = scratch("stale-index")?;
    let db = dir.join("scratch.db");

    // A database in the TARGET table shape whose UNIQUE index is the
    // TypeScript era's family-blind one under the target name. This is exactly
    // what an ADD COLUMN migration would have left behind, and what
    // `CREATE ... INDEX IF NOT EXISTS` silently honours: `PRAGMA table_info`
    // reports columns and cannot see an index at all, so every existing check
    // passes while the store runs on the wrong uniqueness law.
    {
        let conn = Connection::open(&db)?;
        conn.execute_batch(
            "CREATE TABLE event_sequence (
                aggregate_family TEXT NOT NULL, aggregate_id TEXT NOT NULL,
                seq INTEGER NOT NULL, owner_id TEXT,
                PRIMARY KEY (aggregate_family, aggregate_id));
             CREATE TABLE event (
                id TEXT PRIMARY KEY, aggregate_family TEXT NOT NULL,
                aggregate_id TEXT NOT NULL, seq INTEGER NOT NULL,
                created INTEGER NOT NULL, type TEXT NOT NULL, data TEXT,
                writer_epoch INTEGER, envelope_bytes BLOB, campaign_transition_id TEXT,
                CONSTRAINT event_family_shape CHECK (
                    (aggregate_family = 'campaign' AND data IS NULL
                        AND envelope_bytes IS NOT NULL AND campaign_transition_id IS NOT NULL)
                    OR (aggregate_family <> 'campaign' AND data IS NOT NULL
                        AND envelope_bytes IS NULL AND campaign_transition_id IS NULL)),
                FOREIGN KEY (aggregate_family, aggregate_id)
                    REFERENCES event_sequence (aggregate_family, aggregate_id) ON DELETE CASCADE);
             CREATE UNIQUE INDEX event_aggregate_seq_idx ON event (aggregate_id, seq);",
        )?;
    }

    let refusal = EventStore::open(&db, "0.0.1");
    match refusal {
        Err(WriteError::IndexShapeMismatch {
            index,
            found,
            expected,
            ..
        }) => {
            assert_eq!(index, "event_aggregate_seq_idx");
            assert_eq!(found, vec!["aggregate_id".to_owned(), "seq".to_owned()]);
            assert_eq!(
                expected,
                ["aggregate_family", "aggregate_id", "seq"],
                "the refusal names the target column list"
            );
        }
        Err(other) => panic!("expected IndexShapeMismatch, got {other}"),
        Ok(_) => panic!(
            "an open ACCEPTED a family-blind UNIQUE index carrying the target name:              CREATE ... INDEX IF NOT EXISTS honoured the wrong-shaped namesake and nothing caught it"
        ),
    }

    // SCOPE INVERSION. The identical database with the CORRECT index column
    // list opens, so the refusal is attributable to the index shape and not to
    // anything else about this schema.
    {
        let conn = Connection::open(&db)?;
        conn.execute_batch(
            "DROP INDEX event_aggregate_seq_idx;
             CREATE UNIQUE INDEX event_aggregate_seq_idx
                ON event (aggregate_family, aggregate_id, seq);",
        )?;
    }
    opened(&db);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn the_run_is_idempotent_and_journals_all_migrations_from_the_injected_clock() -> TestResult {
    let dir = scratch("journal")?;
    let db = dir.join("scratch.db");
    typescript_era_database(&db)?;

    migrated(&db, &dir);
    assert_eq!(
        migrated(&db, &dir),
        MigrationOutcome::AlreadyCurrent,
        "a second run applies nothing and takes no backup"
    );

    let store = opened(&db);
    let mut statement = store
        .connection()
        .prepare("SELECT id, time_completed FROM rust_migration ORDER BY id")?;
    let journal: Vec<(String, i64)> = statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut expected: Vec<_> = grapt_store::ProductionMigrations::load()?
        .as_slice()
        .iter()
        .map(|migration| (migration.id().as_str().to_owned(), FROZEN_MS))
        .collect();
    expected.sort_unstable();
    assert_eq!(
        journal, expected,
        "all migrations are registered, and the durable timestamp comes from the \
         INJECTED authority rather than SQL's strftime('%s','now')"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
