//! Bounded promoted-history reads; admission and decoding retain one owner.
use super::*;
use rusqlite::OptionalExtension;

impl Inbox {
    /// Resolve the input preceding an assistant opening without materializing
    /// discarded transcript text. The strict boundary preserves replay order.
    pub fn promoted_input_before(
        &self,
        session: &SessionId,
        before: grapt_types::EventSeq,
    ) -> Result<Option<MessageId>, InboxError> {
        let visible = grapt_store::history_visibility::HistoryVisibility::read(
            self.store.connection(),
            session,
        )?
        .sql("seq");
        self.promoted_input_before_filtered(session, before, &visible)
    }

    /// Resolve an original execution/input association, including inputs later
    /// hidden by rewind. This returns an identity only; visible transcript reads
    /// still own which text may enter a current provider request.
    pub fn original_promoted_input_before(
        &self,
        session: &SessionId,
        before: grapt_types::EventSeq,
    ) -> Result<Option<MessageId>, InboxError> {
        self.promoted_input_before_filtered(session, before, "1")
    }

    fn promoted_input_before_filtered(
        &self,
        session: &SessionId,
        before: grapt_types::EventSeq,
        visible: &str,
    ) -> Result<Option<MessageId>, InboxError> {
        let before = i64::try_from(before.value()).map_err(|_| InboxError::CorruptPendingId {
            raw: session.as_str().to_owned(),
            reason: "input association boundary exceeds SQLite range".to_owned(),
        })?;
        let row: Option<(String, i64)> = self
            .store
            .connection()
            .query_row(
                &format!(
                    "SELECT id, seq FROM session_message
             WHERE session_id = ?1 AND {visible} AND type IN ('user', 'synthetic') AND seq < ?2
             ORDER BY (seq < 0) DESC, seq DESC LIMIT 1"
                ),
                rusqlite::params![session.as_str(), before],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((raw, seq)) = row else {
            return Ok(None);
        };
        let input = raw
            .parse::<MessageId>()
            .map_err(|_| InboxError::CorruptPendingId {
                raw,
                reason: "projected user message identity is invalid".to_owned(),
            })?;
        if seq < 0 {
            return Err(InboxError::Corruption {
                input,
                reason: format!("projected message sequence {seq} is negative"),
            });
        }
        Ok(Some(input))
    }

    /// Read promoted inputs at or after `from`, inclusively. The selection
    /// policy belongs to the compaction owner; `generation` must still be this
    /// Session's latest committed compaction. A stale or foreign generation
    /// cannot authorize omission of earlier inputs.
    pub fn promoted_messages_from(
        &self,
        session: &SessionId,
        from: grapt_types::EventSeq,
        generation: &EventId,
        limit: usize,
    ) -> Result<Vec<PromotedMessage>, InboxError> {
        let invalid = || InboxError::HistoryBoundary {
            session: session.clone(),
            generation: generation.clone(),
        };
        let latest =
            grapt_store::compaction::latest_visible_descriptor(self.store.connection(), session)?;
        let Some((event, seq, _descriptor)) = latest else {
            return Err(invalid());
        };
        let from = i64::try_from(from.value()).map_err(|_| invalid())?;
        if &event != generation || seq.value() < from as u64 {
            return Err(invalid());
        }
        self.promoted_page(session, from, None, limit)
    }

    /// Reads a bounded page of promoted user messages in durable order.
    ///
    /// The caller supplies its public page bound. This query probes one row
    /// beyond it so a longer transcript refuses instead of appearing complete.
    pub fn promoted_messages(
        &self,
        session: &SessionId,
        limit: usize,
    ) -> Result<Vec<PromotedMessage>, InboxError> {
        self.promoted_page(session, i64::MIN, None, limit)
    }

    /// Read promoted inputs in the half-open retained window `[from, until)`.
    pub fn promoted_messages_window_from(
        &self,
        session: &SessionId,
        from: grapt_types::EventSeq,
        generation: &EventId,
        until: grapt_types::EventSeq,
        limit: usize,
    ) -> Result<Vec<PromotedMessage>, InboxError> {
        let invalid = || InboxError::HistoryBoundary {
            session: session.clone(),
            generation: generation.clone(),
        };
        let latest =
            grapt_store::compaction::latest_visible_descriptor(self.store.connection(), session)?;
        let Some((event, seq, _descriptor)) = latest else {
            return Err(invalid());
        };
        let from = i64::try_from(from.value()).map_err(|_| invalid())?;
        let until = i64::try_from(until.value()).map_err(|_| invalid())?;
        if &event != generation || seq.value() < from as u64 || until <= from {
            return Err(invalid());
        }
        self.promoted_page(session, from, Some(until), limit)
    }

    /// Read promoted inputs in `[from, until)` for a summary window that
    /// starts past the retained cut. The window is the caller's own bounded
    /// choice, so no retained descriptor is consulted.
    pub fn promoted_messages_floor(
        &self,
        session: &SessionId,
        from: grapt_types::EventSeq,
        until: Option<grapt_types::EventSeq>,
        limit: usize,
    ) -> Result<Vec<PromotedMessage>, InboxError> {
        let range = |seq: grapt_types::EventSeq| {
            i64::try_from(seq.value()).map_err(|_| InboxError::CorruptPendingId {
                raw: session.as_str().to_owned(),
                reason: "history window exceeds SQLite range".to_owned(),
            })
        };
        self.promoted_page(session, range(from)?, until.map(range).transpose()?, limit)
    }

    /// Read promoted inputs before `until` when no retained cut exists.
    pub fn promoted_messages_window(
        &self,
        session: &SessionId,
        until: grapt_types::EventSeq,
        limit: usize,
    ) -> Result<Vec<PromotedMessage>, InboxError> {
        let until = i64::try_from(until.value()).map_err(|_| InboxError::CorruptPendingId {
            raw: session.as_str().to_owned(),
            reason: "history window exceeds SQLite range".to_owned(),
        })?;
        self.promoted_page(session, i64::MIN, Some(until), limit)
    }

    /// One visible promoted input at its durable sequence, independently of Session length.
    pub fn promoted_message_at(
        &self,
        session: &SessionId,
        sequence: grapt_types::EventSeq,
    ) -> Result<Option<PromotedMessage>, InboxError> {
        let from = i64::try_from(sequence.value()).map_err(|_| InboxError::CorruptPendingId {
            raw: session.as_str().to_owned(),
            reason: "history sequence exceeds SQLite range".into(),
        })?;
        let until = from
            .checked_add(1)
            .ok_or_else(|| InboxError::CorruptPendingId {
                raw: session.as_str().to_owned(),
                reason: "history sequence has no successor".into(),
            })?;
        Ok(self.promoted_page(session, from, Some(until), 1)?.pop())
    }

    fn promoted_page(
        &self,
        session: &SessionId,
        from: i64,
        until: Option<i64>,
        limit: usize,
    ) -> Result<Vec<PromotedMessage>, InboxError> {
        let probe = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
        let visible = grapt_store::history_visibility::HistoryVisibility::read(
            self.store.connection(),
            session,
        )?
        .sql("seq");
        let mut statement = self.store.connection().prepare(&format!(
            "SELECT id, type, text, description, metadata_json, seq FROM session_message
             WHERE session_id = ?1 AND {visible} AND type IN ('user', 'synthetic') AND (seq >= ?3 OR seq < 0)
             AND (?4 IS NULL OR seq < ?4)
             ORDER BY seq ASC LIMIT ?2",
        ))?;
        let mapped = statement.query_map(
            rusqlite::params![session.as_str(), probe, from, until],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )?;
        let mut rows = Vec::new();
        for row in mapped {
            let (raw, raw_kind, text, description, raw_metadata, seq) = row?;
            let input = raw
                .parse::<MessageId>()
                .map_err(|_| InboxError::CorruptPendingId {
                    raw: raw.clone(),
                    reason: "projected user message identity is invalid".to_owned(),
                })?;
            let seq = u64::try_from(seq).map_err(|_| InboxError::Corruption {
                input: input.clone(),
                reason: format!("projected message sequence {seq} is negative"),
            })?;
            let kind =
                PromotedInputKind::from_str(&raw_kind).ok_or_else(|| InboxError::Corruption {
                    input: input.clone(),
                    reason: format!("projected message type {raw_kind:?} is invalid"),
                })?;
            let metadata = raw_metadata
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .map_err(|error| InboxError::Corruption {
                    input: input.clone(),
                    reason: format!("projected metadata is corrupt: {error}"),
                })?;
            rows.push(PromotedMessage {
                input,
                kind,
                text,
                description,
                metadata,
                seq,
            });
        }
        if rows.len() > limit {
            return Err(InboxError::PromotedPageTooLong {
                session: session.clone(),
                limit,
            });
        }
        Ok(rows)
    }
}
