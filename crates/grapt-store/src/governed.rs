//! The authoritative write chokepoint: SQLite itself refuses a consumer write
//! to a governed table.
//!
//! # What this owns
//!
//! The tables below carry the store's own authority and may only be written by the
//! store's own statements:
//!
//! | table | why it is governed |
//! | --- | --- |
//! | `event` | the durable fact log; every row must carry its elected epoch |
//! | `event_sequence` | the per-aggregate cursor `append_within` allocates from |
//! | `aggregate_writer` | the election row the fence compares against |
//! | `engine_epoch` | the database-format ownership stamp |
//! | `projection_watermark` | authoritative projection progress, which may never lead its input |
//! | `plan_read_receipt` | rows kept from the retired campaign draft feature; not rebuildable, so not consumer state |
//! | `campaign_plan_artifact` | rows kept from the retired campaign draft feature; not rebuildable, so not consumer state |
//! | `mailbox_message` | durable seat-mailbox operational truth; the row IS the received message (R3a) |
//! | `attempt_receipt` | durable process-loss operational truth; a consumer UPDATE could convert an ambiguous lost attempt into a settled one (R2C5A1) |
//! | `attempt_receipt_quarantine` | the durable report of a receipt row that could not be trusted; erasing it would let a cursor advance past unauditable evidence (R2C5I) |
//! | `provider_attempt_accounting` | permanent send-permit evidence; receipt reclamation must not erase dispatch coverage |
//! | `provider_attempt_coverage` | historical coverage gaps established at installation; consumers cannot clear them |
//! | `mcp_oauth_credential` | tokens and revision tombstones; rewriting them could restore a credential after sign-out |
//!
//! Before this module the protection was a convention: `connection_mut` handed
//! every consumer a raw [`Connection`], and nothing but discipline stopped a
//! service from writing any of those tables with its own SQL. The
//! `event_refuses_new_unfenced_write` trigger closed exactly one hole — a new
//! INSERT with a NULL epoch — and left `UPDATE`/`DELETE` open, so a statement
//! could still launder a fenced row back into a legacy-shaped one
//! (`F3B1A_RESIDUALS.md` item 2).
//!
//! # How it is enforced
//!
//! A `sqlite3_set_authorizer` callback is installed on the store's connection
//! for its whole life. `INSERT`, `UPDATE` and `DELETE` naming one of the governed
//! tables are refused at STATEMENT PREPARATION — before a single row is
//! touched — unless the store has explicitly lifted the denial for that exact
//! table. The refusal is [`Authorization::Deny`], never `Ignore`: `Ignore`
//! silently skips the operation and would turn a forbidden write into a
//! successful no-op the caller cannot distinguish from a real one.
//!
//! Creating, removing or renaming governed schema objects is also refused,
//! including temporary objects that could shadow the main schema. Temporary
//! table renames and database attachment or detachment are refused. Index
//! rebuilding is governed by its owning table, including bare and collation-wide
//! `REINDEX` when they reach governed indexes. Even `CREATE TABLE IF NOT EXISTS`
//! on a governed name is refused. Reads, ordinary pragmas, transaction control
//! and writes to consumer tables remain available. Shared connection APIs that
//! can change configuration or override SQL functions/collations are disallowed
//! by the workspace lint, except the fixed directory-query function adapter.
//!
//! # The lift is per statement, per table, and restored on every exit
//!
//! [`WriteGate::execute`] is the only way governed SQL runs. It raises the gate
//! for ONE named table, executes ONE statement, and lowers it in a [`Drop`]
//! guard — so an early `?`, an error or a panic all restore the denial. There is
//! no lift that spans a scope with foreign code in it; in particular projectors
//! run with the denial ON, because `append_within` lifts only around its own
//! two inserts and never around the projector loop.
//!
//! The lift names one table, not "governed writes". While `event` is lifted,
//! `aggregate_writer` is still refused.
//!
//! # Why a consumer cannot forge one
//!
//! [`WriteGate`], [`GovernedTable`] and the guard are private to this crate, so
//! no consumer can name them. That is not the protection. The protection is
//! that the installed authorizer reads the exact [`Arc`] it captured at install
//! time: a second `WriteGate` — even one built inside this crate — raises a
//! DIFFERENT atomic and cannot lift the installed denial
//! (`a_second_gate_cannot_lift_the_installed_denial`).
//!
//! # Scope this does NOT claim
//!
//! A SQLite authorizer is a per-CONNECTION guard. It governs every statement on
//! the store's connection and nothing else; another connection to the same file
//! — another process, or a test staging hostile state — is outside it. The
//! database-wide half of the law is the `event_refuses_new_unfenced_write`
//! trigger, which lives in the schema and therefore fires for every connection.
//!
//! Both premises above are true, and neither conclusion may be stated without
//! its precondition:
//!
//! * **while the trigger exists in the schema**, a new unfenced `event` row is
//!   refused on any connection, in any process;
//! * **while the installed authorizer sees the governed table NAMES**, the ruled
//!   `INSERT`/`UPDATE`/`DELETE` verbs are refused on the store's own connection.
//!
//! The authorizer also refuses the store connection's `DROP TRIGGER` and
//! `ALTER TABLE ... RENAME` routes that could remove those preconditions.
//! Connections outside the store remain outside this connection-local guard.

mod durability;

use crate::notify::{CommittedNotices, DurableNotice, PendingNotices};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, TransactionBehavior};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// The gate is lowered: every governed write is refused.
///
/// Zero is deliberately the denial value, so a zeroed or freshly allocated gate
/// denies rather than permits.
const DENIED: u8 = 0;

/// A table only the store's own statements may write.
///
/// The list is exhaustive by construction: [`GovernedTable::ALL`] is what the
/// authorizer consults, so a variant missing from it is a table nobody governs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GovernedTable {
    Event,
    EventSequence,
    AggregateWriter,
    EngineEpoch,
    ProjectionWatermark,
    /// Durable plan-read operational truth (F4B2a). NOT a projection: a
    /// recorded receipt is not rebuildable from the fact log, so a consumer
    /// statement rewriting it would rewrite audit truth.
    PlanReadReceipt,
    /// Durable seat-mailbox operational truth (R3a). Also not rebuildable: the
    /// row IS the received message, and a consumer statement could otherwise
    /// flip `source_acknowledged` — telling a transport we hold work we do not
    /// — or DELETE admitted mail outright.
    MailboxMessage,
    /// Durable process-loss operational truth (R2C5A1). Not rebuildable either,
    /// and the consequence of a consumer write is the sharpest of the three: an
    /// UPDATE setting `settled_ms` converts an ambiguous lost attempt into a
    /// settled one and authorises the automatic replay of an external effect the
    /// law forbids replaying, while a DELETE erases the only crash-visible
    /// evidence that bytes ever left the process.
    AttemptReceipt,
    /// The durable quarantine for receipt rows that cannot be trusted (R2C5I
    /// ruling 5). Governed for the same reason as the table it drains: a
    /// consumer that could write here could erase the durable report of a
    /// corrupt or oversize row, and a reconciler would then advance its cursor
    /// past evidence nobody can audit.
    AttemptReceiptQuarantine,
    /// OAuth tokens and sign-out tombstones share one versioned authority row.
    McpOAuthCredential,
    ProviderAttemptAccounting,
    ProviderAttemptCoverage,
    CampaignPlanArtifact,
}

impl GovernedTable {
    pub(crate) const ALL: [GovernedTable; 13] = [
        GovernedTable::Event,
        GovernedTable::EventSequence,
        GovernedTable::AggregateWriter,
        GovernedTable::EngineEpoch,
        GovernedTable::ProjectionWatermark,
        GovernedTable::PlanReadReceipt,
        GovernedTable::MailboxMessage,
        GovernedTable::AttemptReceipt,
        GovernedTable::AttemptReceiptQuarantine,
        GovernedTable::McpOAuthCredential,
        GovernedTable::ProviderAttemptAccounting,
        GovernedTable::ProviderAttemptCoverage,
        GovernedTable::CampaignPlanArtifact,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            GovernedTable::Event => "event",
            GovernedTable::EventSequence => "event_sequence",
            GovernedTable::AggregateWriter => "aggregate_writer",
            GovernedTable::EngineEpoch => "engine_epoch",
            GovernedTable::ProjectionWatermark => "projection_watermark",
            GovernedTable::PlanReadReceipt => "plan_read_receipt",
            GovernedTable::MailboxMessage => "mailbox_message",
            GovernedTable::AttemptReceipt => "attempt_receipt",
            GovernedTable::AttemptReceiptQuarantine => "attempt_receipt_quarantine",
            GovernedTable::McpOAuthCredential => "mcp_oauth_credential",
            GovernedTable::ProviderAttemptAccounting => "provider_attempt_accounting",
            GovernedTable::ProviderAttemptCoverage => "provider_attempt_coverage",
            GovernedTable::CampaignPlanArtifact => "campaign_plan_artifact",
        }
    }

    /// The gate value that lifts this table, never [`DENIED`].
    const fn code(self) -> u8 {
        match self {
            GovernedTable::Event => 1,
            GovernedTable::EventSequence => 2,
            GovernedTable::AggregateWriter => 3,
            GovernedTable::EngineEpoch => 4,
            GovernedTable::ProjectionWatermark => 5,
            GovernedTable::PlanReadReceipt => 6,
            GovernedTable::MailboxMessage => 7,
            GovernedTable::AttemptReceipt => 8,
            GovernedTable::AttemptReceiptQuarantine => 9,
            GovernedTable::McpOAuthCredential => 10,
            GovernedTable::ProviderAttemptAccounting => 11,
            GovernedTable::ProviderAttemptCoverage => 12,
            GovernedTable::CampaignPlanArtifact => 13,
        }
    }

    /// SQLite identifiers are case-insensitive, so `INSERT INTO EVENT` names the
    /// same table as `INSERT INTO event`.
    ///
    /// The case-insensitive comparison is DEFENCE IN DEPTH, not a load-bearing
    /// law, and it is recorded that way because a mutation proved it: replacing
    /// `eq_ignore_ascii_case` with `==` leaves
    /// `an_uppercase_statement_names_the_same_governed_table` green. SQLite hands
    /// the authorizer the name as the SCHEMA declares it, not as the statement
    /// spells it, so an exact comparison already matches. The comparison stays
    /// because it costs nothing and does not depend on that behaviour, but no
    /// witness here attributes it and the mutation manifest does not claim one.
    fn from_name(name: &str) -> Option<Self> {
        GovernedTable::ALL
            .into_iter()
            .find(|table| table.name().eq_ignore_ascii_case(name))
    }

    /// Fails closed: an unrecognised gate value lifts nothing.
    fn from_code(code: u8) -> Option<Self> {
        GovernedTable::ALL
            .into_iter()
            .find(|table| table.code() == code)
    }
}

/// The installed denial plus the store-owned lift.
///
/// Cloning shares one gate; the authorizer closure holds a clone of the same
/// [`Arc`]. Crate-private: a consumer can neither name it nor obtain one.
#[derive(Debug, Clone)]
pub(crate) struct WriteGate(Arc<AtomicU8>);

impl WriteGate {
    pub(crate) fn new() -> Self {
        WriteGate(Arc::new(AtomicU8::new(DENIED)))
    }

    /// Installs THIS gate's denial on `conn` for the connection's whole life.
    ///
    /// Called once, after schema preparation and epoch stamping have finished:
    /// those are the store's own first-contact statements and they run before
    /// any consumer can reach the connection.
    pub(crate) fn install(&self, conn: &mut Connection) -> rusqlite::Result<()> {
        // Other governed indexes cannot be created or removed after installation.
        // SQLite's REINDEX callback supplies only the index name.
        let mut indexes = BTreeMap::new();
        {
            let mut statement =
                conn.prepare("SELECT name, tbl_name FROM main.sqlite_schema WHERE type = 'index'")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (index, table) = row?;
                if let Some(table) = GovernedTable::from_name(&table) {
                    indexes.insert(index.to_ascii_lowercase(), table);
                }
            }
        }
        // The deferred indexes are built after installation, so they are
        // governed by name whether or not they exist yet.
        for (table, index, _, _) in crate::EventStore::DEFERRED_INDEXES {
            if let Some(table) = GovernedTable::from_name(table) {
                indexes.insert(index.to_ascii_lowercase(), table);
            }
        }
        let gate = Arc::clone(&self.0);
        conn.authorizer(Some(move |context: AuthContext<'_>| {
            decide(&gate, &indexes, context)
        }))
    }

    /// Runs ONE governed statement with the denial lifted for ONE table.
    ///
    /// The guard is dropped by the end of this function on every path —
    /// success, `?`, or unwind — so no lift outlives the statement it exists
    /// for.
    pub(crate) fn execute<P: rusqlite::Params>(
        &self,
        conn: &Connection,
        table: GovernedTable,
        sql: &str,
        params: P,
    ) -> rusqlite::Result<usize> {
        if !matches!(
            table,
            GovernedTable::Event
                | GovernedTable::EventSequence
                | GovernedTable::ProjectionWatermark
        ) {
            durability::require_connection(conn)?;
        }
        let _lift = self.lift(table);
        conn.execute(sql, params)
    }

    /// Raises the gate for one table until the guard drops.
    ///
    /// `pub(crate)` rather than private only so the mutation manifest can stage
    /// the regression this design exists to prevent — a lift held across the
    /// projector loop. It is still unnameable outside this crate, and
    /// [`Self::execute`] remains the only caller in the store.
    pub(crate) fn lift(&self, table: GovernedTable) -> LiftGuard<'_> {
        self.0.store(table.code(), Ordering::SeqCst);
        LiftGuard { gate: &self.0 }
    }
}

/// Lowers the gate when it drops. Restoring [`DENIED`] rather than a saved
/// previous value is the fail-closed choice: lifts in this crate never nest,
/// and a future one that did would end denied rather than open.
#[derive(Debug)]
pub(crate) struct LiftGuard<'a> {
    gate: &'a AtomicU8,
}

impl Drop for LiftGuard<'_> {
    fn drop(&mut self) {
        self.gate.store(DENIED, Ordering::SeqCst);
    }
}

/// The authorizer decision for one action, consulted while SQLite compiles a
/// statement.
fn decide(
    gate: &AtomicU8,
    indexes: &BTreeMap<String, GovernedTable>,
    context: AuthContext<'_>,
) -> Authorization {
    let governed = match context.action {
        AuthAction::Attach { .. } | AuthAction::Detach { .. } => return Authorization::Deny,
        AuthAction::Pragma {
            pragma_name,
            pragma_value: Some(_),
        } if pragma_name.eq_ignore_ascii_case("writable_schema") => {
            return Authorization::Deny;
        }
        // Bound parameters provide no filename, so rusqlite preserves these as Unknown.
        AuthAction::Unknown {
            code: rusqlite::ffi::SQLITE_ATTACH | rusqlite::ffi::SQLITE_DETACH,
            ..
        } => {
            return Authorization::Deny;
        }
        AuthAction::Reindex { index_name } => {
            if context.database_name == Some("temp") {
                None
            } else {
                indexes.get(&index_name.to_ascii_lowercase()).copied()
            }
        }
        // ALTER reports only the old name: a temporary consumer table could
        // otherwise be renamed to shadow a governed main table.
        AuthAction::AlterTable { database_name, .. }
            if database_name.eq_ignore_ascii_case("temp") =>
        {
            return Authorization::Deny;
        }
        AuthAction::Insert { table_name }
        | AuthAction::Update { table_name, .. }
        | AuthAction::Delete { table_name }
        | AuthAction::DropTable { table_name }
        | AuthAction::DropTempTable { table_name }
        | AuthAction::DropTrigger { table_name, .. }
        | AuthAction::DropTempTrigger { table_name, .. }
        | AuthAction::DropIndex { table_name, .. }
        | AuthAction::DropTempIndex { table_name, .. }
        | AuthAction::AlterTable { table_name, .. }
        | AuthAction::CreateTable { table_name }
        | AuthAction::CreateTempTable { table_name }
        | AuthAction::CreateTrigger { table_name, .. }
        | AuthAction::CreateTempTrigger { table_name, .. }
        | AuthAction::CreateIndex { table_name, .. }
        | AuthAction::CreateTempIndex { table_name, .. }
        | AuthAction::CreateVtable { table_name, .. }
        | AuthAction::DropVtable { table_name, .. } => GovernedTable::from_name(table_name),
        AuthAction::CreateView { view_name }
        | AuthAction::CreateTempView { view_name }
        | AuthAction::DropView { view_name }
        | AuthAction::DropTempView { view_name } => GovernedTable::from_name(view_name),
        _ => return Authorization::Allow,
    };
    let Some(governed) = governed else {
        return Authorization::Allow;
    };
    match GovernedTable::from_code(gate.load(Ordering::SeqCst)) {
        Some(lifted) if lifted == governed => Authorization::Allow,
        _ => Authorization::Deny,
    }
}

/// The one transaction boundary a consumer of [`crate::EventStore`] can open.
///
/// It replaces `EventStore::connection_mut`, which handed out a raw
/// [`Connection`] and left both the isolation level and the reach of the
/// statements to the caller. This type fixes both:
///
/// * it always begins `IMMEDIATE`, so the write lock is taken at `BEGIN` and two
///   writers serialize instead of racing to an upgrade that one of them loses
///   (`the_governed_transaction_takes_the_write_lock_at_begin`);
/// * the connection it borrows carries the installed denial, so the raw
///   [`rusqlite::Transaction`] reachable through [`std::ops::Deref`] can read
///   and can write the caller's own tables, but cannot write a governed one.
///
/// It is `!Send` by construction — it borrows a [`Connection`], which is not
/// `Sync` — so no database guard can be held across an `.await`. That is the
/// sync/async constitution's rule enforced by the type rather than by review;
/// `tests/probes/01_governed_transaction_across_await.rs` is the compile-fail
/// witness.
///
/// # It also owns the transaction's pending live notices
///
/// Every fact appended inside it mints one advisory notice, and
/// [`Self::commit`] is the ONLY producer of the [`CommittedNotices`] carrier
/// that can leave. Rolling back — by `Drop`, by an early `?`, or by a failed
/// `COMMIT` — drops the pending notices with the transaction, so no consumer
/// can publish a fact that is not durable. See [`crate::notify`].
#[derive(Debug)]
pub struct GovernedTransaction<'conn> {
    tx: rusqlite::Transaction<'conn>,
    gate: WriteGate,
    /// `RefCell` because `append_within` takes `&GovernedTransaction`: the
    /// append path may not demand an exclusive borrow of the transaction a
    /// consumer is also reading its domain state through. The type is `!Send`,
    /// so this is single-threaded interior mutability with no sharing story.
    notices: RefCell<PendingNotices>,
    read_projection: Option<Arc<dyn crate::commit_projection::CommittedReadProjection>>,
    projection_dirty: Cell<bool>,
    durable_commit: bool,
    pub(crate) timing: Option<Arc<dyn grapt_types::timing::TimingObserver>>,
    pub(crate) timing_begin: Option<(grapt_types::Timestamp, grapt_types::Timestamp)>,
    pub(crate) timing_scope: RefCell<grapt_types::timing::TimingScope>,
    commit_executor: Option<Arc<dyn crate::commit_executor::CommitExecutor>>,
}

impl<'conn> GovernedTransaction<'conn> {
    pub(crate) fn begin_with_projection(
        conn: &'conn mut Connection,
        gate: WriteGate,
        read_projection: Option<Arc<dyn crate::commit_projection::CommittedReadProjection>>,
        commit_executor: Option<Arc<dyn crate::commit_executor::CommitExecutor>>,
    ) -> rusqlite::Result<Self> {
        Self::begin_with_policy(conn, gate, read_projection, commit_executor, true)
    }

    pub(crate) fn begin_with_policy(
        conn: &'conn mut Connection,
        gate: WriteGate,
        read_projection: Option<Arc<dyn crate::commit_projection::CommittedReadProjection>>,
        commit_executor: Option<Arc<dyn crate::commit_executor::CommitExecutor>>,
        durable: bool,
    ) -> rusqlite::Result<Self> {
        durability::prepare(conn, durable)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Ok(GovernedTransaction {
            tx,
            gate,
            notices: RefCell::new(PendingNotices::default()),
            read_projection,
            projection_dirty: Cell::new(false),
            durable_commit: durable,
            timing: None,
            timing_begin: None,
            timing_scope: RefCell::new(grapt_types::timing::TimingScope {
                session: None,
                attempt: None,
                class: grapt_types::timing::TimingClass::Other,
            }),
            commit_executor,
        })
    }

    /// Commits the transaction and releases its notices.
    ///
    /// The ORDER here is the commit-before-notify law: the notices are converted
    /// into the publishable carrier only after `COMMIT` reports success. A
    /// failed commit returns `Err` and the pending notices are dropped
    /// unpublished; dropping without committing rolls back and does the same,
    /// exactly as [`rusqlite::Transaction`] does.
    pub fn commit(self) -> rusqlite::Result<CommittedNotices> {
        let Some(executor) = self.commit_executor.clone() else {
            return self.commit_inline();
        };
        let mut transaction = Some(self);
        let mut result = None;
        executor.execute(&mut || {
            if let Some(transaction) = transaction.take() {
                result = Some(transaction.commit_inline());
            }
        });
        // A bad executor cannot fabricate an ACK or commit twice. If it omits
        // the work, dropping the retained transaction rolls back as usual.
        match result {
            Some(result) => result,
            None => Err(rusqlite::Error::InvalidQuery),
        }
    }

    fn commit_inline(self) -> rusqlite::Result<CommittedNotices> {
        if let (Some(observer), Some((start, end))) = (&self.timing, self.timing_begin) {
            observer.record(grapt_types::timing::TimingSample {
                scope: self.timing_scope.borrow().clone(),
                stage: grapt_types::timing::TimingStage::TransactionBegin,
                start,
                end,
            });
        }
        let projection_span = self
            .read_projection
            .as_ref()
            .filter(|_| self.projection_dirty.get())
            .and_then(|_| self.timing_span(grapt_types::timing::TimingStage::ProjectionPrepare));
        let prepared = self
            .read_projection
            .as_ref()
            .filter(|_| self.projection_dirty.get())
            .map(|projection| projection.prepare(&self.tx));
        drop(projection_span);
        let commit_span = self.timing_span(grapt_types::timing::TimingStage::SqlCommit);
        let publish_context =
            self.timing
                .as_ref()
                .map(|observer| grapt_types::timing::TimingContext {
                    observer: Arc::clone(observer),
                    scope: self.timing_scope.borrow().clone(),
                });
        // Destructured rather than moved field-by-field through `self` so the
        // pending notices are visibly still owned here, and visibly dropped by
        // the `?` below when the commit fails.
        let GovernedTransaction {
            tx,
            gate: _,
            notices,
            read_projection: _,
            projection_dirty: _,
            durable_commit: _,
            timing: _,
            timing_scope: _,
            timing_begin: _,
            commit_executor: _,
        } = self;
        let pending = notices.into_inner();
        tx.commit()?;
        drop(commit_span);
        if let Some(prepared) = prepared {
            let _publish = publish_context
                .map(|context| context.span(grapt_types::timing::TimingStage::ProjectionPublish));
            prepared.publish();
        }
        Ok(pending.into_committed())
    }

    /// Assert the policy chosen before BEGIN; a stream transaction cannot be
    /// upgraded after writes, and must be dropped and retried as an intent.
    pub fn require_durable_commit(&self) -> rusqlite::Result<()> {
        durability::require(self.durable_commit)
    }

    pub(crate) fn require_event_policy(&self, kind: &str) -> rusqlite::Result<()> {
        if matches!(
            kind,
            "session.content.appended"
                | "session.content-block.opened"
                | "session.content.checkpointed"
                | "session.usage.observed"
        ) {
            Ok(())
        } else {
            self.require_durable_commit()
        }
    }

    /// The store's lift. Crate-private, so the chokepoint has no public escape.
    pub(crate) fn gate(&self) -> &WriteGate {
        &self.gate
    }

    /// Records one advisory notice for a fact this transaction has written.
    ///
    /// Crate-private, and its one caller is the append path after the event row
    /// insert succeeds. Nothing publishes here: this only makes the notice
    /// eligible for release by a successful commit.
    pub(crate) fn push_notice(&self, notice: DurableNotice) {
        // This bit is lossless even when the bounded advisory queue overflows.
        if self
            .read_projection
            .as_ref()
            .is_some_and(|projection| projection.affected(notice.kind()))
        {
            self.projection_dirty.set(true);
        }
        self.notices.borrow_mut().push(notice);
    }
}

impl<'conn> std::ops::Deref for GovernedTransaction<'conn> {
    type Target = rusqlite::Transaction<'conn>;

    fn deref(&self) -> &Self::Target {
        &self.tx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn denied(error: &rusqlite::Error) -> bool {
        matches!(
            error,
            rusqlite::Error::SqliteFailure(code, _)
                if code.code == rusqlite::ErrorCode::AuthorizationForStatementDenied
        )
    }

    /// A connection carrying every governed table and one ordinary table.
    fn guarded() -> Result<(Connection, WriteGate), Box<dyn std::error::Error>> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE event (id TEXT PRIMARY KEY, writer_epoch INTEGER);
             CREATE TABLE event_sequence (aggregate_id TEXT PRIMARY KEY, seq INTEGER);
             CREATE TABLE aggregate_writer (aggregate_id TEXT PRIMARY KEY, epoch INTEGER);
             CREATE TABLE engine_epoch (epoch INTEGER);
             CREATE TABLE projection_watermark (projection TEXT PRIMARY KEY, seq INTEGER);
             CREATE TABLE plan_read_receipt (receipt_id TEXT PRIMARY KEY, record_bytes BLOB);
             CREATE TABLE mailbox_message (message_id TEXT PRIMARY KEY, record_bytes BLOB);
             CREATE TABLE attempt_receipt (attempt_id TEXT PRIMARY KEY, record_bytes BLOB);
             CREATE TABLE attempt_receipt_quarantine (attempt_id TEXT PRIMARY KEY, record_bytes BLOB);
             CREATE TABLE mcp_oauth_credential (key TEXT PRIMARY KEY, revision INTEGER, value TEXT);
             CREATE TABLE provider_attempt_accounting (attempt_id TEXT PRIMARY KEY, record_bytes BLOB);
             CREATE TABLE provider_attempt_coverage (session_id TEXT PRIMARY KEY, complete INTEGER);
             CREATE TABLE ordinary (id TEXT PRIMARY KEY);",
        )?;
        let tx = conn.transaction()?;
        crate::retired_campaign::ensure_plan_artifact_table(&tx)?;
        tx.commit()?;
        let gate = WriteGate::new();
        gate.install(&mut conn)?;
        Ok((conn, gate))
    }

    #[test]
    fn a_lift_names_one_table_and_leaves_the_others_denied() -> TestResult {
        let (conn, gate) = guarded()?;
        // Inside the lift for `event`, the sibling governed table is still shut.
        // A lift that meant "governed writes are open" instead of "this table is
        // open" would make every store statement a four-table window.
        let _ = gate.execute(
            &conn,
            GovernedTable::Event,
            "INSERT INTO event (id, writer_epoch) VALUES ('evt_scope', 1)",
            [],
        )?;
        let inside = gate.execute(
            &conn,
            GovernedTable::Event,
            "INSERT INTO aggregate_writer (aggregate_id, epoch) VALUES ('ses_scope', 1)",
            [],
        );
        match inside {
            Err(ref error) if denied(error) => {}
            other => panic!("a lift for `event` must not open `aggregate_writer`: {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn every_governed_table_is_denied_without_a_lift() -> TestResult {
        // Table-driven over `ALL` on purpose: a table added to the governed set
        // later is covered here the moment it is listed, so "declared governed"
        // and "actually refused" cannot drift apart. `DELETE` is used because it
        // is a governed verb that names no columns, which keeps one statement
        // shape valid for every table.
        let (conn, _gate) = guarded()?;
        for table in GovernedTable::ALL {
            let sql = format!("DELETE FROM \"{}\"", table.name());
            match conn.execute(&sql, []) {
                Err(ref error) if denied(error) => {}
                other => panic!(
                    "{} is declared governed but was not denied: {other:?}",
                    table.name()
                ),
            }
        }
        Ok(())
    }

    #[test]
    fn the_lift_is_lowered_when_the_statement_fails() -> TestResult {
        let (conn, gate) = guarded()?;
        // The statement itself fails (no such column), so the lift is released
        // by unwinding out of `execute` through `?` rather than by returning.
        let failed = gate.execute(
            &conn,
            GovernedTable::Event,
            "INSERT INTO event (id, no_such_column) VALUES ('evt_x', 1)",
            [],
        );
        assert!(failed.is_err(), "the staged statement must fail");
        match conn.execute(
            "INSERT INTO event (id, writer_epoch) VALUES ('evt_after', 1)",
            [],
        ) {
            Err(ref error) if denied(error) => Ok(()),
            other => panic!("a failed governed statement left the gate lifted: {other:?}"),
        }
    }

    #[test]
    fn a_second_gate_cannot_lift_the_installed_denial() -> TestResult {
        let (conn, _installed) = guarded()?;
        // Crate-private types are not the protection. The authorizer reads the
        // exact Arc it captured, so a gate built beside it raises a different
        // atomic and lifts nothing.
        let forged = WriteGate::new();
        match forged.execute(
            &conn,
            GovernedTable::Event,
            "INSERT INTO event (id, writer_epoch) VALUES ('evt_forged', 1)",
            [],
        ) {
            Err(ref error) if denied(error) => Ok(()),
            other => panic!("a second gate lifted the installed denial: {other:?}"),
        }
    }

    #[test]
    fn an_unrecognised_gate_value_denies() -> TestResult {
        let (conn, gate) = guarded()?;
        // Fail closed: a value outside the known codes is not "some table is
        // lifted", it is a gate whose meaning is unknown.
        gate.0.store(u8::MAX, Ordering::SeqCst);
        let outcome = conn.execute(
            "INSERT INTO event (id, writer_epoch) VALUES ('evt_u', 1)",
            [],
        );
        gate.0.store(DENIED, Ordering::SeqCst);
        match outcome {
            Err(ref error) if denied(error) => Ok(()),
            other => panic!("an unrecognised gate value must deny: {other:?}"),
        }
    }

    #[test]
    fn every_governed_table_has_a_distinct_lift_code_and_none_is_the_denial() {
        // A duplicate code would silently make two tables one lift; the denial
        // value colliding with a table's code would make an un-lifted gate open.
        for table in GovernedTable::ALL {
            assert_ne!(
                table.code(),
                DENIED,
                "{} shares the denial value",
                table.name()
            );
            let collisions = GovernedTable::ALL
                .into_iter()
                .filter(|other| other.code() == table.code())
                .count();
            assert_eq!(collisions, 1, "{} shares a lift code", table.name());
        }
    }
}
