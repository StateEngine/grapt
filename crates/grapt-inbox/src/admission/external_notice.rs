//! Authenticated host notices share native wake admission and promotion fences.
use super::*;

impl Inbox {
    /// Sessions whose committed StateEngine identity requires continuity (a
    /// binding that is not incognito), at most `limit`, in Session order.
    pub fn continuity_bound_sessions(&self, limit: usize) -> Result<Vec<SessionId>, InboxError> {
        let mut statement = self.store.connection().prepare(
            "SELECT DISTINCT session_id FROM instruction_entry
             WHERE key='stateengine-identity' AND removed=0
               AND typeof(value)='text' AND json_valid(value)
               AND json_extract(value, '$.scope') IS NOT 'incognito'
             ORDER BY session_id LIMIT ?1",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut sessions = Vec::new();
        for raw in statement.query_map([limit], |row| row.get::<_, String>(0))? {
            if let Ok(session) = raw?.parse::<SessionId>() {
                sessions.push(session);
            }
        }
        Ok(sessions)
    }

    /// Find current persisted same-scope targets, including earlier processes.
    /// The caller fences their writers before selecting the replacement target.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn native_notice_superseded(
        &self,
        session: &SessionId,
    ) -> Result<Vec<SessionId>, InboxError> {
        // Invalid identities remain candidates for refusal, never disappear
        // through a JSON filter. Validate unique coordinate keys so SQLite's
        // first-key decoding cannot disagree with a consumer's last-key decoding.
        // Only bounded owning IDs cross into Rust; identity bodies stay in SQL.
        // Latest state is classified before limiting: absent (NULL), revoked or
        // legacy (0), selected (1), or malformed (-1). A hold is still selected.
        let mut statement = self.store.connection().prepare(
            "WITH bounded AS NOT MATERIALIZED (
                 SELECT session_id,
                        CASE WHEN typeof(value)='text'
                                  AND length(CAST(value AS BLOB))<=65536
                             THEN CASE WHEN json_valid(value) THEN value END END AS body
                 FROM instruction_entry
                 WHERE key='stateengine-identity' AND removed=0
             ), identities AS NOT MATERIALIZED (
                 SELECT session_id,
                        CASE WHEN typeof(session_id)='text'
                                  AND length(CAST(session_id AS BLOB)) BETWEEN 5 AND 512
                                  AND session_id GLOB 'ses_*'
                                  AND session_id NOT GLOB '*[^A-Za-z0-9_]*'
                                  AND instr(session_id, char(0))=0 AND body IS NOT NULL
                             THEN CASE WHEN json_type(body, '$.scope')='text'
                                       AND (SELECT count(*) FROM json_each(body)
                                            WHERE key='scope')=1
                                       AND (json_extract(body, '$.scope')='incognito'
                                            OR (json_type(body, '$.project_id')='text'
                                                AND json_type(body, '$.agent_id')='text'
                                                AND (SELECT count(*) FROM json_each(body)
                                                     WHERE key IN ('project_id','agent_id','scope'))=3
                                                AND NOT EXISTS (
                                                    SELECT 1 FROM json_each(body)
                                                    WHERE key IN ('project_id','agent_id','scope')
                                                      AND length(CAST(value AS BLOB)) NOT BETWEEN 1 AND 256)))
                                  THEN body END END AS identity
                 FROM bounded
             ), current AS (
                 SELECT identity FROM identities WHERE session_id=?1
             ), targets AS NOT MATERIALIZED (
                 SELECT session_id, identity,
                        (SELECT CASE WHEN typeof(data)='text'
                                          AND length(CAST(data AS BLOB))<=4096
                                     THEN CASE WHEN json_valid(data) THEN
                                          CASE WHEN json_extract(data, '$.sessionID')=session_id
                                                AND json_type(data, '$.held') IN ('true','false')
                                                AND json_type(data, '$.humanResume') IN ('true','false')
                                                AND NOT (json_extract(data, '$.held')=1
                                                         AND json_extract(data, '$.humanResume')=1)
                                                AND (SELECT count(*) FROM json_each(data))=5
                                                AND (SELECT count(DISTINCT key) FROM json_each(data)
                                                     WHERE key IN ('sessionID','held','humanResume','target','binding'))=5
                                                AND (json_type(data, '$.target')='null'
                                                     OR (json_type(data, '$.target')='text'
                                                         AND length(CAST(json_extract(data, '$.target') AS BLOB)) BETWEEN 5 AND 512
                                                         AND json_extract(data, '$.target') GLOB 'evt_*'
                                                         AND json_extract(data, '$.target') NOT GLOB '*[^A-Za-z0-9_]*'
                                                         AND instr(json_extract(data, '$.target'), char(0))=0))
                                                AND (json_type(data, '$.binding')='null'
                                                     OR (json_type(data, '$.binding')='text'
                                                         AND json_type(data, '$.target')='text'
                                                         AND length(CAST(json_extract(data, '$.binding') AS BLOB))=64
                                                         AND json_extract(data, '$.binding') NOT GLOB '*[^0-9A-Fa-f]*'
                                                         AND instr(json_extract(data, '$.binding'), char(0))=0))
                                               THEN CASE WHEN json_type(data, '$.binding')='null'
                                                         THEN 0 ELSE 1 END
                                               ELSE -1 END
                                          ELSE -1 END
                                     ELSE -1 END
                         FROM event WHERE aggregate_family='session'
                           AND aggregate_id=session_id AND type='session.native-mail-state.1'
                         ORDER BY seq DESC LIMIT 1) AS target_status
                 FROM identities
             )
             SELECT session_id=?1,
                    CASE WHEN length(CAST(session_id AS BLOB))<=512 THEN session_id END,
                    identity IS NOT NULL AND COALESCE(target_status,0)>=0
             FROM targets
             WHERE session_id=?1 OR identity IS NULL
                OR (json_extract(identity, '$.scope')<>'incognito'
                    AND target_status IS NOT NULL AND target_status<>0
                    AND (target_status=-1 OR (json_extract(identity, '$.project_id')=
                        (SELECT json_extract(identity, '$.project_id') FROM current)
                    AND json_extract(identity, '$.agent_id')=
                        (SELECT json_extract(identity, '$.agent_id') FROM current)
                    AND json_extract(identity, '$.scope')=
                        (SELECT json_extract(identity, '$.scope') FROM current))))
             ORDER BY session_id=?1 DESC
             LIMIT 66",
        )?;
        let mut rows = statement.query([session.as_str()])?;
        let current = rows.next()?.ok_or(InboxError::PendingBound(
            "external notice current identity is absent",
        ))?;
        if !current.get::<_, bool>(0)? {
            return Err(InboxError::PendingBound(
                "external notice current identity is absent",
            ));
        }
        if !current.get::<_, bool>(2)? {
            return Err(InboxError::PendingBound(
                "external notice current identity or target state is malformed or oversized",
            ));
        }
        let mut superseded = Vec::new();
        while let Some(row) = rows.next()? {
            if superseded.len() == 64 {
                return Err(InboxError::PendingBound(
                    "external notice superseded target count",
                ));
            }
            if !row.get::<_, bool>(2)? {
                return Err(InboxError::PendingBound(
                    "external notice binding identity or target state is malformed or oversized",
                ));
            }
            let raw: String = row.get(1)?;
            let owner = raw.parse().map_err(|_| {
                InboxError::PendingBound("external notice binding Session identity")
            })?;
            superseded.push(owner);
        }
        Ok(superseded)
    }

    /// Admit a notice for the exact current target without consuming remote mail.
    pub fn admit_external_notice(
        &mut self,
        target: &EventId,
        binding: &str,
        request: SyntheticAdmitRequest<'_>,
    ) -> Result<AdmitOutcome, InboxError> {
        if request
            .metadata
            .and_then(|m| m.get("binding"))
            .and_then(Value::as_str)
            != Some(binding)
            || request
                .metadata
                .and_then(|m| m.get("hostNotice"))
                .and_then(Value::as_bool)
                != Some(true)
            || request
                .metadata
                .and_then(|m| m.get("native_mail_wake"))
                .and_then(Value::as_u64)
                != Some(1)
            || !request
                .metadata
                .and_then(|m| m.get("messages"))
                .and_then(Value::as_array)
                .is_some_and(|messages| !messages.is_empty())
        {
            return Err(InboxError::PendingBound("external notice metadata"));
        }
        let held = self.store.held_writers();
        let tx = self.store.governed_transaction()?;
        let Some((current, fingerprint, paused)) =
            super::native_mail_state::notice_target(&tx, request.session)?
        else {
            return Err(InboxError::PendingBound(
                "external notice target is absent or revoked",
            ));
        };
        if current != *target || fingerprint != binding {
            return Err(InboxError::PendingBound(
                "external notice target is superseded",
            ));
        }
        if paused {
            return Err(InboxError::NativeWakeHeld);
        }
        super::native_mail::refuse_held_work(&tx, request.session)?;
        let original = super::acceptance::read_admission(&tx, request.session, request.input)?;
        if let Some((receipt, data)) = &original {
            // Payload equality alone would let an old input key migrate when
            // the same instruction fingerprint is selected in a new generation.
            if data.pointer("/input/nativeMailWake") != Some(&Value::Bool(true))
                || data
                    .pointer("/input/nativeMailTarget")
                    .and_then(Value::as_str)
                    != Some(target.as_str())
            {
                return Err(InboxError::Conflict {
                    input: request.input.clone(),
                    differs: "native notice target",
                });
            }
            if receipt.kind != PromotedInputKind::Synthetic
                || receipt.delivery != request.delivery
                || receipt.text != request.text
                || receipt.metadata.as_ref() != request.metadata
                || data
                    .pointer("/input/data/description")
                    .and_then(Value::as_str)
                    != request.description
            {
                return Err(InboxError::Conflict {
                    input: request.input.clone(),
                    differs: "native notice payload",
                });
            }
        }
        let outcome = Self::admit_synthetic_owned_within(&tx, &held, &request, true)?;
        if original.is_none() && matches!(outcome, AdmitOutcome::Reconciled { .. }) {
            return Err(InboxError::Corruption {
                input: request.input.clone(),
                reason: "native notice retry has no canonical admission".into(),
            });
        }
        tx.commit()?.discard();
        Ok(outcome)
    }
}
