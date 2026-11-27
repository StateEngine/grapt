//! Read pending user inputs through the owning projection and exact admission event.
//! A pending compaction is not an input; [`Inbox::pending_compaction`] reads it.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInput {
    pub input: MessageId,
    pub kind: PromotedInputKind,
    pub text: String,
    pub description: Option<String>,
    pub metadata: Option<Value>,
    pub delivery: DeliveryMode,
    pub seq: u64,
    pub created_ms: i64,
}

impl Inbox {
    /// A complete bounded inventory; exceeding a bound refuses rather than truncates.
    pub fn pending_inputs(&self, session: &SessionId) -> Result<Vec<PendingInput>, InboxError> {
        Self::pending_inputs_within(self.store.connection(), session)
    }

    /// Read the same bounded inventory while an existing owner holds its
    /// transaction, for an atomic native followup claim.
    pub fn pending_inputs_within(
        conn: &rusqlite::Connection,
        session: &SessionId,
    ) -> Result<Vec<PendingInput>, InboxError> {
        Self::pending_inputs_bounded_within(conn, session, 262144, 1048576, 4 * 1024 * 1024)
    }

    /// Promotion uses admission-sized reads, not the public listing's display
    /// limit, so an admitted large prompt reaches typed context-capacity handling.
    /// Keep the same canonical decoder and complete projection corroboration.
    pub(super) fn pending_inputs_for_promotion_within(
        conn: &rusqlite::Connection,
        session: &SessionId,
    ) -> Result<Vec<PendingInput>, InboxError> {
        Self::pending_inputs_bounded_within(
            conn,
            session,
            8 * 1024 * 1024,
            64 * 1024 * 1024,
            32 * 1024 * 1024,
        )
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn pending_inputs_bounded_within(
        conn: &rusqlite::Connection,
        session: &SessionId,
        max_text_bytes: i64,
        max_event_bytes: i64,
        max_response_bytes: usize,
    ) -> Result<Vec<PendingInput>, InboxError> {
        let mut query = conn.prepare(
            "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 512 THEN id END,
                    CASE WHEN length(CAST(text AS BLOB)) <= ?2 THEN text END,
                    CASE WHEN length(CAST(delivery AS BLOB)) <= 16 THEN delivery END,
                    admitted_seq,
                    CASE WHEN length(CAST(input_type AS BLOB)) <= 16 THEN input_type END,
                    CASE WHEN description IS NULL OR length(CAST(description AS BLOB)) <= 4096 THEN description END,
                    CASE WHEN metadata_json IS NULL OR length(CAST(metadata_json AS BLOB)) <= 65536 THEN metadata_json END
             FROM session_pending WHERE session_id = ?1 AND input_type != 'compaction'
             ORDER BY admitted_seq, id LIMIT 129",
        )?;
        let mut rows = query.query(rusqlite::params![session.as_str(), max_text_bytes])?;
        let mut result = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = rows.next()? {
            if result.len() == 128 {
                return Err(InboxError::PendingBound("128 rows"));
            }
            let raw: String = row.get(0)?;
            let input: MessageId = raw
                .parse()
                .map_err(|_| InboxError::PendingBound("invalid input id"))?;
            let text: String = row.get(1)?;
            let raw_delivery: String = row.get(2)?;
            let delivery = delivery_from_str(&raw_delivery)
                .ok_or(InboxError::PendingBound("invalid delivery"))?;
            let stored_seq: i64 = row.get(3)?;
            let raw_kind: String = row.get(4)?;
            let kind = PromotedInputKind::from_str(&raw_kind)
                .ok_or(InboxError::PendingBound("invalid input type"))?;
            let description: Option<String> = row.get(5)?;
            let raw_metadata: Option<String> = row.get(6)?;
            let metadata = raw_metadata
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .map_err(|_| InboxError::PendingBound("invalid metadata"))?;
            let seq = u64::try_from(stored_seq)
                .map_err(|_| InboxError::PendingBound("negative admission sequence"))?;
            bytes += text.len() + input.as_str().len();
            if bytes > max_response_bytes {
                return Err(InboxError::PendingBound("pending response byte bound"));
            }
            // Sequence is the primary key: decode exactly the owner's event, with
            // a bounded allocation before inspecting its untrusted JSON.
            let (created_ms, raw_event): (i64, String) = conn.query_row(
                "SELECT created, CASE WHEN length(CAST(data AS BLOB)) <= ?3 THEN data END
                 FROM event WHERE seq = ?1 AND aggregate_family = 'session'
                   AND aggregate_id = ?2 AND type = 'session.input.admitted.1'",
                rusqlite::params![stored_seq, session.as_str(), max_event_bytes],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if created_ms < 0 {
                return Err(InboxError::PendingBound("negative admission time"));
            }
            let data: Value =
                serde_json::from_str(&raw_event).map_err(|error| InboxError::Corruption {
                    input: input.clone(),
                    reason: error.to_string(),
                })?;
            let event =
                InboxEvent::decode("session.input.admitted", 1, &data).map_err(|error| {
                    InboxError::Corruption {
                        input: input.clone(),
                        reason: error.to_string(),
                    }
                })?;
            let expected = match kind {
                PromotedInputKind::User => InboxEvent::Admitted {
                    session: session.clone(),
                    input: input.clone(),
                    delivery,
                },
                PromotedInputKind::Synthetic => InboxEvent::SyntheticAdmitted {
                    session: session.clone(),
                    input: input.clone(),
                    delivery,
                },
            };
            if event != Some(expected)
                || data.pointer("/input/data/text").and_then(Value::as_str) != Some(text.as_str())
                || data
                    .pointer("/input/data/description")
                    .and_then(Value::as_str)
                    != description.as_deref()
                || data.pointer("/input/data/metadata") != metadata.as_ref()
            {
                return Err(InboxError::Corruption {
                    input,
                    reason: "pending projection disagrees with its admission".to_owned(),
                });
            }
            result.push(PendingInput {
                input,
                kind,
                text,
                description,
                metadata,
                delivery,
                seq,
                created_ms,
            });
        }
        Ok(result)
    }
}
