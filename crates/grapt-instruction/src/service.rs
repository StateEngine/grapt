//! Durable instruction application service and rebuildable Session-owned cache.
//!
//! The durable log contains only changed source keys and content hashes. Blob
//! values are content-addressed once. `instruction_state` is merely a fold
//! cache: [`InstructionService::rebuild`] can discard and reproduce it from
//! the ordered Session log plus the blob table.

use crate::InstructionDeltaError;
use grapt_store::{AggregateId, AppendInput, Appended, EventStore, WriteError};
use grapt_types::{
    EventId, EventSeq, EventVersion, ForkBoundaryError, ForkLineageError, InstructionEpoch,
    InstructionKey, SessionId,
};
use serde_json::Value;
use std::collections::BTreeMap;

mod history;
mod projection;
mod request_history;
mod storage;

use history::{
    decode_observations, decode_required, fold_history, resolve_delta, resolve_map, validate_cache,
    write_folded_state, CacheStatus, Observed,
};
pub use projection::InstructionProjector;
use storage::{decode_entry, ensure_tables, insert_blob, instruction_event_data, load_entry};

#[derive(Debug, Clone, PartialEq)]
pub enum InstructionValue {
    Text(String),
    Structured(Value),
}

impl InstructionValue {
    fn stored(&self) -> String {
        match self {
            Self::Text(value) => value.clone(),
            Self::Structured(value) => grapt_event::canonical_string(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceObservation {
    /// Retain the first available value for this Session, including across
    /// instruction-epoch changes. Used for a model's immutable tool catalog.
    AvailableOnce {
        key: InstructionKey,
        value: InstructionValue,
    },
    Available {
        key: InstructionKey,
        value: InstructionValue,
    },
    Unavailable {
        key: InstructionKey,
    },
    Removed {
        key: InstructionKey,
    },
}

impl SourceObservation {
    fn key(&self) -> &InstructionKey {
        match self {
            Self::Available { key, .. }
            | Self::AvailableOnce { key, .. }
            | Self::Unavailable { key }
            | Self::Removed { key } => key,
        }
    }
}

#[derive(Debug)]
pub struct RefreshInput {
    pub event: EventId,
    pub session: SessionId,
    pub created_ms: i64,
    /// Sources that must be observed before the first complete delta commits.
    pub required: Vec<InstructionKey>,
    /// Missing keys mean "not sampled". `Unavailable` is explicit and retains
    /// the prior value after initialization.
    pub observations: Vec<SourceObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    Updated { seq: EventSeq, changed: usize },
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionBoundary {
    CompactionEnded,
    SessionMoved,
    RevertCommitted,
}

impl InstructionBoundary {
    pub fn kind(self) -> &'static str {
        match self {
            Self::CompactionEnded => "session.compaction.ended",
            Self::SessionMoved => "session.moved",
            Self::RevertCommitted => "session.revert.committed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInstruction {
    pub key: InstructionKey,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstructionChange {
    Value(ResolvedInstruction),
    Removed { key: InstructionKey },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionUpdate {
    pub seq: EventSeq,
    pub changes: Vec<InstructionChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionHistory {
    pub epoch: InstructionEpoch,
    pub through: EventSeq,
    pub initial: Vec<ResolvedInstruction>,
    pub updates: Vec<InstructionUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryOutcome {
    Applied,
    Unchanged,
}

#[derive(Debug, thiserror::Error)]
pub enum InstructionError {
    #[error("instruction entry exceeds {bound}")]
    EntryBound { bound: &'static str },
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("event store: {0}")]
    Store(#[from] WriteError),
    #[error("event decode: {0}")]
    Decode(#[from] grapt_store::DecodeError),
    #[error("instruction state is corrupt: {reason}")]
    Corruption { reason: String },
    #[error("initial instruction discovery is incomplete for {keys:?}")]
    InitialIncomplete { keys: Vec<String> },
    #[error("instruction observation repeats source {key}")]
    DuplicateObservation { key: String },
    #[error("required instruction source repeats {key}")]
    DuplicateRequired { key: String },
    #[error("instruction-relevant event {kind}.{version} is unsupported")]
    UnsupportedVersion { kind: String, version: u32 },
    #[error("instruction entry time {updated_ms} precedes its prior time {prior_ms}")]
    TimeRegression { prior_ms: i64, updated_ms: i64 },
    #[error("instruction delta: {0}")]
    Delta(#[from] InstructionDeltaError),
    /// A durable `session.forked` fact did not satisfy the promoted boundary
    /// law. Typed rather than folded into `Corruption` so the exact refusal
    /// survives the seam.
    #[error("instruction fork boundary: {0}")]
    ForkBoundary(#[from] ForkBoundaryError),
    /// The fork chain itself was refused while walking it.
    #[error("instruction fork lineage: {0}")]
    ForkLineage(#[from] ForkLineageError),
}

#[derive(Debug, Default)]
pub struct InstructionService;

impl InstructionService {
    pub fn ensure_tables(store: &mut EventStore) -> Result<(), InstructionError> {
        if storage::schema_is_current(store.connection())? {
            return Ok(());
        }
        let tx = store.governed_transaction()?;
        ensure_tables(&tx)?;
        tx.commit()?.discard();
        Ok(())
    }

    /// [`Self::ensure_tables`] inside a transaction the caller holds, so a
    /// composition can install several owners' schemas in one commit.
    pub fn ensure_tables_within(tx: &rusqlite::Transaction<'_>) -> Result<(), InstructionError> {
        ensure_tables(tx)
    }

    pub fn refresh(
        store: &mut EventStore,
        input: RefreshInput,
    ) -> Result<RefreshOutcome, InstructionError> {
        let held = store.held_writers();
        let aggregate = AggregateId::Session(input.session.clone());
        let tx = store.governed_transaction()?;
        ensure_tables(&tx)?;
        let truth = fold_history(&tx, &input.session)?;
        let cache = validate_cache(
            &tx,
            &input.session,
            truth.as_ref().map(|truth| &truth.state),
        )?;
        if cache != CacheStatus::Present {
            write_folded_state(
                &tx,
                &input.session,
                truth.as_ref().map(|truth| &truth.state),
            )?;
        }
        let required = decode_required(input.required)?;
        let observations = decode_observations(input.observations)?;

        if truth.is_none() {
            let incomplete = required
                .iter()
                .filter(|key| {
                    observations
                        .get(*key)
                        .is_none_or(|observation| matches!(observation, Observed::Unavailable))
                })
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if !incomplete.is_empty() {
                return Err(InstructionError::InitialIncomplete { keys: incomplete });
            }
        }

        let mut next = truth
            .as_ref()
            .map(|truth| truth.state.current.clone())
            .unwrap_or_default();
        let mut changed = BTreeMap::new();
        for (key, observation) in observations {
            match observation {
                Observed::Available { hash, value, once } => {
                    if next.get(&key) == Some(&hash) || (once && next.contains_key(&key)) {
                        continue;
                    }
                    insert_blob(&tx, hash, &value)?;
                    next.insert(key.clone(), hash);
                    changed.insert(key, Some(hash));
                }
                Observed::Unavailable => {}
                Observed::Removed => {
                    if next.remove(&key).is_some() {
                        changed.insert(key, None);
                    }
                }
            }
        }

        if truth.is_some() && changed.is_empty() {
            tx.commit()?.discard();
            return Ok(RefreshOutcome::Unchanged);
        }

        let data = instruction_event_data(&input.session, &changed);
        let version = EventVersion::new(2).ok_or_else(|| InstructionError::UnsupportedVersion {
            kind: "session.instructions.updated".into(),
            version: 2,
        })?;
        let mut projector = InstructionProjector;
        let appended = EventStore::append_within(
            &tx,
            AppendInput {
                id: &input.event,
                aggregate: aggregate.clone(),
                writer: held.token(&aggregate)?,
                kind_name: "session.instructions.updated",
                version,
                created_ms: input.created_ms,
                data: &data,
            },
            &mut [&mut projector],
        )?;
        let seq = match appended {
            Appended::Committed { seq } => seq,
            Appended::Idempotent { seq } => {
                return Err(InstructionError::Corruption {
                    reason: format!(
                        "instruction event retry at sequence {seq} is outside the active durable state"
                    ),
                });
            }
        };
        tx.commit()?.discard();
        Ok(RefreshOutcome::Updated {
            seq: EventSeq::new(seq),
            changed: changed.len(),
        })
    }

    pub fn history(
        store: &EventStore,
        session: &SessionId,
    ) -> Result<Option<InstructionHistory>, InstructionError> {
        let truth = fold_history(store.connection(), session)?;
        validate_cache(
            store.connection(),
            session,
            truth.as_ref().map(|truth| &truth.state),
        )?;
        let Some(truth) = truth else {
            return Ok(None);
        };
        Ok(Some(InstructionHistory {
            epoch: truth.state.epoch,
            through: truth.state.through,
            initial: resolve_map(store.connection(), &truth.state.initial)?,
            updates: truth
                .updates
                .iter()
                .map(|update| {
                    Ok(InstructionUpdate {
                        seq: update.seq,
                        changes: resolve_delta(store.connection(), &update.delta)?,
                    })
                })
                .collect::<Result<_, InstructionError>>()?,
        }))
    }

    /// Current applicable values, validated against the same journal and cache
    /// as chronological instruction history.
    pub fn current(
        store: &EventStore,
        session: &SessionId,
    ) -> Result<Option<Vec<ResolvedInstruction>>, InstructionError> {
        let truth = fold_history(store.connection(), session)?;
        validate_cache(
            store.connection(),
            session,
            truth.as_ref().map(|truth| &truth.state),
        )?;
        truth
            .map(|truth| resolve_map(store.connection(), &truth.state.current))
            .transpose()
    }

    pub fn rebuild(
        store: &mut EventStore,
        session: &SessionId,
    ) -> Result<Option<InstructionHistory>, InstructionError> {
        let tx = store.governed_transaction()?;
        ensure_tables(&tx)?;
        let truth = fold_history(&tx, session)?;
        write_folded_state(&tx, session, truth.as_ref().map(|truth| &truth.state))?;
        tx.commit()?.discard();
        Self::history(store, session)
    }

    pub fn put_entry(
        store: &mut EventStore,
        session: &SessionId,
        key: &InstructionKey,
        value: &str,
        updated_ms: i64,
    ) -> Result<EntryOutcome, InstructionError> {
        if key.as_str().len() > 256 {
            return Err(InstructionError::EntryBound {
                bound: "256 key bytes",
            });
        }
        if value.len() > 65_536 {
            return Err(InstructionError::EntryBound {
                bound: "65,536 value bytes",
            });
        }
        let tx = store.governed_transaction()?;
        ensure_tables(&tx)?;
        storage::entry_bounds(&tx, session, Some((key, value.len())))?;
        let existing = load_entry(&tx, session, key)?;
        if existing
            .as_ref()
            .is_some_and(|entry| !entry.removed && entry.value.as_deref() == Some(value))
        {
            tx.commit()?.discard();
            return Ok(EntryOutcome::Unchanged);
        }
        if let Some(entry) = &existing {
            if updated_ms < entry.updated_ms {
                return Err(InstructionError::TimeRegression {
                    prior_ms: entry.updated_ms,
                    updated_ms,
                });
            }
        }
        let created = existing.map_or(updated_ms, |entry| entry.created_ms);
        if updated_ms < created {
            return Err(InstructionError::TimeRegression {
                prior_ms: created,
                updated_ms,
            });
        }
        tx.execute(
            "INSERT INTO instruction_entry
                (session_id, key, value, removed, time_created, time_updated)
             VALUES (?1, ?2, ?3, 0, ?4, ?5)
             ON CONFLICT(session_id, key) DO UPDATE SET
                value = excluded.value, removed = 0, time_updated = excluded.time_updated",
            rusqlite::params![session.as_str(), key.as_str(), value, created, updated_ms],
        )?;
        tx.commit()?.discard();
        Ok(EntryOutcome::Applied)
    }

    pub fn remove_entry(
        store: &mut EventStore,
        session: &SessionId,
        key: &InstructionKey,
        updated_ms: i64,
    ) -> Result<EntryOutcome, InstructionError> {
        let tx = store.governed_transaction()?;
        ensure_tables(&tx)?;
        storage::entry_bounds(&tx, session, None)?;
        let Some(existing) = load_entry(&tx, session, key)? else {
            tx.commit()?.discard();
            return Ok(EntryOutcome::Unchanged);
        };
        if existing.removed {
            tx.commit()?.discard();
            return Ok(EntryOutcome::Unchanged);
        }
        if updated_ms < existing.updated_ms {
            return Err(InstructionError::TimeRegression {
                prior_ms: existing.updated_ms,
                updated_ms,
            });
        }
        tx.execute(
            "UPDATE instruction_entry
             SET value = NULL, removed = 1, time_updated = ?3
             WHERE session_id = ?1 AND key = ?2",
            rusqlite::params![session.as_str(), key.as_str(), updated_ms],
        )?;
        tx.commit()?.discard();
        Ok(EntryOutcome::Applied)
    }

    pub fn entries(
        store: &EventStore,
        session: &SessionId,
    ) -> Result<Vec<ResolvedInstruction>, InstructionError> {
        storage::entry_bounds(store.connection(), session, None)?;
        let mut stmt = store.connection().prepare(
            "SELECT key, value, removed, time_created, time_updated
             FROM instruction_entry WHERE session_id = ?1 ORDER BY key",
        )?;
        let mut rows = stmt.query([session.as_str()])?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next()? {
            let key_raw: String = row.get(0)?;
            let entry = decode_entry(row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)?;
            if entry.removed {
                continue;
            }
            entries.push(ResolvedInstruction {
                key: InstructionKey::new(key_raw).ok_or_else(|| InstructionError::Corruption {
                    reason: "instruction entry has an empty key".into(),
                })?,
                value: entry.value.ok_or_else(|| InstructionError::Corruption {
                    reason: "live instruction entry has no value".into(),
                })?,
            });
        }
        Ok(entries)
    }
}
