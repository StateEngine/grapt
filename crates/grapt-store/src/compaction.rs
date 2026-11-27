//! F4B1b: the durable compaction fact, and the rebuildable caches derived from
//! it.
//!
//! # What this owns
//!
//! One law, stated twice because the two halves have different owners:
//!
//! * a successful compaction commits **one atomic durable descriptor** — never a
//!   marker plus a copy of the history it replaced;
//! * everything else a reader wants to know about the current compacted window
//!   is a **rebuildable cache** derived from that fact, and is never itself
//!   truth.
//!
//! Ratified §4.2: "A successful compaction commits one atomic durable descriptor
//! containing or referencing [...]. The descriptor does not inline duplicate
//! copies of the complete retained history. A bounded materialized checkpoint
//! may exist as a rebuildable, integrity-checked cache or content-addressed
//! artifact, but it is not an independent source of truth."
//!
//! # The fact
//!
//! [`record`] appends exactly one `session.compaction.ended.2` event through
//! [`crate::EventStore::append_within`], inside the caller's one
//! [`GovernedTransaction`]. The payload is F4B1V's strict codec
//! ([`grapt_event::session_compaction`]), so this module defines no wire shape
//! of its own and cannot drift from the accepted one.
//!
//! There is no second append on the success path and no field anywhere here that
//! could carry retained history inline: [`CompactionDescriptor`] holds
//! [`grapt_types::RetainedHistory`], which is a reference. "One atomic fact"
//! is therefore a shape this module cannot violate rather than a rule it
//! remembers to follow — recorded that way, with its carrier named, instead of
//! claimed as a mutation this lane could redden.
//!
//! # The typed outcomes, which are NOT failures of this module
//!
//! Ratified §4.4: "Failure, cancellation, incompatible summary, and no-progress
//! are typed outcomes and leave the previous durable history recoverable."
//!
//! A compaction that produced no descriptor is admitted here as
//! [`CompactionAttempt::Failed`] carrying its exact
//! [`grapt_types::ContextFailure`]. It appends NOTHING — which is precisely
//! what leaves the previous durable history recoverable — and returns the same
//! variant it was given. The variant is carried, never mapped, collapsed or
//! widened: a reader that cannot tell `CompactionNoProgress` from
//! `CompactionFailed` cannot choose between retrying and giving up.
//!
//! An incompatible provider-bound summary is deliberately NOT checked here.
//! It is a READ outcome owned by [`grapt_types::CompactionSummary::redeem_for`]
//! at redemption time, because a summary that is incompatible with the model
//! being used *now* was still a perfectly legal fact when it was written.
//! Checking it at append time would refuse a truthful fact for a condition that
//! did not yet exist.
//!
//! # The caches are consumer-owned, and therefore untrusted on read
//!
//! Ruled at admission: the cache tables are NOT governed tables. The closed
//! five-variant governed set does not grow, so no existing governed-table
//! witness's grid changes.
//!
//! (`governed` and `GovernedTable` are crate-private and are named here as
//! plain code text, not links, for the reason that module states: a public
//! item's documentation may not link to what its readers cannot reach.)
//!
//! The consequence is stated rather than glossed: because these tables are
//! outside the chokepoint, ANY holder of a connection can write them, and the
//! store cannot claim otherwise. So the two laws that a governed table would get
//! for free are enforced HERE, on the read side, against rows this crate's own
//! writer can never produce:
//!
//! * **corruption refuses** ([`cached_window`]) rather than coercing — a stored
//!   row that is not a window is an error, never a default;
//! * **a cache may lag its input but never lead it** ([`cached_window`] again) —
//!   a watermark past the aggregate's durable head is refused, exactly as
//!   [`crate::projection`] refuses it for the authoritative watermark.
//!
//! Both are white-box witnessed through this module's `hostile_cache` staging,
//! which is `#[cfg(test)]` and therefore also named as plain text rather than
//! linked: it does not exist in a documentation build.

use crate::election::{family_of, identity_of};
use crate::governed::GovernedTransaction;
use crate::write::{AppendInput, Appended, FencedWriter, Projector, WriteError};
use grapt_event::session_compaction::{self, PayloadError};
use grapt_event::AggregateId;
use grapt_types::{
    CompactionDescriptor, CompactionId, ContextEpoch, ContextFailure, ContextWindowId, EventId,
    EventSeq, EventVersion, ForkBoundary,
};

/// The durable name of the Rust-native compaction fact.
///
/// The NAME is `session.compaction.ended` and the VERSION is 2; the wire form
/// `session.compaction.ended.2` is what `EventKind::kind_str` renders from that
/// pair. They are kept as a (name, version) pair here rather than as one string
/// because [`AppendInput`] takes them separately and the store composes the
/// versioned type itself — spelling the joined form here would be a second
/// carrier of one identity, free to drift from the accepted vocabulary.
const COMPACTION_ENDED: &str = "session.compaction.ended";

/// The exact versioned type of the Rust-native fact, as it is stored in
/// `event.type`, shared by descriptor reads and the cache fold.
const COMPACTION_ENDED_V2: &str = "session.compaction.ended.2";

/// Only this exact legacy spelling may be counted without a native descriptor.
const COMPACTION_ENDED_V1: &str = "session.compaction.ended.1";

/// Why a compaction fact or its derived cache could not be produced or read.
///
/// Deliberately its own type rather than four new [`WriteError`] variants. The
/// append path's error set is consulted by witnesses and mutation rows across
/// three earlier lanes; growing it would rot every manifest snippet that pins
/// its text, for no gain — these failures are this module's, and a caller that
/// wants the underlying append error still has it through [`Self::Append`].
#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error(transparent)]
    Visibility(#[from] crate::history_visibility::VisibilityError),
    #[error("the compaction descriptor could not be appended: {0}")]
    Append(#[from] WriteError),

    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A stored `session.compaction.ended.2` row does not decode as a
    /// descriptor. The fold refuses the aggregate rather than skipping the row:
    /// a cache silently rebuilt from the rows that happened to parse would be a
    /// more permissive reading of durable truth than the replay path's.
    #[error("the stored compaction payload at seq {seq} is not a descriptor: {source}")]
    CorruptPayload { seq: u64, source: PayloadError },

    /// The cache claims to have folded past the aggregate's durable head.
    ///
    /// Nothing in this crate can write that state, which is exactly why the read
    /// fails closed on it: the reachable cause is an external write to a table
    /// the chokepoint does not govern.
    #[error(
        "the compaction cache for {aggregate} stands at {watermark} but its durable head is {head:?}"
    )]
    CacheWatermarkLeads {
        aggregate: String,
        watermark: u64,
        head: Option<u64>,
    },

    /// A stored cache row is not a window. Refused, never coerced to a default.
    #[error("the compaction cache row for {aggregate} is corrupt: {detail}")]
    CorruptCacheRow { aggregate: String, detail: String },
}

/// What one compaction attempt produced.
///
/// The two arms are the whole vocabulary on purpose: a compaction either has a
/// descriptor or has a typed reason it does not. There is no third "partial"
/// arm, because a partial compaction is exactly the marker-plus-copy shape
/// §4.2 forbids.
#[derive(Debug)]
pub enum CompactionAttempt<'a> {
    /// The attempt produced a complete descriptor.
    Settled(&'a CompactionDescriptor),
    /// The attempt produced none, for this exact typed reason.
    Failed(ContextFailure),
}

/// What [`record`] settled.
#[derive(Debug, PartialEq, Eq)]
pub enum CompactionOutcome {
    /// The descriptor is durable at this sequence.
    Settled { seq: u64 },
    /// Nothing was appended. The previous durable history is unchanged and
    /// still recoverable, and this is the exact failure that was submitted.
    Refused { failure: ContextFailure },
}

/// Latest committed descriptor, read from canonical truth rather than the
/// rebuildable window cache. Unknown or corrupt newer boundaries refuse.
pub fn latest_descriptor(
    conn: &rusqlite::Connection,
    session: &grapt_types::SessionId,
) -> Result<Option<(EventId, EventSeq, CompactionDescriptor)>, CompactionError> {
    read_latest_descriptor(conn, session, "1")
}

/// The newest summary still present in the conversation after durable cuts.
/// Physical compaction generations remain available through `latest_descriptor`
/// so subsequent epochs never move backwards after a rewind.
pub fn latest_visible_descriptor(
    conn: &rusqlite::Connection,
    session: &grapt_types::SessionId,
) -> Result<Option<(EventId, EventSeq, CompactionDescriptor)>, CompactionError> {
    let visible = crate::history_visibility::HistoryVisibility::read(conn, session)?.sql("seq");
    read_latest_descriptor(conn, session, &visible)
}

fn read_latest_descriptor(
    conn: &rusqlite::Connection,
    session: &grapt_types::SessionId,
    visible: &str,
) -> Result<Option<(EventId, EventSeq, CompactionDescriptor)>, CompactionError> {
    let row = optional(conn.query_row(
        &format!(
            "SELECT id, seq, type,
         CASE WHEN length(CAST(data AS BLOB)) <= 1048576 THEN data ELSE NULL END
         FROM event WHERE aggregate_family = 'session'
         AND aggregate_id = ?1 AND {visible} AND type LIKE 'session.compaction.ended.%'
         ORDER BY seq DESC LIMIT 1"
        ),
        [session.as_str()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    ))?;
    let Some((id, seq, kind, data)) = row else {
        return Ok(None);
    };
    let invalid = |detail: String| CompactionError::CorruptCacheRow {
        aggregate: session.as_str().to_owned(),
        detail,
    };
    if kind != COMPACTION_ENDED_V2 {
        return Err(invalid(format!(
            "unsupported compacted history boundary {kind}"
        )));
    }
    let seq = u64::try_from(seq).map_err(|_| invalid("negative boundary sequence".to_owned()))?;
    let value =
        serde_json::from_str(&data).map_err(|error| invalid(format!("boundary JSON: {error}")))?;
    let descriptor = session_compaction::decode(&value)
        .map_err(|source| CompactionError::CorruptPayload { seq, source })?;
    if descriptor.source_session() != session {
        return Err(invalid("descriptor names another Session".to_owned()));
    }
    let id = id
        .parse()
        .map_err(|error| invalid(format!("boundary event identity: {error}")))?;
    Ok(Some((id, EventSeq::new(seq), descriptor)))
}

/// Appends one atomic compaction descriptor, or records a typed refusal.
///
/// Runs inside the CALLER's transaction and does not commit: the descriptor,
/// every projector's rows and every watermark commit together or not at all,
/// which is what makes the crash cuts around this call recoverable rather than
/// half-applied.
///
/// The aggregate is taken from the descriptor's own `source_session` rather
/// than from a parameter. A descriptor appended to some other session's log
/// would be a fact about one session stored in another's history; making the
/// aggregate underivable by the caller means that state has no spelling.
pub fn record(
    tx: &GovernedTransaction<'_>,
    id: &EventId,
    writer: FencedWriter,
    created_ms: i64,
    attempt: CompactionAttempt<'_>,
    projectors: &mut [&mut dyn Projector],
) -> Result<CompactionOutcome, CompactionError> {
    let descriptor = match attempt {
        // Nothing is appended. That is the mechanism, not an omission: the
        // previous durable history is left exactly as it was, so a caller can
        // retry, compact differently, or surface the failure without first
        // undoing a partial write.
        //
        // The variant is returned as received. Mapping it — even onto a
        // "compaction failed" catch-all that reads as more honest — would erase
        // the distinction between a retryable failure and a bound that has been
        // reached.
        CompactionAttempt::Failed(failure) => return Ok(CompactionOutcome::Refused { failure }),
        CompactionAttempt::Settled(descriptor) => descriptor,
    };

    let aggregate = AggregateId::Session(descriptor.source_session().clone());
    let payload = session_compaction::encode(descriptor);
    // `unreachable!` rather than `expect`: the workspace denies
    // `clippy::expect_used`, and 2 is a literal that cannot fail the
    // non-zero check.
    let version = match EventVersion::new(2) {
        Some(version) => version,
        None => unreachable!("2 is a non-zero version"),
    };

    // ONE append. The success path below has no second statement to split,
    // which is how "one atomic descriptor, never a marker plus a copy" is
    // carried here.
    let appended = crate::EventStore::append_within(
        tx,
        AppendInput {
            id,
            aggregate,
            writer,
            kind_name: COMPACTION_ENDED,
            version,
            created_ms,
            data: &payload,
        },
        projectors,
    )?;

    let seq = match appended {
        Appended::Committed { seq } | Appended::Idempotent { seq } => seq,
    };
    Ok(CompactionOutcome::Settled { seq })
}

/// The compacted window a session currently stands in, as the cache has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedWindow {
    compaction: CompactionId,
    window: ContextWindowId,
    epoch: ContextEpoch,
    seq: u64,
}

impl CachedWindow {
    pub fn compaction(&self) -> &CompactionId {
        &self.compaction
    }
    pub fn window(&self) -> &ContextWindowId {
        &self.window
    }
    pub fn epoch(&self) -> ContextEpoch {
        self.epoch
    }
    /// The sequence of the descriptor this window came from.
    pub fn seq(&self) -> u64 {
        self.seq
    }
}

/// What one [`rebuild_cache`] pass folded.
#[derive(Debug, PartialEq, Eq)]
pub struct CacheRebuild {
    /// Rust-native descriptors folded on this pass.
    pub folded: u64,
    /// Legacy v1 `session.compaction.ended` rows SEEN and deliberately not
    /// folded.
    ///
    /// v1 is the frozen oracle payload `{sessionID, reason, text, recent}` and
    /// carries no window or epoch, so there is no window it could contribute.
    /// Counted rather than ignored: "the cache is empty" and "the cache is empty
    /// because this session's only compactions predate the Rust-native fact"
    /// are different answers, and a caller that cannot tell them apart would
    /// read a legacy session as never compacted.
    pub legacy_seen: u64,
    /// Where the cache now stands, or `None` when the aggregate has no events.
    pub watermark: Option<u64>,
}

pub(crate) fn ensure_schema(conn: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
    // Consumer-owned tables, created through the same schema-preparation path
    // every other store table uses. They are deliberately absent from
    // `GovernedTable`: the ruling is that rebuildable caches are consumer-owned
    // and enforced on read, so the closed governed set does not grow and no
    // existing governed-table grid changes size.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS compaction_window_cache (
            aggregate_family TEXT NOT NULL,
            aggregate_id TEXT NOT NULL,
            compaction_id TEXT NOT NULL,
            target_window TEXT NOT NULL,
            target_epoch INTEGER NOT NULL,
            seq INTEGER NOT NULL,
            PRIMARY KEY (aggregate_family, aggregate_id)
        );
        CREATE TABLE IF NOT EXISTS compaction_cache_watermark (
            aggregate_family TEXT NOT NULL,
            aggregate_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            PRIMARY KEY (aggregate_family, aggregate_id)
        );",
    )
}

/// Rebuilds one aggregate's compaction cache from canonical truth.
///
/// The input is the `event` table and NOTHING else. The existing cache rows are
/// deleted before the fold rather than updated in place, so a rebuild cannot
/// carry a stale or hostile value forward: a cache that seeded itself from its
/// own previous contents would converge on whatever was written to it, which is
/// precisely the "rebuildable cache became truth" failure §4.2 forbids.
///
/// Rows are folded in sequence order and the LAST Rust-native descriptor wins:
/// compaction epochs are monotonic, so the newest descriptor is the current
/// window.
///
/// The cache rows and the cache watermark are written in the CALLER's
/// transaction, together. A watermark that committed separately could outlive a
/// rolled-back fold and would then claim coverage the rows do not have — the
/// lead state this module refuses on read.
pub fn rebuild_cache(
    tx: &GovernedTransaction<'_>,
    aggregate: &AggregateId,
) -> Result<CacheRebuild, CompactionError> {
    let family = family_of(aggregate);
    let identity = identity_of(aggregate);

    // Every event of this aggregate, in sequence order. The whole log is read
    // rather than only the compaction rows, because the watermark must describe
    // how far the fold has SEEN, not how far it happened to find something
    // interesting. A watermark that advanced only on compaction rows would sit
    // below the head forever on a session that compacted once and then kept
    // working, and every read would report it as lagging.
    let rows: Vec<(i64, String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT seq, type, data FROM event
             WHERE aggregate_family = ?1 AND aggregate_id = ?2
             ORDER BY seq ASC",
        )?;
        let found = stmt.query_map(rusqlite::params![family, identity], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        found.collect::<Result<Vec<_>, _>>()?
    };

    let mut folded = 0_u64;
    let mut legacy_seen = 0_u64;
    let mut watermark: Option<u64> = None;
    let mut current: Option<(CompactionId, ContextWindowId, ContextEpoch, u64)> = None;

    for (seq_db, versioned_type, data_text) in rows {
        let seq = u64::try_from(seq_db).map_err(|_| CompactionError::CorruptCacheRow {
            aggregate: identity.to_owned(),
            detail: format!("event sequence {seq_db} is negative"),
        })?;
        watermark = Some(seq);

        if versioned_type == COMPACTION_ENDED_V2 {
            let value: serde_json::Value =
                serde_json::from_str(&data_text).map_err(|_| CompactionError::CorruptCacheRow {
                    aggregate: identity.to_owned(),
                    detail: format!("the compaction payload at seq {seq} is not JSON"),
                })?;
            // Through F4B1V's accepted strict codec, not a local reader. A
            // second, more permissive reading of the same bytes is how a cache
            // ends up disagreeing with replay about what the log says.
            let descriptor = session_compaction::decode(&value)
                .map_err(|source| CompactionError::CorruptPayload { seq, source })?;
            current = Some((
                descriptor.id().clone(),
                descriptor.target_window().clone(),
                descriptor.target_epoch(),
                seq,
            ));
            folded += 1;
        } else if versioned_type == COMPACTION_ENDED_V1 {
            // A legacy v1 compaction. Counted, never folded: v1 is frozen and
            // carries no window this cache could name.
            legacy_seen += 1;
        } else if versioned_type.starts_with(COMPACTION_ENDED) {
            return Err(CompactionError::CorruptCacheRow {
                aggregate: identity.to_owned(),
                detail: format!("unsupported compacted history boundary {versioned_type}"),
            });
        }
    }

    // Validate the entire fold before replacing any cache state. Even a caller
    // that commits after a refusal cannot advance or erase its previous cache.
    tx.execute(
        "DELETE FROM compaction_window_cache
         WHERE aggregate_family = ?1 AND aggregate_id = ?2",
        rusqlite::params![family, identity],
    )?;
    tx.execute(
        "DELETE FROM compaction_cache_watermark
         WHERE aggregate_family = ?1 AND aggregate_id = ?2",
        rusqlite::params![family, identity],
    )?;

    if let Some((compaction, window, epoch, seq)) = current {
        tx.execute(
            "INSERT INTO compaction_window_cache
                (aggregate_family, aggregate_id, compaction_id, target_window, target_epoch, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                family,
                identity,
                compaction.as_str(),
                window.as_str(),
                i64::try_from(epoch.value()).unwrap_or(i64::MAX),
                i64::try_from(seq).unwrap_or(i64::MAX),
            ],
        )?;
    }
    if let Some(mark) = watermark {
        tx.execute(
            "INSERT INTO compaction_cache_watermark (aggregate_family, aggregate_id, seq)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![family, identity, i64::try_from(mark).unwrap_or(i64::MAX)],
        )?;
    }

    Ok(CacheRebuild {
        folded,
        legacy_seen,
        watermark,
    })
}

/// Reads the cached window, refusing a cache that is not trustworthy.
///
/// This is where "consumer-owned" is paid for. Two states this crate's writer
/// cannot produce are checked before any value is returned:
///
/// * the cache watermark is PAST the aggregate's durable head — a lead, which
///   means the cache is describing events that do not exist;
/// * a stored row does not decode as a window — corruption, refused rather than
///   coerced into a default window that a caller would then act on.
pub fn cached_window(
    conn: &rusqlite::Connection,
    aggregate: &AggregateId,
) -> Result<Option<CachedWindow>, CompactionError> {
    let family = family_of(aggregate);
    let identity = identity_of(aggregate);

    let watermark: Option<i64> = optional(conn.query_row(
        "SELECT seq FROM compaction_cache_watermark
         WHERE aggregate_family = ?1 AND aggregate_id = ?2",
        rusqlite::params![family, identity],
        |r| r.get::<_, i64>(0),
    ))?;

    let head: Option<i64> = optional(conn.query_row(
        "SELECT MAX(seq) FROM event WHERE aggregate_family = ?1 AND aggregate_id = ?2",
        rusqlite::params![family, identity],
        |r| r.get::<_, Option<i64>>(0),
    ))?
    .flatten();

    if let Some(mark) = watermark {
        let mark = u64::try_from(mark).map_err(|_| CompactionError::CorruptCacheRow {
            aggregate: identity.to_owned(),
            detail: format!("the cache watermark {mark} is negative"),
        })?;
        let head_u = match head {
            None => None,
            Some(value) => {
                Some(
                    u64::try_from(value).map_err(|_| CompactionError::CorruptCacheRow {
                        aggregate: identity.to_owned(),
                        detail: format!("the durable head {value} is negative"),
                    })?,
                )
            }
        };
        // May lag; may never lead. `None` head with any watermark is also a
        // lead: a cache cannot have folded an event of an aggregate that has
        // none.
        let leads = match head_u {
            None => true,
            Some(head_value) => mark > head_value,
        };
        if leads {
            return Err(CompactionError::CacheWatermarkLeads {
                aggregate: identity.to_owned(),
                watermark: mark,
                head: head_u,
            });
        }
    }

    let row: Option<(String, String, i64, i64)> = optional(conn.query_row(
        "SELECT compaction_id, target_window, target_epoch, seq FROM compaction_window_cache
         WHERE aggregate_family = ?1 AND aggregate_id = ?2",
        rusqlite::params![family, identity],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        },
    ))?;

    let Some((compaction_raw, window_raw, epoch_db, seq_db)) = row else {
        return Ok(None);
    };

    let corrupt = |detail: String| CompactionError::CorruptCacheRow {
        aggregate: identity.to_owned(),
        detail,
    };

    // Each coordinate goes back through its own validating constructor. An
    // empty id, a negative epoch or a negative sequence is a refusal, never a
    // default — the whole reason the read side is where these caches are
    // policed.
    let compaction = CompactionId::new(&compaction_raw)
        .ok_or_else(|| corrupt("the cached compaction id is empty".to_owned()))?;
    let window = ContextWindowId::new(&window_raw)
        .ok_or_else(|| corrupt("the cached context window id is empty".to_owned()))?;
    let epoch = u64::try_from(epoch_db)
        .map_err(|_| corrupt(format!("the cached target epoch {epoch_db} is negative")))?;
    let seq = u64::try_from(seq_db).map_err(|_| {
        corrupt(format!(
            "the cached descriptor sequence {seq_db} is negative"
        ))
    })?;

    Ok(Some(CachedWindow {
        compaction,
        window,
        epoch: ContextEpoch::new(epoch),
        seq,
    }))
}

/// The parent sequences a fork at `boundary` may reconstruct.
///
/// Ratified §4.1: "a fork whose boundary precedes a compaction checkpoint can
/// reconstruct the pre-compaction history still present in the append-only
/// parent lineage."
///
/// The cap is the whole mechanism. A fork taken BEFORE a compaction sees the
/// parent's pre-compaction events and does NOT see the compaction descriptor or
/// anything after it, because [`grapt_types::LineageCap::admits`] refuses every
/// sequence past the boundary. Admitting past the cap would hand the child the
/// post-compaction truth its boundary was drawn to exclude.
///
/// The filter is applied in Rust over the parent's rows rather than as a SQL
/// `seq <= ?`, so the ACCEPTED [`grapt_types::LineageCap`] is the single
/// carrier of the boundary rule. A hand-written SQL bound would be a second
/// expression of the same law, free to disagree with the type that owns it.
pub fn admitted_parent_history(
    conn: &rusqlite::Connection,
    boundary: &ForkBoundary,
) -> Result<Vec<u64>, CompactionError> {
    let parent = AggregateId::Session(boundary.parent().clone());
    let cap = grapt_types::LineageCap::Own.narrow(boundary.parent_seq());

    let mut stmt = conn.prepare(
        "SELECT seq FROM event
         WHERE aggregate_family = ?1 AND aggregate_id = ?2
         ORDER BY seq ASC",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![family_of(&parent), identity_of(&parent)],
        |r| r.get::<_, i64>(0),
    )?;

    let mut admitted = Vec::new();
    for row in rows {
        let seq_db = row?;
        let Ok(seq) = u64::try_from(seq_db) else {
            return Err(CompactionError::CorruptCacheRow {
                aggregate: identity_of(&parent).to_owned(),
                detail: format!("parent event sequence {seq_db} is negative"),
            });
        };
        if cap.admits(EventSeq::new(seq)) {
            admitted.push(seq);
        }
    }
    Ok(admitted)
}

/// `QueryReturnedNoRows` is absence, not failure.
fn optional<T>(result: rusqlite::Result<T>) -> Result<Option<T>, rusqlite::Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::SessionId;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// An elected in-memory store with one ordinary durable fact, so the
    /// aggregate has a head for a cache watermark to be compared against.
    fn seeded() -> Result<(crate::EventStore, AggregateId), Box<dyn std::error::Error>> {
        let mut store = crate::EventStore::open_in_memory()?;
        let session: SessionId = "ses_033601c26ffe".parse()?;
        let aggregate = AggregateId::Session(session.clone());
        store.elect_writer(&aggregate)?;
        let writer = store.fencing_token(&aggregate)?;
        let payload = serde_json::json!({ "sessionID": session.as_str(), "inputID": "seed" });
        let (_appended, notices) = store.append(
            AppendInput {
                id: &"evt_0336000000one".parse::<EventId>()?,
                aggregate: aggregate.clone(),
                writer,
                kind_name: "session.input.admitted",
                version: match EventVersion::new(1) {
                    Some(version) => version,
                    None => unreachable!("1 is a non-zero version"),
                },
                created_ms: 1_786_147_200_000,
                data: &payload,
            },
            &mut [],
        )?;
        notices.discard();
        Ok((store, aggregate))
    }

    /// LAW 5: a cache watermark PAST the aggregate's durable head is refused.
    ///
    /// The state is staged directly, because `rebuild_cache` writes the
    /// watermark from the same fold that wrote the rows and therefore cannot
    /// produce a lead. The reachable cause is a write to a table the chokepoint
    /// does not govern — which is exactly the cost of the consumer-owned ruling,
    /// paid here.
    #[test]
    fn a_cache_watermark_past_the_durable_head_is_refused() -> TestResult {
        let (store, aggregate) = seeded()?;
        // The seeded aggregate's head is sequence 0. Claim to have folded 7.
        hostile_cache::install_watermark(store.connection(), &aggregate, 7)?;

        match cached_window(store.connection(), &aggregate) {
            Err(CompactionError::CacheWatermarkLeads {
                watermark, head, ..
            }) => {
                assert_eq!(watermark, 7, "the refusal must name the leading watermark");
                assert_eq!(head, Some(0), "the refusal must name the durable head");
                Ok(())
            }
            other => panic!("a leading cache watermark must refuse: {other:?}"),
        }
    }

    /// LAW 5, the boundary: a watermark EQUAL to the head is caught up, not a
    /// lead.
    ///
    /// The negative control for the witness above. Without it, a refusal that
    /// fired on `>=` instead of `>` would look identical — the lead witness
    /// would still be green while every honest caught-up cache was refused.
    #[test]
    fn a_cache_watermark_equal_to_the_durable_head_is_not_a_lead() -> TestResult {
        let (store, aggregate) = seeded()?;
        hostile_cache::install_watermark(store.connection(), &aggregate, 0)?;

        match cached_window(store.connection(), &aggregate) {
            Ok(None) => Ok(()),
            other => panic!("a caught-up cache must not be refused as a lead: {other:?}"),
        }
    }

    /// LAW 5: any watermark at all over an aggregate with NO events is a lead.
    ///
    /// A cache cannot have folded an event of an aggregate that has none, so
    /// this is not a lag with an absent head — it is a claim about events that
    /// do not exist.
    #[test]
    fn a_watermark_over_an_empty_aggregate_is_a_lead() -> TestResult {
        let mut store = crate::EventStore::open_in_memory()?;
        let session: SessionId = "ses_033601c26ffe".parse()?;
        let aggregate = AggregateId::Session(session);
        store.elect_writer(&aggregate)?;
        hostile_cache::install_watermark(store.connection(), &aggregate, 0)?;

        match cached_window(store.connection(), &aggregate) {
            Err(CompactionError::CacheWatermarkLeads { head: None, .. }) => Ok(()),
            other => panic!("a watermark over an empty aggregate must refuse: {other:?}"),
        }
    }

    /// LAW 4: a corrupt cache row is REFUSED, never coerced into a value.
    ///
    /// An empty compaction id is not a window with a blank name; it is a row
    /// that does not decode. Coercing it — to a default, to `None`, to an empty
    /// string — would hand a caller a window it would then act on.
    #[test]
    fn a_cache_row_with_an_empty_compaction_id_is_refused() -> TestResult {
        let (store, aggregate) = seeded()?;
        hostile_cache::install_watermark(store.connection(), &aggregate, 0)?;
        hostile_cache::install_window(store.connection(), &aggregate, "", "cwn-1", 2, 0)?;

        match cached_window(store.connection(), &aggregate) {
            Err(CompactionError::CorruptCacheRow { detail, .. }) => {
                assert!(
                    detail.contains("compaction id"),
                    "the refusal must name the corrupt coordinate, got {detail}"
                );
                Ok(())
            }
            other => panic!("an empty cached compaction id must refuse: {other:?}"),
        }
    }

    /// LAW 4: a negative stored epoch is refused rather than wrapped.
    ///
    /// The separate coordinate matters: proving the id refuses says nothing
    /// about the epoch, and a `as u64` cast would turn -1 into an enormous
    /// epoch that compares as newer than every real one.
    #[test]
    fn a_cache_row_with_a_negative_epoch_is_refused() -> TestResult {
        let (store, aggregate) = seeded()?;
        hostile_cache::install_watermark(store.connection(), &aggregate, 0)?;
        hostile_cache::install_window(store.connection(), &aggregate, "cmp-1", "cwn-1", -1, 0)?;

        match cached_window(store.connection(), &aggregate) {
            Err(CompactionError::CorruptCacheRow { detail, .. }) => {
                assert!(
                    detail.contains("target epoch"),
                    "the refusal must name the corrupt coordinate, got {detail}"
                );
                Ok(())
            }
            other => panic!("a negative cached epoch must refuse: {other:?}"),
        }
    }

    /// LAW 4: a well-formed staged row still READS, so the refusals above are
    /// the corruption check firing rather than the read path refusing every
    /// staged row.
    ///
    /// Without this control, "nothing reads" and "corruption is refused" are
    /// indistinguishable.
    #[test]
    fn a_well_formed_staged_cache_row_reads() -> TestResult {
        let (store, aggregate) = seeded()?;
        hostile_cache::install_watermark(store.connection(), &aggregate, 0)?;
        hostile_cache::install_window(store.connection(), &aggregate, "cmp-1", "cwn-1", 2, 0)?;

        let window = cached_window(store.connection(), &aggregate)?
            .ok_or("a well-formed staged row must read back")?;
        assert_eq!(window.compaction().as_str(), "cmp-1");
        assert_eq!(window.window().as_str(), "cwn-1");
        assert_eq!(window.epoch().value(), 2);
        Ok(())
    }

    #[test]
    fn unsupported_compaction_version_preserves_cache_even_if_caller_commits() -> TestResult {
        for version in [3, 20] {
            let (mut store, aggregate) = seeded()?;
            hostile_cache::install_watermark(store.connection(), &aggregate, 0)?;
            hostile_cache::install_window(
                store.connection(),
                &aggregate,
                "cmp-old",
                "cwn-old",
                2,
                0,
            )?;
            let kind = format!("{COMPACTION_ENDED}.{version}");
            let gate = store.gate().clone();
            crate::write::hostile_rows::install_event(
                store.connection(),
                &gate,
                crate::write::hostile_rows::ForeignEvent {
                    aggregate: &aggregate,
                    id: "evt_unsupported_compaction",
                    seq: 1,
                    created_ms: 100,
                    versioned_type: &kind,
                    data_text: r#"{"sessionID":"ses_033601c26ffe","reason":"manual","text":"legacy","recent":[]}"#,
                    writer_epoch: Some(1),
                },
            )?;
            let tx = store.governed_transaction()?;
            let outcome = rebuild_cache(&tx, &aggregate);
            assert!(
                matches!(&outcome,
                Err(CompactionError::CorruptCacheRow { detail, .. })
                    if detail.contains("unsupported") && detail.contains(&kind)),
                "unsupported boundary was folded: {outcome:?}"
            );
            tx.commit()?.discard();
            let window = cached_window(store.connection(), &aggregate)?
                .ok_or("refusal erased the prior window")?;
            assert_eq!(window.compaction().as_str(), "cmp-old");
            assert_eq!(window.window().as_str(), "cwn-old");
            let mark: i64 = store.connection().query_row(
                "SELECT seq FROM compaction_cache_watermark WHERE aggregate_family = ?1 AND aggregate_id = ?2",
                rusqlite::params![family_of(&aggregate), identity_of(&aggregate)],
                |row| row.get(0),
            )?;
            assert_eq!(mark, 0, "refusal changed the previous cache progress");
        }
        Ok(())
    }

    /// LAW 4: a stored `session.compaction.ended.2` row whose PAYLOAD is not a
    /// descriptor refuses the fold rather than being skipped.
    ///
    /// Skipping it would rebuild the cache from whichever rows happened to
    /// parse — a more permissive reading of durable truth than the replay path
    /// allows, and one that would quietly report an older window as current.
    #[test]
    fn a_corrupt_stored_payload_refuses_the_fold() -> TestResult {
        let (mut store, aggregate) = seeded()?;
        // Staged through the store's own hostile-row seam: the `event` table is
        // governed, so this needs the store's lift like any other governed write.
        let gate = store.gate().clone();
        crate::write::hostile_rows::install_event(
            store.connection(),
            &gate,
            crate::write::hostile_rows::ForeignEvent {
                aggregate: &aggregate,
                id: "evt_033600000bad",
                seq: 1,
                created_ms: 1_786_147_200_000,
                versioned_type: "session.compaction.ended.2",
                data_text: "{\"compactionID\":\"cmp-1\"}",
                writer_epoch: Some(1),
            },
        )?;

        let tx = store.governed_transaction()?;
        let outcome = rebuild_cache(&tx, &aggregate);
        match outcome {
            Err(CompactionError::CorruptPayload { seq, .. }) => {
                assert_eq!(seq, 1, "the refusal must name the offending sequence");
                Ok(())
            }
            other => panic!("a corrupt stored payload must refuse: {other:?}"),
        }
    }
}

/// Deliberate injection of cache rows this module's own writer can never
/// produce.
///
/// The two laws [`cached_window`] enforces — corruption refuses, and a cache may
/// never lead — are unreachable through the public path by construction:
/// [`rebuild_cache`] writes only validated coordinates, and it writes the
/// watermark from the same fold that wrote the rows. Staging those states is
/// therefore white-box by necessity, and it is collected here so every such
/// write is named in one place.
///
/// `#[cfg(test)]` seals the module out of production builds entirely. A cargo
/// feature would not: features are additive, so any dependent in the graph could
/// switch this on for everyone.
#[cfg(test)]
pub(crate) mod hostile_cache {
    use super::*;

    /// Writes a cache watermark verbatim, with no fold behind it.
    ///
    /// These tables are consumer-owned, so this needs no lift — and that is the
    /// finding, not a convenience. It is exactly the reach an ordinary consumer
    /// has, which is why the read side has to police it.
    pub(crate) fn install_watermark(
        conn: &rusqlite::Connection,
        aggregate: &AggregateId,
        seq: i64,
    ) -> Result<usize, rusqlite::Error> {
        conn.execute(
            "INSERT INTO compaction_cache_watermark (aggregate_family, aggregate_id, seq)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(aggregate_family, aggregate_id) DO UPDATE SET seq = excluded.seq",
            rusqlite::params![family_of(aggregate), identity_of(aggregate), seq],
        )
    }

    /// Writes a cache window row verbatim, bypassing the fold.
    pub(crate) fn install_window(
        conn: &rusqlite::Connection,
        aggregate: &AggregateId,
        compaction_id: &str,
        target_window: &str,
        target_epoch: i64,
        seq: i64,
    ) -> Result<usize, rusqlite::Error> {
        conn.execute(
            "INSERT INTO compaction_window_cache
                (aggregate_family, aggregate_id, compaction_id, target_window, target_epoch, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(aggregate_family, aggregate_id) DO UPDATE SET
                compaction_id = excluded.compaction_id,
                target_window = excluded.target_window,
                target_epoch = excluded.target_epoch,
                seq = excluded.seq",
            rusqlite::params![
                family_of(aggregate),
                identity_of(aggregate),
                compaction_id,
                target_window,
                target_epoch,
                seq
            ],
        )
    }
}
