//! Locating and reconciling an existing admission for an input id, under
//! the admitting transaction. Moved unchanged out of `admission.rs` under
//! the workspace god-file ceiling.
use super::*;

type PendingAdmissionRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);
type PromotedAdmissionRow = (String, String, String, Option<String>, Option<String>);

pub(super) struct ExistingAdmission {
    pub(super) session: String,
    pub(super) input_type: String,
    pub(super) delivery: String,
    pub(super) text: String,
    pub(super) description: Option<String>,
    pub(super) metadata: Option<Value>,
}

impl Inbox {
    /// Recover the immutable mode of a stable retry before applying a new default.
    /// The ordinary admission still validates the complete identity and content.
    pub fn admitted_delivery(
        &mut self,
        input: &MessageId,
    ) -> Result<Option<DeliveryMode>, InboxError> {
        let tx = self.store.governed_transaction()?;
        let delivery = Self::find_existing(&tx, input)?
            .map(|existing| {
                delivery_from_str(&existing.delivery).ok_or_else(|| InboxError::Corruption {
                    input: input.clone(),
                    reason: "stored admission delivery is outside the closed vocabulary".into(),
                })
            })
            .transpose()?;
        tx.commit()?.discard();
        Ok(delivery)
    }

    /// Locate an existing admission for this input UNDER the caller's
    /// transaction. Pending row wins; otherwise the durable admitted event,
    /// strictly decoded and corroborated by exactly one projected user
    /// message (review B). Absent everywhere → None (fresh admission).
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn find_existing(
        tx: &Transaction<'_>,
        input: &MessageId,
    ) -> Result<Option<ExistingAdmission>, InboxError> {
        let pending: Option<PendingAdmissionRow> = tx
            .query_row(
                "SELECT session_id, input_type, delivery, text, description, metadata_json
                 FROM session_pending WHERE id = ?1",
                [input.as_str()],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .map(Some)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        if let Some((session, input_type, delivery, text, description, metadata)) = pending {
            let metadata = metadata
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .map_err(|error| InboxError::Corruption {
                    input: input.clone(),
                    reason: format!("pending metadata is corrupt: {error}"),
                })?;
            return Ok(Some(ExistingAdmission {
                session,
                input_type,
                delivery,
                text,
                description,
                metadata,
            }));
        }

        let corrupt = |reason: String| InboxError::Corruption {
            input: input.clone(),
            reason,
        };

        // A compaction id stays reserved after its request settles and its
        // pending row is gone: its own admission event owns the identity, in
        // every Session and against every input type.
        // Bounded before anything is allocated: at most two rows, and only
        // the owning Session's bounded identity, never the payload. A second
        // row is a duplicate; a malformed, oversized or missing identity is
        // NULL. Both are corruption.
        let compactions: Vec<Option<String>> = tx
            .prepare(
                "SELECT CASE WHEN json_valid(data) = 1
                          AND json_type(data, '$.sessionID') = 'text'
                          AND length(CAST(json_extract(data, '$.sessionID') AS BLOB)) <= 512
                        THEN json_extract(data, '$.sessionID') END
                 FROM event
                 WHERE type = 'session.compaction.admitted.1'
                   AND (json_valid(data) = 0 OR json_extract(data, '$.inputID') = ?1)
                 LIMIT 2",
            )?
            .query_map([input.as_str()], |r| r.get::<_, Option<String>>(0))?
            .collect::<Result<_, _>>()?;
        if let Some(session) = compactions.first() {
            if compactions.len() > 1 {
                return Err(corrupt(
                    "more than one compaction admission for one id".into(),
                ));
            }
            let session = session.as_ref().ok_or_else(|| {
                corrupt("compaction admission is malformed or its Session is oversized".into())
            })?;
            return Ok(Some(ExistingAdmission {
                session: session.clone(),
                input_type: "compaction".to_owned(),
                delivery: "queue".to_owned(),
                text: String::new(),
                description: None,
                metadata: None,
            }));
        }

        // Strictly exactly one admitted event, or none, or corruption.
        // json_valid guard: a corrupt admitted blob makes json_extract NULL,
        // which would silently fall through to FRESH admission — the exact
        // failure review B forbids. Corrupt admission-log rows surface as
        // typed corruption instead.
        let mut stmt = tx.prepare(
            "SELECT data FROM event
             WHERE type = 'session.input.admitted.1'
               AND (json_valid(data) = 0 OR json_extract(data, '$.inputID') = ?1)",
        )?;
        let raws: Vec<String> = stmt
            .query_map([input.as_str()], |r| r.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        if raws.is_empty() {
            return Ok(None);
        }
        if raws.len() > 1 {
            return Err(corrupt(format!(
                "{} admitted events for one input",
                raws.len()
            )));
        }
        let data: Value = serde_json::from_str(&raws[0])
            .map_err(|e| corrupt(format!("admitted event data is not valid JSON: {e}")))?;
        let session = data
            .get("sessionID")
            .and_then(Value::as_str)
            .ok_or_else(|| corrupt("admitted event has no sessionID".into()))?;
        let delivery = data
            .pointer("/input/delivery")
            .and_then(Value::as_str)
            .ok_or_else(|| corrupt("admitted event has no input.delivery".into()))?;
        if delivery_from_str(delivery).is_none() {
            return Err(corrupt(format!(
                "delivery {delivery:?} outside the closed vocabulary"
            )));
        }
        let text = data
            .pointer("/input/data/text")
            .and_then(Value::as_str)
            .ok_or_else(|| corrupt("admitted event has no input.data.text".into()))?;
        let input_type = data
            .pointer("/input/type")
            .and_then(Value::as_str)
            .ok_or_else(|| corrupt("admitted event has no input.type".into()))?;
        if PromotedInputKind::from_str(input_type).is_none() {
            return Err(corrupt(format!(
                "active input type {input_type:?} is invalid"
            )));
        }
        if input_type == "synthetic"
            && data
                .pointer("/input/rustAuthorized")
                .and_then(Value::as_bool)
                != Some(true)
        {
            return Err(corrupt(
                "legacy synthetic admission is not active input".into(),
            ));
        }
        let description = data
            .pointer("/input/data/description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let metadata = data.pointer("/input/data/metadata").cloned();

        // Corroborate the projected user message (post-promotion truth).
        let messages: Vec<PromotedAdmissionRow> = tx
            .prepare(
                "SELECT session_id, type, text, description, metadata_json
                 FROM session_message WHERE id = ?1 AND type IN ('user', 'synthetic')",
            )?
            .query_map([input.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<_, _>>()?;
        match messages.as_slice() {
            [] => {
                // A retired synthetic auto-resume input is a valid terminal
                // outcome: its `session_pending` slot was freed without
                // promoting a message, and exact-key retries reconcile cleanly.
                if input_type == "synthetic" {
                    use rusqlite::OptionalExtension as _;
                    let retired: Option<i64> = tx
                        .query_row(
                            "SELECT active FROM session_resume_input
                             WHERE session_id = ?1 AND input_id = ?2",
                            rusqlite::params![session, input.as_str()],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if retired == Some(0) {
                        return Ok(Some(ExistingAdmission {
                            session: session.to_owned(),
                            input_type: input_type.to_owned(),
                            delivery: delivery.to_owned(),
                            text: text.to_owned(),
                            description,
                            metadata,
                        }));
                    }
                }
                Err(corrupt(
                    "admitted event exists but neither pending row nor projected message".into(),
                ))
            }
            [(
                message_session,
                message_type,
                message_text,
                message_description,
                message_metadata,
            )] => {
                let message_metadata = message_metadata
                    .as_ref()
                    .map(|raw| serde_json::from_str::<Value>(raw))
                    .transpose()
                    .map_err(|error| corrupt(format!("projected metadata is corrupt: {error}")))?;
                if message_session != session
                    || message_type != input_type
                    || message_text != text
                    || message_description != &description
                    || message_metadata != metadata
                {
                    return Err(corrupt(
                        "projected message disagrees with the admitted event".into(),
                    ));
                }
                Ok(Some(ExistingAdmission {
                    session: session.to_owned(),
                    input_type: input_type.to_owned(),
                    delivery: delivery.to_owned(),
                    text: text.to_owned(),
                    description,
                    metadata,
                }))
            }
            _ => Err(corrupt(format!(
                "{} projected messages for one input",
                messages.len()
            ))),
        }
    }

    pub(super) fn reconcile_user(
        request: &AdmitRequest<'_>,
        existing: ExistingAdmission,
        wake: WakeDisposition,
        image_metadata: Option<&Value>,
    ) -> Result<AdmitOutcome, InboxError> {
        if existing.session != request.session.as_str() {
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs: "session",
            });
        }
        if existing.delivery != delivery_str(request.delivery) {
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs: "delivery",
            });
        }
        if existing.text != request.text {
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs: "text",
            });
        }
        if existing.input_type != "user"
            || existing.description.is_some()
            || existing.metadata.as_ref() != image_metadata
        {
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs: "type",
            });
        }
        Ok(AdmitOutcome::Reconciled { wake })
    }
}
