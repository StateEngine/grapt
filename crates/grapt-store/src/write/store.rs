use super::WriteError;
use crate::election::WriterOwnerId;
use crate::governed::WriteGate;
use grapt_event::AggregateId;
use grapt_types::{IdAuthority, WriterEpoch};
use rusqlite::Connection;
use std::collections::HashMap;

/// Writable event store. Opening stamps the engine epoch (first-contact law):
/// an oracle-era file becomes Rust-owned; a foreign epoch refuses.
///
/// # Writer identity is store-owned
///
/// Each instance mints its own [`WriterOwnerId`] and remembers the epochs it
/// was elected for. Consumers pass no epoch through their public signatures:
/// a service obtains [`super::FencedWriter`] from the store it already owns. Two
/// instances open on one file are two owners and fence each other, which is
/// how the stale-writer refusal is proven without simulating a second process.
///
/// # Governed writes
///
/// The connection carries an installed denial for the four governed tables
/// (`event`, `event_sequence`, `aggregate_writer`, `engine_epoch`). Consumers
/// reach it only through `GovernedTransaction`; the store's own governed
/// statements run one at a time through this instance's `WriteGate`.
///
/// `governed` and `WriteGate` are crate-private and are named here as plain
/// code text, not links: a public item's documentation may not link to what its
/// readers cannot reach.
#[derive(Debug)]
pub struct EventStore {
    pub(crate) readers: std::sync::OnceLock<Option<crate::read_pool::ReadPool>>,
    pub(crate) timing: Option<std::sync::Arc<dyn grapt_types::timing::TimingObserver>>,
    pub(super) conn: Connection,
    pub(super) owner: WriterOwnerId,
    /// Epochs obtained by an explicit [`EventStore::elect_writer`] call. Append
    /// reads this map; it never writes it, so no append can elect.
    pub(super) held: HashMap<AggregateId, WriterEpoch>,
    /// This connection's governed-write denial and the only lift for it.
    pub(super) gate: WriteGate,
    pub(crate) commit_executor: Option<std::sync::Arc<dyn crate::commit_executor::CommitExecutor>>,
    pub(crate) read_projection:
        Option<std::sync::Arc<dyn crate::commit_projection::CommittedReadProjection>>,
}

impl EventStore {
    /// Installs the governed-write denial and hands back the finished store.
    ///
    /// Called AFTER `stamp_and_prepare`, and that order is the law: stamping
    /// writes `engine_epoch` and schema preparation creates the governed tables,
    /// both of which the denial refuses. They are the store's own first-contact
    /// statements and complete before any consumer can hold the store, so the
    /// window they run in is not reachable from outside this function.
    pub(crate) fn guarded(mut conn: Connection, ids: &dyn IdAuthority) -> Result<Self, WriteError> {
        let gate = WriteGate::new();
        gate.install(&mut conn)?;
        Ok(Self {
            readers: std::sync::OnceLock::new(),
            conn,
            owner: WriterOwnerId::mint(ids),
            held: HashMap::new(),
            gate,
            timing: None,
            read_projection: None,
            commit_executor: None,
        })
    }
}

impl EventStore {
    /// Accounting shares the enclosing pooled pin and owns its query budget.
    pub fn pooled_read_snapshot(
        &mut self,
    ) -> Result<crate::read_snapshot::ReadSnapshot<'_>, String> {
        crate::read_snapshot::ReadSnapshot::pooled(&mut self.conn)
    }
}
