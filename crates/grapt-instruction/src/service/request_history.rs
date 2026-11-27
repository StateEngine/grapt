//! Read-only bounded model context; no event or cache write is introduced.
use super::*;
use grapt_types::InstructionHash;
const MAX_RESOLVED_BYTES: usize = 4 * 1024 * 1024;
impl InstructionService {
    /// Immutable epoch baseline and ordered updates for a provider request.
    /// Refuses oversized history rather than silently omitting instructions.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn request_history(
        store: &EventStore,
        session: &SessionId,
    ) -> Result<Option<InstructionHistory>, InstructionError> {
        let conn = store.connection();
        let fold = history::fold_request_history(conn, session)?;
        // Bound persisted cache JSON and its referenced blobs before the normal
        // independent journal/cache comparison allocates either.
        let sizes: Option<(i64, i64)> = conn
            .query_row(
                "SELECT length(CAST(initial_values AS BLOB)), length(CAST(current_values AS BLOB))
             FROM instruction_state WHERE session_id = ?1",
                [session.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((initial, current)) = sizes {
            if !(0..=131_072).contains(&initial) || !(0..=131_072).contains(&current) {
                return Err(bound());
            }
            let mut statement = conn.prepare(
                "SELECT hash FROM (
                   SELECT j.value AS hash FROM instruction_state s, json_each(s.initial_values) j WHERE s.session_id = ?1
                   UNION
                   SELECT j.value AS hash FROM instruction_state s, json_each(s.current_values) j WHERE s.session_id = ?1
                 ) LIMIT 513",
            )?;
            let mut rows = statement.query([session.as_str()])?;
            let mut count = 0usize;
            let mut bytes = 0usize;
            while let Some(row) = rows.next()? {
                count += 1;
                if count > 512 {
                    return Err(bound());
                }
                let hash: String = row.get(0)?;
                let hash = hash.parse::<InstructionHash>().map_err(|error| {
                    InstructionError::Corruption {
                        reason: error.to_string(),
                    }
                })?;
                bytes = bytes
                    .checked_add(history::request_blob_size(conn, hash)?)
                    .ok_or_else(bound)?;
                if bytes > MAX_RESOLVED_BYTES {
                    return Err(bound());
                }
            }
        }
        check_cache(conn, session, &fold)?;
        let skipped = fold.skipped;
        let Some(mut truth) = fold.history else {
            return Ok(InstructionHistory::unreadable(skipped));
        };
        // A fork renumbers its copied conversation. Ancestor positions cannot
        // be interleaved with those rows, even when the child's first update
        // happens to sort after its parent's. Validate the durable cache above,
        // then carry inherited values as the request's baseline only.
        // The first own update already supersedes these inherited values
        // before the child's first provider call. Keep their immutable prefix
        // rather than moving an earlier child's identity into a nested fork.
        let replaced = truth
            .updates
            .iter()
            .find(|update| update.owner == *session)
            .map_or_else(std::collections::BTreeSet::new, |update| {
                update.delta.keys().cloned().collect()
            });
        truth.updates.retain(|update| {
            if update.owner == *session {
                true
            } else {
                let delta = update
                    .delta
                    .iter()
                    .filter(|(key, _)| !replaced.contains(*key))
                    .map(|(key, hash)| (key.clone(), *hash))
                    .collect();
                history::apply_delta(&mut truth.state.initial, &delta);
                false
            }
        });
        let mut bytes = 0usize;
        let mut resolve = |key: &InstructionKey,
                           hash: grapt_types::InstructionHash|
         -> Result<ResolvedInstruction, InstructionError> {
            let size = history::request_blob_size(conn, hash)?;
            bytes = bytes
                .checked_add(key.as_str().len())
                .and_then(|bytes| bytes.checked_add(size))
                .ok_or_else(bound)?;
            if bytes > MAX_RESOLVED_BYTES {
                return Err(InstructionError::EntryBound {
                    bound: "instruction epoch request bytes",
                });
            }
            Ok(ResolvedInstruction {
                key: key.clone(),
                value: storage::load_blob(conn, hash)?,
            })
        };
        let initial = truth
            .state
            .initial
            .iter()
            .map(|(key, hash)| resolve(key, *hash))
            .collect::<Result<_, _>>()?;
        let updates = truth
            .updates
            .iter()
            .map(|update| {
                let changes = update
                    .delta
                    .iter()
                    .map(|(key, hash)| match hash {
                        Some(hash) => resolve(key, *hash).map(InstructionChange::Value),
                        None => Ok(InstructionChange::Removed { key: key.clone() }),
                    })
                    .collect::<Result<_, InstructionError>>()?;
                Ok(InstructionUpdate {
                    seq: update.seq,
                    changes,
                })
            })
            .collect::<Result<_, InstructionError>>()?;
        Ok(Some(InstructionHistory {
            epoch: truth.state.epoch,
            through: truth.state.through,
            initial,
            updates,
            skipped,
        }))
    }
}
fn bound() -> InstructionError {
    InstructionError::EntryBound {
        bound: "instruction request history",
    }
}
use rusqlite::OptionalExtension;
