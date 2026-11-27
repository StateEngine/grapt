use super::{ProjectorError, WriteError};
use crate::decode::DecodedEvent;
use crate::notify::CommittedNotices;
use crate::projection::ProjectionId;
use grapt_event::AggregateId;
use grapt_types::{EventId, EventVersion, WriterEpoch};
use serde_json::Value;
use std::collections::HashMap;

pub trait Projector {
    fn projection(&self) -> ProjectionId;
    fn project(
        &mut self,
        tx: &rusqlite::Transaction<'_>,
        event: &DecodedEvent,
    ) -> Result<(), ProjectorError>;
    /// Session projectors refuse global facts unless their owner explicitly
    /// implements the global-form projection.
    fn project_form(
        &mut self,
        _tx: &rusqlite::Transaction<'_>,
        _event: &crate::DecodedFormFact,
    ) -> Result<(), ProjectorError> {
        Err("this projector does not own global form facts".into())
    }
}

/// The fencing token an elected store instance holds for one aggregate.
///
/// The field is private and [`super::EventStore::fencing_token`] is the only
/// constructor, so no consumer crate can author one. That is defence in depth
/// rather than the protection itself: [`super::EventStore::append_within`] compares
/// the token against the elected row IN THE DATABASE, so even a token forged
/// inside this crate is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FencedWriter {
    pub(super) epoch: WriterEpoch,
}

impl FencedWriter {
    pub fn epoch(self) -> WriterEpoch {
        self.epoch
    }
}

/// An owned snapshot of the epochs a store instance holds.
///
/// It exists for the caller shape the six consumer services already use: open
/// one immediate transaction, READ the domain state that names the aggregate,
/// then append. By then the store is mutably borrowed by its own transaction,
/// so [`super::EventStore::fencing_token`] is unreachable and the token has to have
/// been taken out first — before the aggregate is known.
///
/// Snapshotting loses nothing: the held set changes only through
/// [`super::EventStore::elect_writer`], which takes `&mut self` and therefore cannot
/// run while a transaction from the same store is alive. A snapshot taken just
/// before the transaction is the live set for that transaction's whole life.
#[derive(Debug, Clone, Default)]
pub struct HeldWriters(pub(super) HashMap<AggregateId, WriterEpoch>);

impl HeldWriters {
    /// The token for an aggregate this store was elected for, or the typed
    /// [`WriteError::NotElected`] refusal.
    pub fn token(&self, aggregate: &AggregateId) -> Result<FencedWriter, WriteError> {
        match self.0.get(aggregate) {
            Some(&epoch) => Ok(FencedWriter { epoch }),
            None => Err(WriteError::NotElected {
                family: crate::election::family_of(aggregate),
                aggregate: crate::election::identity_of(aggregate).to_owned(),
            }),
        }
    }
}

#[derive(Debug)]
pub struct AppendInput<'a> {
    pub id: &'a EventId,
    /// Family PLUS identity. A session and a campaign that happened to share
    /// an identity string are different aggregates with independent sequences
    /// and independent elected writers, so the store key can never be a bare
    /// identity string.
    pub aggregate: AggregateId,
    /// The epoch this store instance holds for `aggregate`, from
    /// [`super::EventStore::fencing_token`]. Consumer public signatures do not carry
    /// this: a service obtains it from the store it already owns.
    pub writer: FencedWriter,
    pub kind_name: &'a str,
    pub version: EventVersion,
    pub created_ms: i64,
    pub data: &'a Value,
}

/// The stored columns the idempotency pre-check compares an incoming append
/// against. `writer_epoch` is optional because a NULL there is exactly
/// `EventFencing::LegacyUnfenced` — an imported v1 row, never a new write.
pub(super) type StoredRow = (String, String, i64, i64, String, String, Option<i64>);

#[derive(Debug, PartialEq, Eq)]
pub enum Appended {
    /// Committed at this sequence.
    Committed { seq: u64 },
    /// Identical event already present (canonical-hash match): no-op.
    Idempotent { seq: u64 },
}

/// What one [`super::EventStore::catch_up`] pass moved.
#[derive(Debug)]
pub struct CaughtUp {
    /// Events folded through the projector on this pass.
    pub projected: u64,
    /// Where the projection now stands, or `None` when the aggregate has no
    /// events at all.
    pub watermark: Option<u64>,
    /// Always empty: re-projecting durable facts names none. Carried rather
    /// than dropped internally so the emptiness is something a caller — and a
    /// witness — can assert instead of take on trust.
    pub notices: CommittedNotices,
}
