mod request;
pub(super) use request::{blob_size as request_blob_size, fold_request_history};

use super::{
    InstructionChange, InstructionError, ResolvedInstruction, SkippedInstructionEvent,
    SourceObservation,
};
use grapt_store::DecodedEvent;
use grapt_types::{
    EventSeq, ForkBoundary, ForkLineage, ForkLineageBuilder, InstructionEpoch, InstructionHash,
    InstructionKey, SessionId,
};
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::projection::{
    decode_compaction_ended, decode_compaction_ended_v2, decode_event_delta, decode_fork,
    decode_moved, decode_revert_committed,
};
use super::storage::{load_blob, write_state};

#[derive(Debug)]
pub(super) enum Observed {
    Available {
        hash: InstructionHash,
        value: String,
        once: bool,
    },
    Unavailable,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CachedState {
    pub(super) epoch: InstructionEpoch,
    pub(super) through: EventSeq,
    pub(super) initial: BTreeMap<InstructionKey, InstructionHash>,
    pub(super) current: BTreeMap<InstructionKey, InstructionHash>,
}

#[derive(Debug)]
pub(super) struct FoldedHistory {
    pub(super) state: CachedState,
    pub(super) updates: Vec<RawInstructionUpdate>,
    owner: SessionId,
}

#[derive(Debug)]
pub(super) struct RawInstructionUpdate {
    pub(super) owner: SessionId,
    pub(super) seq: EventSeq,
    pub(super) delta: BTreeMap<InstructionKey, Option<InstructionHash>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CacheStatus {
    Present,
    Missing,
    LegacyFork,
    /// The fold skipped rows, so it cannot judge a cache a full reader wrote.
    Degraded,
}

/// A fold of durable history and the rows it had to leave out.
#[derive(Debug, Default)]
pub(super) struct Fold {
    pub(super) history: Option<FoldedHistory>,
    pub(super) skipped: Vec<SkippedInstructionEvent>,
}

impl Fold {
    pub(super) fn state(&self) -> Option<&CachedState> {
        self.history.as_ref().map(|history| &history.state)
    }

    /// The state a writer may persist. A degraded fold persists none, so the
    /// next reader that can decode every row rebuilds the cache from the log.
    pub(super) fn persistable(&self) -> Option<&CachedState> {
        if self.skipped.is_empty() {
            self.state()
        } else {
            None
        }
    }
}

/// The decoded meaning of one instruction-relevant journal row.
enum Payload {
    Updated(BTreeMap<InstructionKey, Option<InstructionHash>>),
    CompactionEnded,
    Cleared,
    Unrelated,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FoldMode {
    Current,
    LegacyFork,
}

pub(super) fn decode_required(
    required: Vec<InstructionKey>,
) -> Result<BTreeSet<InstructionKey>, InstructionError> {
    required
        .into_iter()
        .try_fold(BTreeSet::new(), |mut set, key| {
            if !set.insert(key.clone()) {
                return Err(InstructionError::DuplicateRequired {
                    key: key.to_string(),
                });
            }
            Ok(set)
        })
}

pub(super) fn decode_observations(
    observations: Vec<SourceObservation>,
) -> Result<BTreeMap<InstructionKey, Observed>, InstructionError> {
    observations
        .into_iter()
        .try_fold(BTreeMap::new(), |mut decoded, observation| {
            let key = observation.key().clone();
            let once = matches!(observation, SourceObservation::AvailableOnce { .. });
            let value = match observation {
                SourceObservation::Available { value, .. }
                | SourceObservation::AvailableOnce { value, .. } => {
                    let value = value.stored();
                    Observed::Available {
                        hash: hash_value(&value),
                        value,
                        once,
                    }
                }
                SourceObservation::Unavailable { .. } => Observed::Unavailable,
                SourceObservation::Removed { .. } => Observed::Removed,
            };
            if decoded.insert(key.clone(), value).is_some() {
                return Err(InstructionError::DuplicateObservation {
                    key: key.to_string(),
                });
            }
            Ok(decoded)
        })
}

fn hash_value(value: &str) -> InstructionHash {
    InstructionHash::from_bytes(Sha256::digest(value.as_bytes()).into())
}

/// Fold a Session's whole lineage. A row whose envelope or payload cannot be
/// decoded is skipped and named in [`Fold::skipped`]; ordering and blob
/// failures still fail the fold.
pub(super) fn fold_history(
    conn: &Connection,
    session: &SessionId,
) -> Result<Fold, InstructionError> {
    fold_segments(conn, &own_lineage(conn, session)?)
}

pub(super) fn fold_parent_history(
    conn: &Connection,
    fork: &ForkBoundary,
) -> Result<Fold, InstructionError> {
    fold_segments(conn, &inherited_lineage(conn, fork)?)
}

/// Check the cache against a fold, unless the fold is degraded.
pub(super) fn check_cache(
    conn: &Connection,
    session: &SessionId,
    fold: &Fold,
) -> Result<CacheStatus, InstructionError> {
    if !fold.skipped.is_empty() {
        return Ok(CacheStatus::Degraded);
    }
    validate_cache(conn, session, fold.state())
}

fn fold_segments(conn: &Connection, lineage: &ForkLineage) -> Result<Fold, InstructionError> {
    let mut folded = None;
    let mut skipped = Vec::new();
    for segment in lineage.segments() {
        let mut stmt = conn.prepare(
            "SELECT id, seq, created, type, data FROM event
             WHERE aggregate_id = ?1
               AND (?2 IS NULL OR seq <= ?2)
               AND (type LIKE 'session.instructions.updated.%'
                 OR type LIKE 'session.compaction.ended.%'
                 OR type LIKE 'session.moved.%'
                 OR type LIKE 'session.revert.committed.%')
             ORDER BY seq",
        )?;
        // `Own` lowers to SQL NULL (uncapped); an inherited cap lowers to its
        // exact frozen integer, so `ParentSeq::Nothing` stays `-1` and matches
        // no row rather than becoming an uncapped read.
        let cap = segment.cap().bound().map(grapt_types::ParentSeq::as_raw);
        let mut rows = stmt.query(rusqlite::params![segment.session().as_str(), cap])?;
        while let Some(row) = rows.next()? {
            let decoded = decode_journal_row(row, segment.session());
            match decoded.and_then(|decoded| row_blobs(conn, &decoded.1).map(|()| decoded)) {
                Ok((event, payload)) => {
                    apply_payload(&mut folded, &event, payload, FoldMode::Current)?;
                }
                Err(error) if row_damage(&error) => {
                    skipped.push(skip(row, segment.session(), &error));
                }
                Err(error) => return Err(error),
            }
        }
    }
    Ok(Fold {
        history: folded,
        skipped,
    })
}

pub(super) fn apply_history_event(
    conn: &Connection,
    folded: &mut Option<FoldedHistory>,
    event: &DecodedEvent,
) -> Result<(), InstructionError> {
    apply_history_event_with_mode(conn, folded, event, FoldMode::Current)
}

fn apply_history_event_with_mode(
    conn: &Connection,
    folded: &mut Option<FoldedHistory>,
    event: &DecodedEvent,
    mode: FoldMode,
) -> Result<(), InstructionError> {
    let payload = decode_payload(event)?;
    row_blobs(conn, &payload)?;
    apply_payload(folded, event, payload, mode)
}

/// Every blob an update row names reads as the content it addresses.
fn row_blobs(conn: &Connection, payload: &Payload) -> Result<(), InstructionError> {
    if let Payload::Updated(delta) = payload {
        for hash in delta.values().flatten() {
            load_blob(conn, *hash)?;
        }
    }
    Ok(())
}

/// Whether `error` is damage to the row being folded, which the fold skips
/// and names, rather than a failure of the database reading it.
fn row_damage(error: &InstructionError) -> bool {
    match error {
        InstructionError::Sqlite(error) => !grapt_store::sqlite_storage_failure(error),
        InstructionError::EntryBound { .. } => false,
        _ => true,
    }
}

/// Validate one row's payload for its kind and version.
/// Read and decode one journal row (`id, seq, created, type, data`). Every
/// failure, a column SQLite cannot read as its type included, belongs to the
/// row, so the caller skips it rather than ending the fold.
fn decode_journal_row(
    row: &rusqlite::Row<'_>,
    session: &SessionId,
) -> Result<(DecodedEvent, Payload), InstructionError> {
    let id: String = row.get(0)?;
    let seq: i64 = row.get(1)?;
    let created: i64 = row.get(2)?;
    let kind: String = row.get(3)?;
    let data: String = row.get(4)?;
    let event = grapt_store::decode::decode_row(&id, session.as_str(), seq, created, &kind, &data)?;
    let payload = decode_payload(&event)?;
    Ok((event, payload))
}

/// The marker for a row [`decode_journal_row`] could not decode.
fn skip(
    row: &rusqlite::Row<'_>,
    session: &SessionId,
    error: &InstructionError,
) -> SkippedInstructionEvent {
    SkippedInstructionEvent {
        session: session.clone(),
        seq: row
            .get::<_, i64>(1)
            .ok()
            .and_then(|seq| u64::try_from(seq).ok())
            .map(EventSeq::new),
        reason: error.to_string(),
    }
}

fn decode_payload(event: &DecodedEvent) -> Result<Payload, InstructionError> {
    match (event.kind.name(), event.kind.version().value()) {
        ("session.instructions.updated", 2) => Ok(Payload::Updated(decode_event_delta(
            &event.aggregate_id,
            &event.data,
        )?)),
        ("session.compaction.ended", 1) => {
            decode_compaction_ended(event)?;
            Ok(Payload::CompactionEnded)
        }
        // The Rust-native boundary. SAME epoch semantics as v1 — this is one
        // semantic boundary carried at two payload versions, not two events —
        // and an EXPLICIT arm rather than a widened v1 pattern, so the two
        // payload contracts can never validate each other.
        ("session.compaction.ended", 2) => {
            decode_compaction_ended_v2(event)?;
            Ok(Payload::CompactionEnded)
        }
        ("session.moved", 1) => {
            decode_moved(event)?;
            Ok(Payload::Cleared)
        }
        ("session.revert.committed", 1) => {
            decode_revert_committed(event)?;
            Ok(Payload::Cleared)
        }
        ("session.instructions.updated", version)
        | ("session.compaction.ended", version)
        | ("session.moved", version)
        | ("session.revert.committed", version) => Err(InstructionError::UnsupportedVersion {
            kind: event.kind.name().to_owned(),
            version,
        }),
        _ => Ok(Payload::Unrelated),
    }
}

fn apply_payload(
    folded: &mut Option<FoldedHistory>,
    event: &DecodedEvent,
    payload: Payload,
    mode: FoldMode,
) -> Result<(), InstructionError> {
    match payload {
        Payload::Updated(delta) => {
            if let Some(history) = folded {
                enter_aggregate(history, event, mode);
                ensure_monotonic(&history.state, event)?;
                apply_delta(&mut history.state.current, &delta);
                history.state.through = event.seq;
                history.updates.push(RawInstructionUpdate {
                    owner: event.aggregate_id.clone(),
                    seq: event.seq,
                    delta,
                });
                return Ok(());
            }
            let mut current = BTreeMap::new();
            apply_delta(&mut current, &delta);
            *folded = Some(FoldedHistory {
                state: CachedState {
                    epoch: InstructionEpoch::new(event.seq.value()),
                    through: event.seq,
                    initial: current.clone(),
                    current,
                },
                updates: Vec::new(),
                owner: event.aggregate_id.clone(),
            });
        }
        Payload::CompactionEnded => {
            if let Some(history) = folded {
                enter_aggregate(history, event, mode);
                ensure_monotonic(&history.state, event)?;
                history.state.epoch = InstructionEpoch::new(event.seq.value());
                history.state.through = event.seq;
                history.state.initial = history.state.current.clone();
                history.updates.clear();
            }
        }
        Payload::Cleared => {
            if let Some(history) = folded {
                enter_aggregate(history, event, mode);
                ensure_monotonic(&history.state, event)?;
            }
            *folded = None;
        }
        Payload::Unrelated => {}
    }
    Ok(())
}

// A fork can copy fewer conversation rows than its parent has journal rows.
// When its own numbering restarts below the inherited epoch, carry the frozen
// effective instructions as its baseline. Parent positions cannot be sorted
// among the child's conversation. Existing increasing histories keep their
// established epoch and cache representation.
fn enter_aggregate(history: &mut FoldedHistory, event: &DecodedEvent, mode: FoldMode) {
    if history.owner == event.aggregate_id {
        return;
    }
    if mode == FoldMode::Current && event.seq <= history.state.through {
        history.state.epoch = InstructionEpoch::new(0);
        history.state.through = EventSeq::FIRST;
        history.state.initial = history.state.current.clone();
        history.updates.clear();
    }
    history.owner = event.aggregate_id.clone();
}

fn ensure_monotonic(state: &CachedState, event: &DecodedEvent) -> Result<(), InstructionError> {
    if event.seq > state.through {
        return Ok(());
    }
    Err(InstructionError::Corruption {
        reason: format!(
            "instruction history event {} at {} does not follow {}",
            event.kind.name(),
            event.seq.value(),
            state.through.value()
        ),
    })
}

pub(super) fn apply_delta(
    current: &mut BTreeMap<InstructionKey, InstructionHash>,
    delta: &BTreeMap<InstructionKey, Option<InstructionHash>>,
) {
    for (key, hash) in delta {
        match hash {
            Some(hash) => {
                current.insert(key.clone(), *hash);
            }
            None => {
                current.remove(key);
            }
        }
    }
}

/// Walk the fork chain upward from `session`, reading its own history uncapped.
fn own_lineage(conn: &Connection, session: &SessionId) -> Result<ForkLineage, InstructionError> {
    walk(conn, ForkLineageBuilder::own(session.clone()))
}

/// Walk what a child inherits: start at the parent, already capped by the
/// boundary, and never read the child's own aggregate.
fn inherited_lineage(
    conn: &Connection,
    boundary: &ForkBoundary,
) -> Result<ForkLineage, InstructionError> {
    walk(conn, ForkLineageBuilder::inherited(boundary))
}

/// Drive the promoted builder upward one durable boundary at a time.
///
/// This function owns only the lookup. Cap narrowing, cycle refusal and
/// segment ordering belong to `grapt_types::lineage` and are witnessed there
/// without a database.
fn walk(
    conn: &Connection,
    mut builder: ForkLineageBuilder,
) -> Result<ForkLineage, InstructionError> {
    let mut depth = 1usize;
    while let Some(boundary) = load_fork(conn, builder.head().session())? {
        if depth >= 128 {
            return Err(InstructionError::EntryBound {
                bound: "instruction fork depth",
            });
        }
        builder.descend(&boundary)?;
        depth += 1;
    }
    Ok(builder.finish())
}

pub(super) fn load_fork(
    conn: &Connection,
    session: &SessionId,
) -> Result<Option<ForkBoundary>, InstructionError> {
    let mut stmt = conn.prepare(
        "SELECT id, seq, created, type, data FROM event
         WHERE aggregate_id = ?1 AND type LIKE 'session.forked.%' ORDER BY seq",
    )?;
    let mut rows = stmt.query([session.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let id: String = row.get(0)?;
    let seq: i64 = row.get(1)?;
    let created: i64 = row.get(2)?;
    let kind: String = row.get(3)?;
    let data: String = row.get(4)?;
    let event = grapt_store::decode::decode_row(&id, session.as_str(), seq, created, &kind, &data)?;
    let fork = decode_fork(&event)?;
    if rows.next()?.is_some() {
        return Err(InstructionError::Corruption {
            reason: format!("session {session} has multiple durable fork events"),
        });
    }
    Ok(Some(fork))
}

pub(super) fn validate_cache(
    conn: &Connection,
    session: &SessionId,
    expected: Option<&CachedState>,
) -> Result<CacheStatus, InstructionError> {
    let actual = load_state(conn, session)?;
    if let Some(actual) = &actual {
        validate_state_blobs(conn, actual)?;
    }
    match (actual, expected) {
        (None, None) => Ok(CacheStatus::Present),
        (None, Some(_)) => Ok(CacheStatus::Missing),
        (Some(_), None) => Err(InstructionError::Corruption {
            reason: "instruction_state exists without active durable instruction history".into(),
        }),
        (Some(actual), Some(expected)) if &actual == expected => Ok(CacheStatus::Present),
        (Some(actual), Some(_))
            if request::legacy_fork_history(conn, session)?
                .is_some_and(|legacy| legacy.state == actual) =>
        {
            // A discardable projection written by the old fork numbering is
            // recognized only by exact independent journal reconstruction.
            // Reads use current truth; refresh republishes it transactionally.
            Ok(CacheStatus::LegacyFork)
        }
        (Some(_), Some(_)) => Err(InstructionError::Corruption {
            reason: "instruction_state diverges from independently folded durable history".into(),
        }),
    }
}

pub(super) fn write_folded_state(
    tx: &Transaction<'_>,
    session: &SessionId,
    state: Option<&CachedState>,
) -> Result<(), InstructionError> {
    let Some(state) = state else {
        tx.execute(
            "DELETE FROM instruction_state WHERE session_id = ?1",
            [session.as_str()],
        )?;
        return Ok(());
    };
    write_state(
        tx,
        session,
        state.epoch,
        state.through,
        &state.initial,
        &state.current,
    )
}

fn load_state(
    conn: &Connection,
    session: &SessionId,
) -> Result<Option<CachedState>, InstructionError> {
    let row = conn
        .query_row(
            "SELECT epoch_start, through_seq, initial_values, current_values
             FROM instruction_state WHERE session_id = ?1",
            [session.as_str()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    row.map(|(epoch, through, initial, current)| {
        let epoch = u64::try_from(epoch).map_err(|_| InstructionError::Corruption {
            reason: "instruction epoch is negative".into(),
        })?;
        let through = u64::try_from(through).map_err(|_| InstructionError::Corruption {
            reason: "instruction through sequence is negative".into(),
        })?;
        if through < epoch {
            return Err(InstructionError::Corruption {
                reason: "instruction through sequence precedes its epoch".into(),
            });
        }
        Ok(CachedState {
            epoch: InstructionEpoch::new(epoch),
            through: EventSeq::new(through),
            initial: decode_hash_map(&initial, "initial_values")?,
            current: decode_hash_map(&current, "current_values")?,
        })
    })
    .transpose()
}

fn decode_hash_map(
    raw: &str,
    field: &'static str,
) -> Result<BTreeMap<InstructionKey, InstructionHash>, InstructionError> {
    let value: Value =
        serde_json::from_str(raw).map_err(|source| InstructionError::Corruption {
            reason: format!("{field} is invalid JSON: {source}"),
        })?;
    let Value::Object(map) = value else {
        return Err(InstructionError::Corruption {
            reason: format!("{field} is not an object"),
        });
    };
    map.into_iter()
        .map(|(key, hash)| {
            let key = InstructionKey::new(key).ok_or_else(|| InstructionError::Corruption {
                reason: format!("{field} contains an empty source key"),
            })?;
            let Value::String(hash) = hash else {
                return Err(InstructionError::Corruption {
                    reason: format!("{field} contains a non-string hash for {key}"),
                });
            };
            let hash = hash
                .parse()
                .map_err(|source| InstructionError::Corruption {
                    reason: format!("{field} contains an invalid hash for {key}: {source}"),
                })?;
            Ok((key, hash))
        })
        .collect()
}

fn validate_state_blobs(conn: &Connection, state: &CachedState) -> Result<(), InstructionError> {
    state
        .initial
        .values()
        .chain(state.current.values())
        .copied()
        .collect::<HashSet<_>>()
        .into_iter()
        .try_for_each(|hash| load_blob(conn, hash).map(|_| ()))
}

pub(super) fn resolve_map(
    conn: &Connection,
    values: &BTreeMap<InstructionKey, InstructionHash>,
) -> Result<Vec<ResolvedInstruction>, InstructionError> {
    values
        .iter()
        .map(|(key, hash)| {
            Ok(ResolvedInstruction {
                key: key.clone(),
                value: load_blob(conn, *hash)?,
            })
        })
        .collect()
}

pub(super) fn resolve_delta(
    conn: &Connection,
    delta: &BTreeMap<InstructionKey, Option<InstructionHash>>,
) -> Result<Vec<InstructionChange>, InstructionError> {
    delta
        .iter()
        .map(|(key, hash)| match hash {
            Some(hash) => Ok(InstructionChange::Value(ResolvedInstruction {
                key: key.clone(),
                value: load_blob(conn, *hash)?,
            })),
            None => Ok(InstructionChange::Removed { key: key.clone() }),
        })
        .collect()
}
