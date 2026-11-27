//! The production migration set: an oracle-era database becomes the
//! family-tagged target shape this crate opens.
//!
//! # Why a full table rebuild, and why it is not a preference
//!
//! `F3B1A_RESIDUALS.md` item 8 describes an `ALTER TABLE ... ADD COLUMN`
//! import, and for the fencing columns alone that would be enough. It does not
//! reach the target shape, and the reason is measured on the frozen sanitized
//! clone rather than reasoned from the schema:
//!
//! 1. `data` is `TEXT NOT NULL` in the TypeScript era, and the target's
//!    `event_family_shape` CHECK requires `data IS NULL` for a campaign row.
//!    SQLite can neither drop a `NOT NULL` nor add a table-level CHECK.
//! 2. `event_sequence`'s primary key changes from `(aggregate_id)` to
//!    `(aggregate_family, aggregate_id)`.
//! 3. The foreign key widens from one column to two.
//!
//! Any one of those forces a rebuild; all three are present. A rebuild also
//! dissolves a fourth hazard for free: the TypeScript era already carries
//! indexes named `event_aggregate_seq_idx` and `event_aggregate_type_seq_idx`
//! over family-BLIND column lists, and `CREATE INDEX IF NOT EXISTS` honours a
//! wrong-shaped namesake silently. Dropping the table drops its indexes, so the
//! recreated ones are the target's.
//!
//! # Statement order is load-bearing three times over
//!
//! Each of these was measured against a real SQLite, not inferred:
//!
//! * **The parent's rows are copied before the child's.** The rebuilt `event`
//!   declares a composite foreign key into the rebuilt `event_sequence`, and
//!   enforcement is on for this connection, so copying children first fails
//!   loudly with `FOREIGN KEY constraint failed`.
//! * **The child's rows are copied before the old parent is dropped.** This is
//! the dangerous one, because getting it wrong is SILENT. The oracle-era
//!   foreign key is `ON DELETE CASCADE`, and with enforcement on, `DROP TABLE
//!   event_sequence` performs an implicit `DELETE FROM` that cascades into
//!   `event`. Dropping the parent first therefore leaves an empty source,
//!   copies nothing, and COMMITS a migration reporting success over an empty
//!   log. Measured: two rows in, zero rows out, no error raised.
//! * **The no-new-v1 trigger is created after the data move.** Legacy rows are
//!   `LegacyUnfenced` and carry a NULL `writer_epoch` by definition, and that
//!   INSERT is exactly what the trigger aborts. Created first, it refuses the
//!   import it exists to permit.
//!
//! The migration cannot sidestep any of this by turning enforcement off:
//! `PRAGMA foreign_keys` is a documented no-op inside an open transaction, and
//! every [`Migration::up`] runs inside one. Measured: set to `OFF` inside a
//! transaction it still reads back 1, both during and after. Order is the only
//! available defence, which is why each of the three carries its own witness.
//!
//! # What is preserved, exactly
//!
//! Every legacy row keeps its `id`, `aggregate_id`, `seq`, `created`, `type`
//! and `data` bytes verbatim. Nothing is re-encoded, re-hashed or re-chained:
//! the writer-epoch law makes imported v1 events `LegacyUnfenced`, retaining
//! their original canonical bytes and hashes, and a migration that normalised a
//! payload would silently change the fact it claims to carry forward.
//! Sequences are monotonic-with-GAPS in the real corpus — a measured `1053 ->
//! 1075` jump — and gaps are carried, never compacted.

use crate::migration::{Migration, MigrationError};
use grapt_types::MigrationId;

/// The aggregate family every oracle-era aggregate belongs to.
///
/// Measured on the frozen sanitized clone: all 305 legacy aggregates are
/// sessions, and the campaign family has no v1 history. The back-fill is a
/// single named constant so that ONE edit changes both copy statements
/// together. Spelling it twice would make a wrong value fail as a foreign-key
/// mismatch between the two rebuilt tables — a loud refusal for the wrong
/// reason, instead of the silent misfiling this law is actually about.
pub(crate) const LEGACY_FAMILY: &str = "session";

const REBUILD_ID: &str = "20260811_rebuild_event_tables_family_tagged";
const RUST_ERA_ID: &str = "20260811_create_rust_era_tables";
const SESSION_LIST_ID: &str = "20260813_create_session_list_projection";
const PERMISSION_HTTP_ID: &str = "20260829_create_permission_http_state";
const PLUGIN_COMPATIBILITY_ID: &str = "20260916_create_plugin_compatibility_state";
const EVENT_TYPE_INDEX_ID: &str = "20260920_create_event_type_id_index";

/// Rebuilds `event` and `event_sequence` into the family-tagged target shape.
#[derive(Debug)]
pub struct RebuildEventTablesForFamilyTaggedRows {
    id: MigrationId,
}

/// Creates the Rust-era authority and projection tables.
#[derive(Debug)]
pub struct CreateRustEraTables {
    id: MigrationId,
}

/// Creates and imports the bounded session-list projection after the event log
/// has acquired family tags and the watermark table exists.
#[derive(Debug)]
pub struct CreateSessionListProjection {
    id: MigrationId,
}

/// Creates the durable permission HTTP adjunct and its saved-rule state.
#[derive(Debug)]
pub struct CreatePermissionHttpState {
    id: MigrationId,
}

/// Creates plugin credential ownership state, and the legacy credential and kv
/// tables when an imported or new store does not already have them.
#[derive(Debug)]
pub struct CreatePluginCompatibilityState {
    id: MigrationId,
}

/// Gives the carry enumeration its bounded access path: the type-leading
/// index whose leading column is the query's only predicate.
#[derive(Debug)]
pub struct CreateEventTypeIndex {
    id: MigrationId,
}

/// The production migration set, in order.
///
/// # Why the constructor is fallible
///
/// [`MigrationId::new`] returns an `Option` because the empty string is not an
/// identity, and this crate denies `unwrap`/`expect`. The literals above are
/// non-empty, so the `None` arm is unreachable — but resolving an unreachable
/// branch by assertion is precisely what that deny exists to prevent. The
/// branch is REPRESENTED instead: it becomes a typed
/// [`MigrationError::InvalidMigrationIdentity`], so the impossible case refuses
/// to migrate rather than aborting the process.
#[derive(Debug)]
pub struct ProductionMigrations {
    rebuild: RebuildEventTablesForFamilyTaggedRows,
    rust_era: CreateRustEraTables,
    session_list: CreateSessionListProjection,
    permission_http: CreatePermissionHttpState,
    plugin_compatibility: CreatePluginCompatibilityState,
    event_type_index: CreateEventTypeIndex,
    permission_revocations: crate::permission_revocation::CreatePermissionRuleRevocations,
}

impl ProductionMigrations {
    pub fn load() -> Result<Self, MigrationError> {
        Ok(Self {
            rebuild: RebuildEventTablesForFamilyTaggedRows {
                id: identity(REBUILD_ID)?,
            },
            rust_era: CreateRustEraTables {
                id: identity(RUST_ERA_ID)?,
            },
            session_list: CreateSessionListProjection {
                id: identity(SESSION_LIST_ID)?,
            },
            permission_http: CreatePermissionHttpState {
                id: identity(PERMISSION_HTTP_ID)?,
            },
            plugin_compatibility: CreatePluginCompatibilityState {
                id: identity(PLUGIN_COMPATIBILITY_ID)?,
            },
            event_type_index: CreateEventTypeIndex {
                id: identity(EVENT_TYPE_INDEX_ID)?,
            },
            permission_revocations: crate::permission_revocation::CreatePermissionRuleRevocations {
                id: identity("20261101_create_permission_rule_revocations")?,
            },
        })
    }

    /// The set in the order it must be applied.
    pub fn as_slice(&self) -> [&dyn Migration; 7] {
        [
            &self.rebuild,
            &self.rust_era,
            &self.session_list,
            &self.permission_http,
            &self.plugin_compatibility,
            &self.event_type_index,
            &self.permission_revocations,
        ]
    }
}

fn identity(literal: &'static str) -> Result<MigrationId, MigrationError> {
    MigrationId::new(literal).ok_or(MigrationError::InvalidMigrationIdentity { literal })
}

/// Which of the target columns a source table already carries.
///
/// A migration must be honest about what it finds. A pure oracle-era table
/// has none of them and every row is back-filled to [`LEGACY_FAMILY`]. A table
/// that already carries `aggregate_family` — a database part-way to the target
/// shape, or a law-conformance database carrying campaign history — has its
/// families PRESERVED, because overwriting them would refile campaign rows as
/// sessions and then fail the family CHECK on the migration's own output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct SourceColumns {
    family: bool,
    writer_epoch: bool,
    envelope_bytes: bool,
    campaign_transition_id: bool,
}

impl SourceColumns {
    fn read(tx: &rusqlite::Transaction<'_>, table: &str) -> Result<Self, String> {
        let present = columns_of(tx, table)?;
        let has = |name: &str| present.iter().any(|c| c == name);
        Ok(Self {
            family: has("aggregate_family"),
            writer_epoch: has("writer_epoch"),
            envelope_bytes: has("envelope_bytes"),
            campaign_transition_id: has("campaign_transition_id"),
        })
    }

    /// The `aggregate_family` expression for a copy out of this source.
    fn family_expr(self) -> String {
        if self.family {
            "aggregate_family".to_owned()
        } else {
            format!("'{LEGACY_FAMILY}'")
        }
    }
}

fn column_or_null(present: bool, name: &str) -> &str {
    if present {
        name
    } else {
        "NULL"
    }
}

fn columns_of(tx: &rusqlite::Transaction<'_>, table: &str) -> Result<Vec<String>, String> {
    let mut statement = tx
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    for row in rows {
        names.push(row.map_err(|e| e.to_string())?);
    }
    Ok(names)
}

fn table_exists(tx: &rusqlite::Transaction<'_>, table: &str) -> Result<bool, String> {
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(count != 0)
}

impl Migration for RebuildEventTablesForFamilyTaggedRows {
    fn id(&self) -> MigrationId {
        self.id.clone()
    }

    fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
        if !table_exists(tx, "event")? {
            // Nothing to carry forward. The store's own `ensure_schema` builds
            // the target tables on first open, and a second copy of that DDL
            // for an empty database would be a redundant carrier.
            return Ok(());
        }
        let source = SourceColumns::read(tx, "event")?;
        let parent = SourceColumns::read(tx, "event_sequence")?;

        // 1. The target tables, under temporary names, so both old tables stay
        //    readable until every row has been copied out of them.
        tx.execute_batch(REBUILT_TABLES)
            .map_err(|e| e.to_string())?;

        // 2. PARENT FIRST. The rebuilt child's composite foreign key resolves
        //    against these rows, so copying the child first fails loudly.
        tx.execute_batch(&format!(
            "INSERT INTO event_sequence_rebuilt (aggregate_family, aggregate_id, seq, owner_id)
             SELECT {}, aggregate_id, seq, owner_id FROM event_sequence",
            parent.family_expr()
        ))
        .map_err(|e| e.to_string())?;

        // 3. CHILD, BEFORE THE OLD PARENT IS DROPPED. Payload columns are
        //    copied verbatim: no re-encode, no re-hash, no normalisation.
        tx.execute_batch(&format!(
            "INSERT INTO event_rebuilt
                 (id, aggregate_family, aggregate_id, seq, created, type,
                  data, writer_epoch, envelope_bytes, campaign_transition_id)
             SELECT id, {}, aggregate_id, seq, created, type,
                    data, {}, {}, {}
             FROM event",
            source.family_expr(),
            column_or_null(source.writer_epoch, "writer_epoch"),
            column_or_null(source.envelope_bytes, "envelope_bytes"),
            column_or_null(source.campaign_transition_id, "campaign_transition_id"),
        ))
        .map_err(|e| e.to_string())?;

        interrupt_between_rebuild_steps()?;

        // 4. Only now may the old tables go.
        tx.execute_batch(
            "DROP TABLE event;
             DROP TABLE event_sequence;
             ALTER TABLE event_sequence_rebuilt RENAME TO event_sequence;
             ALTER TABLE event_rebuilt RENAME TO event;",
        )
        .map_err(|e| e.to_string())?;

        // 5. The target indexes. The old family-blind namesakes went with the
        //    table they belonged to, so these names are free and these column
        //    lists are the ones that survive.
        tx.execute_batch(REBUILT_INDEXES)
            .map_err(|e| e.to_string())?;

        // 6. LAST: the no-new-v1 gate. Every row copied above is
        //    `LegacyUnfenced` with a NULL `writer_epoch`, which is precisely
        //    what this trigger aborts. It may only exist once the import is
        //    already durable.
        tx.execute_batch(NO_NEW_V1_TRIGGER)
            .map_err(|e| e.to_string())
    }
}

impl Migration for CreateRustEraTables {
    fn id(&self) -> MigrationId {
        self.id.clone()
    }

    fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
        // The store's OWN schema functions, called rather than restated: a
        // second copy of this DDL here would be a redundant carrier, and no
        // single edit could then attribute either one.
        crate::election::ensure_schema(tx).map_err(|e| e.to_string())?;
        crate::projection::ensure_schema(tx).map_err(|e| e.to_string())?;
        crate::retired_campaign::ensure_phase_table(tx).map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl Migration for CreateSessionListProjection {
    fn id(&self) -> MigrationId {
        self.id.clone()
    }

    fn validate(&self, conn: &rusqlite::Connection) -> Result<(), String> {
        crate::session_list::validate_legacy_source(conn)
    }

    fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
        crate::session_list::import_legacy(tx)
    }
}

impl Migration for CreatePermissionHttpState {
    fn id(&self) -> MigrationId {
        self.id.clone()
    }

    fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
        migrate_permission_http_primary(tx)?;
        interrupt_between_permission_http_steps()?;
        tx.execute_batch(PERMISSION_HTTP_ADJUNCT_SCHEMA)
            .map_err(|error| error.to_string())
    }
}

impl Migration for CreatePluginCompatibilityState {
    fn id(&self) -> MigrationId {
        self.id.clone()
    }

    fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
        crate::plugin_compatibility::ensure_schema(tx).map_err(|error| error.to_string())
    }
}

impl Migration for CreateEventTypeIndex {
    fn id(&self) -> MigrationId {
        self.id.clone()
    }

    fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
        if !table_exists(tx, "event")? {
            // A brand-new file: the rebuild migration found nothing to carry
            // forward, so the event table does not exist yet - the store's
            // own `ensure_schema` builds it, with this index, at first open.
            return Ok(());
        }
        // The carry enumeration's query filters on the event type alone and
        // orders by id, so only a type-leading index serves it. For an
        // existing database that runs through the full set, the rebuild
        // migration drops the event table with its indexes, so THIS migration
        // is what gives the rebuilt table the bounded access path; on an
        // already-current database the `IF NOT EXISTS` is a no-op against the
        // index `ensure_schema` shaped. A wrong-shaped namesake is refused at
        // open by the REQUIRED_INDEXES shape check before this migration can
        // matter, so no silent namesake is honoured here either.
        tx.execute_batch("CREATE INDEX IF NOT EXISTS event_type_id_idx ON event (type, id);")
            .map_err(|error| error.to_string())
    }
}

/// The new-database and existing-database paths share one target DDL. The
/// migration journal remains the authority for an existing database; the
/// store schema path only gives a brand-new database the current target shape.
pub(crate) fn ensure_permission_http_schema(
    tx: &rusqlite::Transaction<'_>,
) -> Result<(), rusqlite::Error> {
    tx.execute_batch(PERMISSION_HTTP_PRIMARY_SCHEMA)?;
    tx.execute_batch(PERMISSION_HTTP_ADJUNCT_SCHEMA)
}

const PERMISSION_HTTP_PRIMARY_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS permission_request (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    action TEXT NOT NULL,
    status TEXT NOT NULL,
    suspended_reason TEXT,
    reply TEXT
);
CREATE TABLE IF NOT EXISTS permission_http_rule (
    project_id TEXT NOT NULL,
    action TEXT NOT NULL,
    resource TEXT NOT NULL,
    time_created INTEGER NOT NULL,
    PRIMARY KEY (project_id, action, resource)
);";

const PERMISSION_HTTP_ADJUNCT_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS permission_http_request (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    resources_json TEXT NOT NULL,
    save_json TEXT,
    metadata_json TEXT,
    source_json TEXT,
    owner_subject TEXT NOT NULL,
    owner_client TEXT NOT NULL,
    FOREIGN KEY(id) REFERENCES permission_request(id)
);";

const CURRENT_PERMISSION_REQUEST_COLUMNS: &[&str] = &[
    "id",
    "session_id",
    "action",
    "status",
    "suspended_reason",
    "reply",
];

const TYPESCRIPT_PERMISSION_REQUEST_COLUMNS: &[&str] = &[
    "id",
    "session_id",
    "agent",
    "action",
    "resources",
    "save",
    "metadata",
    "source",
    "status",
    "reply",
    "suspended_reason",
    "waiter_epoch",
    "time_created",
    "time_updated",
];

fn exact_columns(observed: &[String], expected: &[&str]) -> bool {
    observed
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
}

fn migrate_permission_http_primary(tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
    let columns = columns_of(tx, "permission_request")?;
    if columns.is_empty() || exact_columns(&columns, CURRENT_PERMISSION_REQUEST_COLUMNS) {
        return tx
            .execute_batch(PERMISSION_HTTP_PRIMARY_SCHEMA)
            .map_err(|error| error.to_string());
    }
    if !exact_columns(&columns, TYPESCRIPT_PERMISSION_REQUEST_COLUMNS) {
        return Err(format!(
            "permission_request has an unsupported pre-migration shape: {columns:?}"
        ));
    }
    tx.execute_batch("ALTER TABLE permission_request RENAME TO permission_request_ts_legacy;")
        .map_err(|error| error.to_string())?;
    tx.execute_batch(PERMISSION_HTTP_PRIMARY_SCHEMA)
        .map_err(|error| error.to_string())?;
    tx.execute_batch(
        "INSERT INTO permission_request
           (id, session_id, action, status, suspended_reason, reply)
         SELECT id, session_id, action, status, suspended_reason, reply
           FROM permission_request_ts_legacy;
         DROP TABLE permission_request_ts_legacy;",
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
thread_local! {
    static INTERRUPT_PERMISSION_HTTP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn interrupt_between_permission_http_steps() -> Result<(), String> {
    if INTERRUPT_PERMISSION_HTTP.with(std::cell::Cell::get) {
        return Err("interrupted between permission HTTP migration steps".to_owned());
    }
    Ok(())
}

#[cfg(not(test))]
fn interrupt_between_permission_http_steps() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod permission_http_migration_tests {
    use super::*;

    #[test]
    fn production_permission_migration_creates_primary_rule_and_adjunct_tables() {
        let mut connection = match rusqlite::Connection::open_in_memory() {
            Ok(connection) => connection,
            Err(error) => panic!("permission migration fixture must open: {error}"),
        };
        let migration = match ProductionMigrations::load() {
            Ok(migrations) => migrations.permission_http,
            Err(error) => panic!("production migrations must load: {error}"),
        };
        let tx = connection
            .transaction()
            .unwrap_or_else(|error| panic!("permission migration transaction must start: {error}"));
        migration
            .up(&tx)
            .unwrap_or_else(|error| panic!("production permission migration must run: {error}"));
        tx.commit()
            .unwrap_or_else(|error| panic!("permission migration must commit: {error}"));
        for table in [
            "permission_request",
            "permission_http_rule",
            "permission_http_request",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap_or_else(|error| panic!("migrated schema must be inspectable: {error}"));
            assert_eq!(count, 1, "production migration must create {table}");
        }
        let project_coordinate: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('permission_http_request')
                 WHERE name = 'project_id' AND [notnull] = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("permission replay coordinate must inspect: {error}"));
        assert_eq!(
            project_coordinate, 1,
            "production migration must own the replayable project coordinate"
        );
    }

    #[test]
    fn production_permission_migration_rebuilds_the_measured_typescript_primary_shape() {
        let mut connection = match rusqlite::Connection::open_in_memory() {
            Ok(connection) => connection,
            Err(error) => panic!("permission migration fixture must open: {error}"),
        };
        if let Err(error) = connection.execute_batch(
            "CREATE TABLE permission_request (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 agent TEXT,
                 action TEXT NOT NULL,
                 resources TEXT NOT NULL,
                 save TEXT,
                 metadata TEXT,
                 source TEXT,
                 status TEXT DEFAULT 'pending' NOT NULL,
                 reply TEXT,
                 suspended_reason TEXT,
                 waiter_epoch TEXT NOT NULL,
                 time_created INTEGER NOT NULL,
                 time_updated INTEGER NOT NULL
             );
             INSERT INTO permission_request VALUES
               ('per_legacy', 'ses_legacy', 'agt_legacy', 'write', '[\"/work\"]',
                '[\"/work/*\"]', NULL, NULL, 'suspended', NULL, 'replacement',
                'epoch-1', 41, 42);",
        ) {
            panic!("measured TypeScript permission shape must install: {error}");
        }
        let migration = match ProductionMigrations::load() {
            Ok(migrations) => migrations.permission_http,
            Err(error) => panic!("production migrations must load: {error}"),
        };
        let tx = match connection.transaction() {
            Ok(tx) => tx,
            Err(error) => panic!("permission migration transaction must start: {error}"),
        };
        if let Err(error) = migration.up(&tx) {
            panic!("measured TypeScript permission shape must migrate: {error}");
        }
        if let Err(error) = tx.commit() {
            panic!("permission migration must commit: {error}");
        }
        let columns = match connection
            .prepare("PRAGMA table_info('permission_request')")
            .and_then(|mut statement| {
                let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
                rows.collect::<Result<Vec<_>, _>>()
            }) {
            Ok(columns) => columns,
            Err(error) => panic!("rebuilt permission shape must be inspectable: {error}"),
        };
        assert!(exact_columns(&columns, CURRENT_PERMISSION_REQUEST_COLUMNS));
        let row: (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        ) = match connection.query_row(
            "SELECT id, session_id, action, status, suspended_reason, reply
                 FROM permission_request",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        ) {
            Ok(row) => row,
            Err(error) => panic!("rebuilt permission row must survive: {error}"),
        };
        assert_eq!(
            row,
            (
                "per_legacy".to_owned(),
                "ses_legacy".to_owned(),
                "write".to_owned(),
                "suspended".to_owned(),
                Some("replacement".to_owned()),
                None,
            )
        );
    }

    #[test]
    fn interrupted_permission_http_schema_rolls_back_every_partial_table() {
        let mut connection = match rusqlite::Connection::open_in_memory() {
            Ok(connection) => connection,
            Err(error) => panic!("permission migration fixture must open: {error}"),
        };
        let migration = match ProductionMigrations::load() {
            Ok(migrations) => migrations.permission_http,
            Err(error) => panic!("production migrations must load: {error}"),
        };
        INTERRUPT_PERMISSION_HTTP.with(|armed| armed.set(true));
        let result = match connection.transaction() {
            Ok(tx) => {
                let result = migration.up(&tx);
                drop(tx);
                result
            }
            Err(error) => panic!("permission migration transaction must start: {error}"),
        };
        INTERRUPT_PERMISSION_HTTP.with(|armed| armed.set(false));
        assert!(
            matches!(
                &result,
                Err(message) if message.contains("interrupted between permission HTTP")
            ),
            "the named interruption must fire, got {result:?}"
        );
        for table in [
            "permission_request",
            "permission_http_rule",
            "permission_http_request",
        ] {
            let count: i64 = match connection.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            ) {
                Ok(count) => count,
                Err(error) => panic!("schema rollback must be inspectable: {error}"),
            };
            assert_eq!(count, 0, "interruption must roll back {table}");
        }
    }
}

// A `cfg(test)`-sealed cut point in the middle of the rebuild.
//
// The crash-cut grid needs an interruption that happens BETWEEN rebuild steps —
// after the rows are copied, before the old tables are dropped — and a
// hand-written marker row would not be a real interrupted run. This is
// unnameable in production builds, exactly like `migration::Interrupt`.
#[cfg(test)]
thread_local! {
    static INTERRUPT_BETWEEN_STEPS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn arm_interrupt_between_rebuild_steps() {
    INTERRUPT_BETWEEN_STEPS.with(|armed| armed.set(true));
}

#[cfg(test)]
pub(crate) fn disarm_interrupt_between_rebuild_steps() {
    INTERRUPT_BETWEEN_STEPS.with(|armed| armed.set(false));
}

#[cfg(test)]
fn interrupt_between_rebuild_steps() -> Result<(), String> {
    if INTERRUPT_BETWEEN_STEPS.with(std::cell::Cell::get) {
        return Err("interrupted between rebuild steps".to_owned());
    }
    Ok(())
}

#[cfg(not(test))]
fn interrupt_between_rebuild_steps() -> Result<(), String> {
    Ok(())
}

/// The target tables, built under temporary names.
///
/// This DDL is deliberately a SECOND statement of the shape `write.rs` builds,
/// and the duplication is bounded rather than hidden: the migration cannot call
/// `ensure_schema`, which creates the FINAL names with `IF NOT EXISTS` and
/// would no-op against the very tables being replaced. What binds the two is
/// not shared text but an executed check — every migration witness opens a REAL
/// `EventStore` on the migrated file afterwards, and that open runs
/// `verify_columns` four times, the four-arm target-shape probe, and the index
/// column-shape verification against these tables. Drift between this DDL and
/// the store's expectation fails the open, not a string comparison.
const REBUILT_TABLES: &str = "CREATE TABLE event_sequence_rebuilt (
    aggregate_family TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    owner_id TEXT,
    PRIMARY KEY (aggregate_family, aggregate_id)
);
CREATE TABLE event_rebuilt (
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
        REFERENCES event_sequence_rebuilt (aggregate_family, aggregate_id) ON DELETE CASCADE
);";

const REBUILT_INDEXES: &str = "CREATE UNIQUE INDEX event_aggregate_seq_idx
    ON event (aggregate_family, aggregate_id, seq);
CREATE INDEX event_aggregate_type_seq_idx
    ON event (aggregate_family, aggregate_id, type, seq);
CREATE UNIQUE INDEX event_campaign_transition_idx
    ON event (aggregate_family, aggregate_id, campaign_transition_id)
    WHERE campaign_transition_id IS NOT NULL;";

const NO_NEW_V1_TRIGGER: &str = "CREATE TRIGGER event_refuses_new_unfenced_write
    BEFORE INSERT ON event
    WHEN NEW.writer_epoch IS NULL
    BEGIN
        SELECT RAISE(ABORT, 'no new v1 write: a new event row must carry its elected writer epoch');
    END;";
