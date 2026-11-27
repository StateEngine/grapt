use super::{EventStore, WriteError};
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

/// A synchronous projector. Runs INSIDE the append transaction — it either
/// commits with the event or the whole append rolls back. No projector may
/// hold state across events except through the database it is handed.
///
/// [`Self::projection`] is both the durable key of this projector's watermark
/// and the name its failures are reported under. It is deliberately ONE method:
/// a separate display name beside the watermark key would be a second carrier of
/// one identity, free to drift, and no single edit could then attribute either.
/// One arm of the target-shape probe: the row it offers, and the verdict the
/// target schema must return for it.
struct ProbeArm<'a> {
    name: &'a str,
    family: &'a str,
    data: Option<&'a str>,
    envelope: Option<&'a [u8]>,
    transition: Option<&'a str>,
    expect: ProbeExpectation,
}

/// What the target schema must do with a probe row.
///
/// Two outcomes only, and neither is "either": an arm that tolerated both
/// would prove nothing about the constraint it exists to exercise.
enum ProbeExpectation {
    /// The target shape stores this row.
    Accepts,
    /// The target shape refuses it, by the NAMED family-shape constraint.
    RefusesFamilyShape,
}

impl EventStore {
    /// The indexes the target shape requires, by COLUMN LIST and uniqueness.
    ///
    /// Names are not evidence here — that is the whole point. The TypeScript
    /// era ships `event_aggregate_seq_idx` as `UNIQUE (aggregate_id, seq)` and
    /// `event_aggregate_type_seq_idx` as `(aggregate_id, type, seq)`, both
    /// under the exact names the target uses and both family-BLIND. On any
    /// database where those survived, `CREATE ... INDEX IF NOT EXISTS` no-ops
    /// and the store runs on the old law while every existing check passes.
    /// `(table, index, exact column list, unique)`.
    ///
    /// The table travels with the index because more than one governed table
    /// now carries required indexes, and one table-driven verifier is what
    /// keeps "declared required" and "actually shaped that way" from drifting
    /// apart per table.
    const REQUIRED_INDEXES: &'static [(
        &'static str,
        &'static str,
        &'static [&'static str],
        bool,
    )] = &[
        (
            "campaign_plan_artifact",
            "campaign_plan_artifact_identity",
            &["campaign_id", "plan_version"],
            true,
        ),
        (
            "event",
            "event_aggregate_seq_idx",
            &["aggregate_family", "aggregate_id", "seq"],
            true,
        ),
        (
            "event",
            "event_aggregate_type_seq_idx",
            &["aggregate_family", "aggregate_id", "type", "seq"],
            false,
        ),
        (
            "event",
            "event_campaign_transition_idx",
            &["aggregate_family", "aggregate_id", "campaign_transition_id"],
            true,
        ),
        // The carry enumeration's access law (content_refs): its predicate
        // is the event TYPE ALONE, so no index that leads with the
        // aggregate coordinate can serve it, and without this one the
        // per-launch admission is a full scan of the growing event table.
        // `id` is second because it is also the query's ordering column.
        ("event", "event_type_id_idx", &["type", "id"], false),
        (
            "provider_attempt_accounting",
            "provider_attempt_accounting_session_idx",
            &["session_id", "attempt_id"],
            false,
        ),
        // The mailbox's ordering index is UNIQUE on purpose: two rows at one
        // local position for one recipient would make the exclusive page cursor
        // ambiguous and could hide a message behind its twin. A same-named
        // non-unique namesake would be honoured silently by
        // `CREATE ... IF NOT EXISTS`, which is exactly the hazard this check
        // exists for.
        (
            "mailbox_message",
            "mailbox_message_recipient_seq_idx",
            &["recipient", "admission_seq"],
            true,
        ),
        (
            "mailbox_message",
            "mailbox_message_supersedes_idx",
            &["recipient", "supersedes"],
            false,
        ),
        // NOT unique, and the column ORDER is the law rather than a preference.
        // These are exactly the recovery scan's predicate and ordering:
        // equality on the session, range and order on the attempt. A same-named
        // namesake in any other shape would be honoured silently by
        // `CREATE INDEX IF NOT EXISTS`, so the shape is verified at open rather
        // than assumed from the name.
        //
        // This replaced a `(settled_ms, session_id, attempt_id)` index whose
        // leading column the corrected settled-INCLUSIVE scan never constrains.
        // Verification passed against that index while it served nothing: a
        // shape check proves the index is what was declared, not that the
        // declaration matches the query.
        (
            "attempt_receipt",
            "attempt_receipt_recovery",
            &["session_id", "attempt_id"],
            false,
        ),
    ];

    /// Built after readiness by `build_mailbox_scan_indexes`, so an open may
    /// find them absent; a present one must still have this shape. The wake
    /// scan's open-work read and delivery debt by SQL.
    pub(crate) const DEFERRED_INDEXES: &'static [(
        &'static str,
        &'static str,
        &'static [&'static str],
        bool,
    )] = &[
        (
            "mailbox_message",
            "mailbox_message_open_idx",
            &["recipient", "consumed", "admission_seq"],
            false,
        ),
        (
            "mailbox_message",
            "mailbox_message_debt_idx",
            &["recipient", "source_acknowledged", "admission_seq"],
            false,
        ),
    ];

    pub(super) fn verify_index_shapes(conn: &Connection) -> Result<(), WriteError> {
        let required = Self::REQUIRED_INDEXES.iter().map(|entry| (entry, true));
        let deferred = Self::DEFERRED_INDEXES.iter().map(|entry| (entry, false));
        for ((table, index, expected, expected_unique), must_exist) in required.chain(deferred) {
            let mut listed = conn.prepare(&format!("PRAGMA index_list(\"{table}\")"))?;
            let unique = listed
                .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(index))
                .map(|(_, unique)| unique != 0);
            if !must_exist {
                // SQLite index names are case-insensitive and schema-wide: any
                // namesake, on any table, makes CREATE INDEX IF NOT EXISTS
                // keep it, so it is verified here rather than treated as absent.
                let owner: Option<String> = conn
                    .query_row(
                        "SELECT tbl_name FROM sqlite_master WHERE type = 'index' AND name = ?1 COLLATE NOCASE",
                        [index],
                        |row| row.get(0),
                    )
                    .optional()?;
                match owner {
                    None => continue,
                    Some(owner) if !owner.eq_ignore_ascii_case(table) => {
                        return Err(WriteError::IndexShapeMismatch {
                            index,
                            found: vec![format!("<an index on {owner}>")],
                            found_unique: unique.unwrap_or(false),
                            expected,
                            expected_unique: *expected_unique,
                        });
                    }
                    Some(_) => {}
                }
            }
            let mut info = conn.prepare(&format!("PRAGMA index_info(\"{index}\")"))?;
            // An expression index reports a NULL column name; it is not the
            // target shape either way, so it renders as an explicit marker
            // rather than being silently skipped.
            let found = info
                .query_map([], |r| r.get::<_, Option<String>>(2))?
                .map(|column| column.map(|c| c.unwrap_or_else(|| "<expression>".to_owned())))
                .collect::<Result<Vec<_>, _>>()?;
            let found_unique = unique.unwrap_or(false);
            if found
                .iter()
                .map(String::as_str)
                .ne(expected.iter().copied())
                || found_unique != *expected_unique
            {
                return Err(WriteError::IndexShapeMismatch {
                    index,
                    found,
                    found_unique,
                    expected,
                    expected_unique: *expected_unique,
                });
            }
        }
        Ok(())
    }

    /// Brings the database at `path` to the shape this store opens.
    ///
    /// THE production migration entry point. Before this existed,
    /// `migration::apply` had no production caller at all: the mechanism was
    /// complete and its content was empty — zero `impl Migration` outside test
    /// modules, zero call sites. A migration path that only tests can reach is
    /// not a migration path.
    ///
    /// Applies [`crate::migrations::ProductionMigrations`] behind the existing
    /// backup-first, `data_version`-fenced, per-migration-transaction
    /// machinery, and registers each in the `rust_migration` journal.
    pub fn migrate(
        path: &Path,
        backup_dir: &Path,
        engine_version: &str,
    ) -> Result<crate::migration::MigrationOutcome, crate::migration::MigrationError> {
        Self::migrate_with_clock(path, backup_dir, engine_version, &crate::clock::SystemClock)
    }

    /// [`EventStore::migrate`] under an injected clock authority.
    pub fn migrate_with_clock(
        path: &Path,
        backup_dir: &Path,
        engine_version: &str,
        clock: &dyn grapt_types::ClockAuthority,
    ) -> Result<crate::migration::MigrationOutcome, crate::migration::MigrationError> {
        let set = crate::migrations::ProductionMigrations::load()?;
        crate::migration::apply_with_clock(path, backup_dir, engine_version, &set.as_slice(), clock)
    }

    /// Enables foreign-key enforcement and PROVES it took effect.
    ///
    /// The declared `event -> event_sequence` foreign key is enforced today,
    /// but only because `rusqlite` happens to enable foreign keys by default.
    /// That is a DEPENDENCY DEFAULT, not something this crate says: a driver
    /// change, or a connection that reaches this database another way, would
    /// turn it off, and the only symptom would be orphan rows that quietly stop
    /// refusing. `F3B3_RESIDUALS.md` item 2 records the measurement and hands
    /// the initialization here; this statement makes the guarantee the store's
    /// own.
    ///
    /// The read-back is not ceremony. The `ON` form reports no error on a build
    /// without foreign-key support — it simply has no effect — so setting
    /// without verifying would hand back a store whose central integrity
    /// constraint is inert and say nothing about it.
    ///
    /// Scope, stated honestly: the pragma is per-CONNECTION. Every caller that
    /// reaches this database through the store inherits it, but a raw
    /// `rusqlite::Connection` opened to the same file by some other route does
    /// not. See `F3B4B_RESIDUALS.md`.
    pub(super) fn enforce_foreign_keys(conn: &Connection) -> Result<(), WriteError> {
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        let read_back: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
        if read_back != 1 {
            return Err(WriteError::ForeignKeysUnavailable { read_back });
        }
        Ok(())
    }

    /// Returns how many row changes it made that were rolled back again: the
    /// schema probe's rows, written inside savepoints that are always rolled
    /// back. `stamp_and_prepare` needs that count to tell a preparation that
    /// changed nothing durable from one that did.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn ensure_schema(
        conn: &rusqlite::Transaction<'_>,
        budget: &crate::session_list::DeferredVmBudget,
    ) -> Result<u64, rusqlite::Error> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS event_sequence (
                aggregate_family TEXT NOT NULL,
                aggregate_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                owner_id TEXT,
                PRIMARY KEY (aggregate_family, aggregate_id)
            );
            CREATE TABLE IF NOT EXISTS event (
                id TEXT PRIMARY KEY,
                aggregate_family TEXT NOT NULL,
                aggregate_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                created INTEGER NOT NULL,
                type TEXT NOT NULL,
                data TEXT,
                writer_epoch INTEGER,
                envelope_bytes BLOB,
                campaign_transition_id TEXT,
                CONSTRAINT event_family_shape CHECK (
                    (aggregate_family = 'campaign'
                        AND data IS NULL
                        AND envelope_bytes IS NOT NULL
                        AND campaign_transition_id IS NOT NULL)
                    OR
                    (aggregate_family <> 'campaign'
                        AND data IS NOT NULL
                        AND envelope_bytes IS NULL
                        AND campaign_transition_id IS NULL)
                ),
                FOREIGN KEY (aggregate_family, aggregate_id)
                    REFERENCES event_sequence (aggregate_family, aggregate_id) ON DELETE CASCADE
            );",
        )?;
        // The writer-election table is a GOVERNED table and is prepared in the
        // same transaction as the event tables, so a database can never be
        // Rust-owned while missing the authority that fences its writers.
        crate::election::ensure_schema(conn)?;
        // Likewise the projection watermark: a database that carried facts but
        // no place to record authoritative projection progress would force the
        // first projector either to lead its input or to invent a second
        // ownership story for its own position.
        crate::projection::ensure_schema(conn)?;
        // Tables kept for rows the retired campaign draft feature wrote. They
        // are still prepared here so every database keeps one shape; see
        // `crate::retired_campaign`.
        crate::retired_campaign::ensure_phase_table(conn)?;
        crate::retired_campaign::ensure_plan_artifact_table(conn)?;
        // The compaction caches are CONSUMER-OWNED and deliberately absent from
        // the governed set, but they are still prepared here: a database that
        // carried compaction facts and no place to derive a window from them
        // would force the first reader to fold the whole log on every read.
        // Preparing them with the rest is what keeps "the cache is empty" a
        // statement about the log rather than about the schema.
        crate::compaction::ensure_schema(conn)?;
        // The retained plan-read receipt table stays governed so no consumer
        // can rewrite rows an older release recorded. It carries no FOREIGN KEY
        // into event/event_sequence, which keeps it outside any event-table
        // rebuild.
        crate::retired_campaign::ensure_plan_read_table(conn)?;
        // The seat mailbox IS governed for the same reason — durable
        // operational truth, not a projection — and is prepared in the same
        // transaction so a database can never be Rust-owned while missing the
        // place its coordination mail lands. It carries no FOREIGN KEY into
        // event/event_sequence either: a received message is not a fact-log
        // child, and FK-freedom keeps it outside any later event rebuild.
        crate::mailbox::ensure_schema(conn)?;
        // The attempt receipt IS governed for the third time and the same
        // reason — durable operational truth, not a projection — and is prepared
        // in the same transaction so a database can never be Rust-owned while
        // missing the place its process-loss evidence lands. It carries no
        // FOREIGN KEY into event/event_sequence either: an operational receipt
        // is not a fact-log child, and FK-freedom keeps it outside the forced
        // event rebuild the production migration performs.
        crate::attempt_receipt::ensure_schema(conn)?;
        // The receipt quarantine is prepared in the same transaction as the
        // table it drains. A database that could hold receipts with nowhere to
        // durably report an untrustworthy one would force a reconciler either
        // to skip the row or to keep the debt in memory, and both are exactly
        // what R2C5I ruling 5 forbids.
        crate::attempt_receipt::ensure_quarantine_schema(conn)?;
        // CREATE IF NOT EXISTS silently accepts a preexisting INCOMPATIBLE
        // table; verify the shape so an alien schema refuses the open inside
        // the same transaction (review round 2, item E). The fencing columns
        // are required here too, so a database written under the pre-fencing
        // shape refuses to open rather than silently appending unfenced rows.
        //
        // The ORDER here is load-bearing. Two later statements also name the
        // fencing columns — the unique index over
        // `(aggregate_family, aggregate_id, seq)` and the trigger's
        // `NEW.writer_epoch` — so on a pre-fencing table SQLite would refuse
        // one of those first and the refusal would describe an index or a
        // trigger rather than the missing column. Three mechanisms reaching
        // the same verdict for different reasons is exactly how a witness ends
        // up passing for the wrong reason, so the shape is verified before
        // anything is built on top of it, and the witness asserts its message.
        Self::verify_columns(
            conn,
            "event_sequence",
            &["aggregate_family", "aggregate_id", "seq"],
        )?;
        Self::verify_columns(
            conn,
            "aggregate_writer",
            &["aggregate_family", "aggregate_id", "owner_id", "epoch"],
        )?;
        Self::verify_columns(
            conn,
            "projection_watermark",
            &["projection", "aggregate_family", "aggregate_id", "seq"],
        )?;
        Self::verify_columns(
            conn,
            "event",
            &[
                "id",
                "aggregate_family",
                "aggregate_id",
                "seq",
                "created",
                "type",
                "data",
                "writer_epoch",
                "envelope_bytes",
                "campaign_transition_id",
            ],
        )?;
        // CREATE IF NOT EXISTS honours a wrong-shaped namesake silently, and
        // the graph records a live hazard of exactly that class for the legacy
        // event indexes — so the new table's shape is asserted rather than
        // assumed, like every governed table above.
        Self::verify_columns(
            conn,
            "plan_read_receipt",
            &["receipt_id", "campaign_id", "recorded", "record_bytes"],
        )?;
        Self::verify_columns(
            conn,
            "campaign_plan_artifact",
            &[
                "campaign_id",
                "plan_version",
                "artifact_id",
                "artifact_length",
                "artifact_digest",
                "graph_digest",
                "versioned_sequence",
            ],
        )?;
        // Same hazard, same order, for the mailbox: its columns are asserted
        // before either of its indexes may name them.
        Self::verify_columns(
            conn,
            "mailbox_message",
            &[
                "message_id",
                "recipient",
                "admission_seq",
                "kind",
                "supersedes",
                "source_acknowledged",
                "consumed",
                "record_bytes",
            ],
        )?;
        // Same hazard, same order, for the attempt receipt. `writer_epoch` is
        // asserted here like every other column because it is a stored column,
        // not merely a value the record happens to carry.
        Self::verify_columns(
            conn,
            "attempt_receipt",
            &[
                "attempt_id",
                "session_id",
                "step",
                "ordinal",
                "writer_epoch",
                "terminal_event_id",
                "terminal_created_ms",
                "reserved_ms",
                "request_sent_ms",
                "settled_ms",
                "record_bytes",
            ],
        )?;
        Self::verify_columns(
            conn,
            "attempt_receipt_quarantine",
            &[
                "attempt_id",
                "session_id",
                "step",
                "ordinal",
                "writer_epoch",
                "terminal_event_id",
                "terminal_created_ms",
                "reserved_ms",
                "request_sent_ms",
                "settled_ms",
                "record_bytes",
                "reason",
                "declared_bytes",
                "detected_ms",
                "detected_epoch",
            ],
        )?;
        // Coverage seeding reads receipt columns only after their shape is proven.
        crate::provider_attempt_accounting::ensure_schema(conn)?;
        Self::verify_columns(
            conn,
            "provider_attempt_accounting",
            &["attempt_id", "session_id", "record_bytes"],
        )?;
        Self::verify_columns(
            conn,
            "provider_attempt_coverage",
            &["session_id", "complete"],
        )?;
        // Only now may the page-read index name those columns; on an alien
        // namesake the check above got first refusal.
        crate::retired_campaign::ensure_indexes(conn)?;
        crate::mailbox::ensure_indexes(conn)?;
        crate::attempt_receipt::ensure_index(conn)?;
        crate::provider_attempt_accounting::ensure_index(conn)?;
        // Column PRESENCE is all the check above can see. `PRAGMA table_info`
        // reports names, declared types, notnull and pk — it cannot report a
        // CHECK constraint at all, and it must NOT be used to pre-refuse the
        // tagged union's payload nullability either, or the arm that is
        // supposed to attribute an old `data TEXT NOT NULL` table would never
        // be reached and the refusal would be recorded against the wrong
        // mechanism. The semantic probe below is the SOLE carrier for
        // data-nullability and family-shape CHECK behaviour.
        let probe_start = conn.total_changes();
        Self::probe_target_shape(conn)?;
        let rolled_back = conn.total_changes() - probe_start;
        // Session-list persistence is raw store schema only; `grapt-session`
        // remains the semantic owner. A legacy `session` table without this
        // projection is refused here and must pass through ProductionMigrations
        // rather than open an `empty-rust-store` projection over legacy rows.
        //
        // It runs AFTER the fact-log shape checks above, and the order is
        // load-bearing for the same reason theirs is. A pre-fencing or alien
        // event/watermark table is very often accompanied by a legacy `session`
        // table — that pairing IS what an oracle-era database looks like —
        // so a session-list refusal placed earlier would answer first and every
        // one of those column checks would be recorded as enforced while never
        // being reached. The fact log proves its own shape first; only then is
        // the projection built on top of it judged.
        crate::session_list::ensure_schema(conn, budget)?;
        // The no-new-v1 gate, enforced by SQLite rather than by convention.
        //
        // `writer_epoch IS NULL` is exactly `EventFencing::LegacyUnfenced`, and
        // the law is that no new unfenced event may be WRITTEN. The refusal is
        // deliberately not a Rust-side check in `append_within`: a Rust check
        // proves only that the store's own append path behaves, while this
        // aborts any INSERT from any statement on this connection, including
        // raw SQL that never touched the append path at all.
        //
        // It does not obstruct legacy import. An oracle-era database already CONTAINS
        // its v1 rows; migrating it adds the column to rows that are already
        // there (`ALTER TABLE ... ADD COLUMN`, leaving them NULL and their bytes
        // untouched) and inserts nothing, so the trigger never fires on the
        // import path. See `F3B1A_RESIDUALS.md`.
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS event_refuses_new_unfenced_write
             BEFORE INSERT ON event
             WHEN NEW.writer_epoch IS NULL
             BEGIN
                 SELECT RAISE(ABORT, 'no new v1 write: a new event row must carry its elected writer epoch');
             END;
             CREATE UNIQUE INDEX IF NOT EXISTS event_aggregate_seq_idx
                 ON event (aggregate_family, aggregate_id, seq);
             CREATE INDEX IF NOT EXISTS event_aggregate_type_seq_idx
                 ON event (aggregate_family, aggregate_id, type, seq);
             -- Transition identity is unique WITHIN its campaign, never
             -- globally: two campaigns may legitimately mint the same
             -- transition identity, and a global unique index would refuse the
             -- second one as a duplicate of a fact it has nothing to do with.
             -- PARTIAL so the session family, whose column is always NULL, is
             -- not indexed at all: SQLite treats NULLs as distinct, but an
             -- unfiltered index would still carry a row per session event for
             -- no law.
             CREATE UNIQUE INDEX IF NOT EXISTS event_campaign_transition_idx
                 ON event (aggregate_family, aggregate_id, campaign_transition_id)
                 WHERE campaign_transition_id IS NOT NULL;
             -- The carry enumeration (content_refs) filters on the event
             -- type alone and orders by id; only a type-leading index can
             -- serve that predicate, so the per-launch admission scan stays
             -- bounded by the matching rows, never by the table.
             CREATE INDEX IF NOT EXISTS event_type_id_idx
                 ON event (type, id);",
        )?;
        crate::migrations::ensure_permission_http_schema(conn)?;
        crate::permission_revocation::ensure_schema(conn)?;
        crate::plugin_compatibility::ensure_schema(conn)?;
        crate::mcp_credential::ensure_schema(conn)?;
        Ok(rolled_back)
    }

    /// The reserved identity every schema-probe row is written under.
    ///
    /// It is deliberately not a parseable `SessionId` or `AgentCampaignId`, so
    /// no real aggregate can ever occupy it. Occupancy is therefore corruption
    /// or a foreign writer, never a collision with legitimate data — which is
    /// why the precondition below REFUSES instead of overwriting.
    const PROBE_IDENTITY: &'static str = "grapt-store schema probe (reserved)";

    /// Proves the `event` table's TARGET shape by executing it.
    ///
    /// Runs inside `ensure_schema`'s uncommitted IMMEDIATE transaction, before
    /// the trigger/index batch, and every arm is contained in its own
    /// savepoint that is unconditionally rolled back — so no probe row can
    /// survive even a fully successful open, and a successful positive arm
    /// cannot leave state that changes a later arm's verdict.
    ///
    /// Arm ORDER is load-bearing. The valid-campaign arm runs FIRST because it
    /// is the only arm that can attribute a retained `data TEXT NOT NULL`: on
    /// an old-shape table it is the insert that refuses with NOT NULL. Were
    /// the invalid-session arm to run first, that same old table would refuse
    /// THERE with `SQLITE_CONSTRAINT_NOTNULL` where the arm asserts
    /// `SQLITE_CONSTRAINT_CHECK`, and the open would fail naming the wrong
    /// mechanism.
    ///
    /// Every probe row carries a NON-NULL `writer_epoch`. On a preexisting
    /// database the no-new-v1 trigger is already installed, and statement
    /// order inside this function cannot keep it out of the way: a NULL-epoch
    /// probe row would abort with the no-v1 message and steal the attribution
    /// from whichever arm was actually being proved.
    fn probe_target_shape(conn: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
        Self::ensure_probe_namespace_indexes(conn)?;
        Self::verify_probe_namespace_unoccupied(conn)?;
        // 1. VALID CAMPAIGN — the sole attribution for a retained
        //    `data NOT NULL`, so it runs before anything else.
        Self::probe_arm(
            conn,
            ProbeArm {
                name: "valid-campaign",
                family: "campaign",
                data: None,
                envelope: Some(b"probe"),
                transition: Some("probe-transition"),
                expect: ProbeExpectation::Accepts,
            },
        )?;
        // 2. VALID SESSION — the legacy row contract still writes.
        Self::probe_arm(
            conn,
            ProbeArm {
                name: "valid-session",
                family: "session",
                data: Some("{}"),
                envelope: None,
                transition: None,
                expect: ProbeExpectation::Accepts,
            },
        )?;
        // 3. INVALID SESSION — a session row with no payload.
        Self::probe_arm(
            conn,
            ProbeArm {
                name: "invalid-session",
                family: "session",
                data: None,
                envelope: None,
                transition: None,
                expect: ProbeExpectation::RefusesFamilyShape,
            },
        )?;
        // 4. INVALID CAMPAIGN — both payloads at once, the second
        //    representation the tagged union exists to forbid.
        Self::probe_arm(
            conn,
            ProbeArm {
                name: "invalid-campaign",
                family: "campaign",
                data: Some("{}"),
                envelope: Some(b"probe"),
                transition: Some("probe-transition"),
                expect: ProbeExpectation::RefusesFamilyShape,
            },
        )
    }

    /// Auxiliary admission indexes are prepared once under the existing owned
    /// schema transaction. Keep the original case-insensitive prefix protection
    /// without scanning unrelated event history on each open.
    pub(super) fn ensure_probe_namespace_indexes(
        conn: &rusqlite::Transaction<'_>,
    ) -> Result<(), rusqlite::Error> {
        for (index, table, column, collation) in [
            (
                "event_probe_aggregate_idx",
                "event",
                "aggregate_id",
                "BINARY",
            ),
            ("event_probe_id_idx", "event", "id", "NOCASE"),
            (
                "event_sequence_probe_aggregate_idx",
                "event_sequence",
                "aggregate_id",
                "BINARY",
            ),
        ] {
            conn.execute(
                &format!(
                    "CREATE INDEX IF NOT EXISTS {index} ON {table} ({column} COLLATE {collation})"
                ),
                [],
            )?;
            let shape: Vec<(Option<String>, Option<String>, i64)> = conn
                .prepare(
                    "SELECT CASE WHEN length(CAST(name AS BLOB))<=128 THEN name END,
                        CASE WHEN length(CAST(coll AS BLOB))<=128 THEN coll END, desc
                 FROM pragma_index_xinfo(?1) WHERE key=1 ORDER BY seqno LIMIT 2",
                )?
                .query_map([index], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<Result<_, _>>()?;
            let properties: Option<(i64, i64)> = conn
                .query_row(
                    "SELECT \"unique\", partial FROM pragma_index_list(?1) WHERE name=?2",
                    rusqlite::params![table, index],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if shape.as_slice() != [(Some(column.to_owned()), Some(collation.to_owned()), 0)]
                || properties != Some((0, 0))
            {
                return Err(Self::shape_refusal(&format!(
                    "schema-probe admission index {index} has an incompatible shape"
                )));
            }
        }
        Ok(())
    }

    /// Refuses when anything already occupies the reserved probe namespace.
    ///
    /// Checked in BOTH tables, and in `event` by BOTH keys the probe would
    /// touch: the row id and the `(family, aggregate_id, seq)` coordinates the
    /// unique index covers. An occupied namespace is a typed schema refusal —
    /// the probe never overwrites, because deleting a row it did not write
    /// would destroy exactly the evidence that something is wrong.
    fn verify_probe_namespace_unoccupied(
        conn: &rusqlite::Transaction<'_>,
    ) -> Result<(), rusqlite::Error> {
        let occupied_parent: i64 = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM event_sequence INDEXED BY event_sequence_probe_aggregate_idx WHERE aggregate_id = ?1)",
            [Self::PROBE_IDENTITY],
            |r| r.get(0),
        )?;
        let occupied_event: i64 = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM event INDEXED BY event_probe_aggregate_idx WHERE aggregate_id = ?1)
                 OR EXISTS(SELECT 1 FROM event INDEXED BY event_probe_id_idx WHERE id LIKE ?2)",
            rusqlite::params![Self::PROBE_IDENTITY, "grapt-store schema probe%"],
            |r| r.get(0),
        )?;
        if occupied_parent != 0 || occupied_event != 0 {
            return Err(Self::shape_refusal(&format!(
                "reserved schema-probe namespace is occupied \
                 (sequence occupied={occupied_parent}, event occupied={occupied_event}): \
                 refusing rather than overwriting evidence of a foreign writer"
            )));
        }
        Ok(())
    }

    /// Runs one arm inside its own unconditionally-rolled-back savepoint.
    fn probe_arm(
        conn: &rusqlite::Transaction<'_>,
        arm: ProbeArm<'_>,
    ) -> Result<(), rusqlite::Error> {
        conn.execute_batch("SAVEPOINT grapt_schema_probe")?;
        let outcome = Self::probe_arm_within(conn, &arm);
        // Unconditional: the rollback runs on the accepting arms too, so a
        // successful probe leaves the database byte-identical.
        conn.execute_batch("ROLLBACK TO grapt_schema_probe; RELEASE grapt_schema_probe")?;
        outcome
    }

    fn probe_arm_within(
        conn: &rusqlite::Transaction<'_>,
        arm: &ProbeArm<'_>,
    ) -> Result<(), rusqlite::Error> {
        // The parent cursor is staged before the event row, so the probe is
        // correct whether `PRAGMA foreign_keys` is ON or OFF, and it proves
        // nothing ABOUT the foreign key either way; see F3B3_RESIDUALS.md.
        //
        // The claim this comment used to make — that enforcement "is off by
        // default" — was wrong about which default. SQLite's own engine default
        // is off, but `rusqlite` turns foreign keys ON for these connections,
        // and since F3B4b `EventStore` sets and verifies the pragma itself
        // before any transaction. So on every connection this probe actually
        // runs on, enforcement is ON. Its independence from that state is real
        // and unchanged; only the reason given for it was false.
        conn.execute(
            "INSERT INTO event_sequence (aggregate_family, aggregate_id, seq)
             VALUES (?1, ?2, 0)",
            rusqlite::params![arm.family, Self::PROBE_IDENTITY],
        )?;
        let inserted = conn.execute(
            "INSERT INTO event
                (id, aggregate_family, aggregate_id, seq, created, type,
                 data, writer_epoch, envelope_bytes, campaign_transition_id)
             VALUES (?1, ?2, ?3, 0, 0, 'probe', ?4, 1, ?5, ?6)",
            rusqlite::params![
                format!("grapt-store schema probe {}", arm.name),
                arm.family,
                Self::PROBE_IDENTITY,
                arm.data,
                arm.envelope,
                arm.transition,
            ],
        );
        match (&arm.expect, inserted) {
            (ProbeExpectation::Accepts, Ok(_)) => Ok(()),
            (ProbeExpectation::Accepts, Err(error)) => Err(Self::shape_refusal(&format!(
                "incompatible preexisting event table: the {} probe row was refused ({error}); \
                 the target shape stores campaign envelope bytes with a NULL `data`, \
                 so a table whose `data` is still NOT NULL cannot hold one. \
                 Migration is F3B4's; this open refuses rather than writing into the old shape",
                arm.name
            ))),
            (ProbeExpectation::RefusesFamilyShape, Ok(_)) => Err(Self::shape_refusal(&format!(
                "incompatible preexisting event table: the {} probe row was ACCEPTED; \
                 the `event_family_shape` constraint is missing or too weak to refuse a row \
                 carrying the wrong payload for its family",
                arm.name
            ))),
            (ProbeExpectation::RefusesFamilyShape, Err(error)) => {
                if Self::is_named_family_shape_violation(&error) {
                    Ok(())
                } else {
                    // A refusal for some OTHER reason is not proof of this
                    // arm: a UNIQUE, NOT NULL or trigger abort would leave the
                    // family-shape constraint entirely unproven while looking
                    // like a pass.
                    Err(Self::shape_refusal(&format!(
                        "incompatible preexisting event table: the {} probe row was refused, \
                         but not by the named `event_family_shape` constraint ({error})",
                        arm.name
                    )))
                }
            }
        }
    }

    /// True only for a CHECK violation naming `event_family_shape`.
    ///
    /// Both halves matter: the extended result code alone would accept ANY
    /// check constraint the table happens to carry, and the message alone
    /// would accept a different constraint class that mentioned the name.
    fn is_named_family_shape_violation(error: &rusqlite::Error) -> bool {
        match error {
            rusqlite::Error::SqliteFailure(code, Some(message)) => {
                code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_CHECK
                    && message.contains("event_family_shape")
            }
            _ => false,
        }
    }

    fn shape_refusal(reason: &str) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
            Some(reason.to_owned()),
        )
    }

    fn verify_columns(
        conn: &rusqlite::Transaction<'_>,
        table: &str,
        required: &[&str],
    ) -> Result<(), rusqlite::Error> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let present = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        for column in required {
            if !present.iter().any(|c| c == column) {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
                    Some(format!(
                        "incompatible preexisting table {table}: missing column {column}"
                    )),
                ));
            }
        }
        Ok(())
    }
}
