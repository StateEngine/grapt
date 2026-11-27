use super::*;

pub(super) fn source_json(source: &PermissionHttpSource) -> Value {
    match source {
        PermissionHttpSource::Tool {
            message_id,
            call_id,
        } => json!({"type": "tool", "messageID": message_id, "callID": call_id}),
    }
}

pub(super) fn decode_source(
    raw: Option<&str>,
    corrupt: &impl Fn(String) -> PermissionHttpError,
) -> Result<Option<PermissionHttpSource>, PermissionHttpError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let value: Value = decode_json(raw, corrupt)?;
    let object = value
        .as_object()
        .ok_or_else(|| corrupt("permission source is not an object".to_owned()))?;
    if object.len() != 3 {
        return Err(corrupt(
            "permission source has unknown or missing fields".to_owned(),
        ));
    }
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| corrupt("permission source type is absent".to_owned()))?;
    if kind != "tool" {
        return Err(corrupt(format!("unknown permission source {kind:?}")));
    }
    let message_id = object
        .get("messageID")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| corrupt("permission source messageID is absent".to_owned()))?;
    let call_id = object
        .get("callID")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| corrupt("permission source callID is absent".to_owned()))?;
    Ok(Some(PermissionHttpSource::Tool {
        message_id: message_id.to_owned(),
        call_id: call_id.to_owned(),
    }))
}

pub(super) fn saved_rule_patterns(
    store: &EventStore,
    project: &ProjectId,
    action: &str,
) -> Result<Vec<String>, PermissionHttpError> {
    let mut statement = store.connection().prepare(
        "SELECT resource FROM permission_http_rule
         WHERE project_id = ?1 AND action = ?2 ORDER BY resource LIMIT ?3",
    )?;
    let rows = statement.query_map(
        rusqlite::params![
            project.as_str(),
            action,
            i64::try_from(MAX_HTTP_PERMISSION_RESOURCES + 1).map_err(|_| {
                PermissionHttpError::ResourceBound {
                    max: MAX_HTTP_PERMISSION_RESOURCES,
                }
            })?
        ],
        |row| row.get::<_, String>(0),
    )?;
    let patterns = rows.collect::<Result<Vec<_>, _>>()?;
    if patterns.len() > MAX_HTTP_PERMISSION_RESOURCES {
        return Err(PermissionHttpError::ResourceBound {
            max: MAX_HTTP_PERMISSION_RESOURCES,
        });
    }
    Ok(patterns)
}

pub(super) fn saved_patterns(
    tx: &grapt_store::GovernedTransaction<'_>,
    id: &PermissionId,
) -> Result<Vec<String>, PermissionHttpError> {
    let raw = tx.query_row(
        "SELECT save_json FROM permission_http_request WHERE id = ?1",
        [id.as_str()],
        |row| row.get::<_, Option<String>>(0),
    )?;
    let corrupt = |reason: String| PermissionHttpError::Corruption {
        id: id.clone(),
        reason,
    };
    let patterns: Vec<String> = decode_optional_json(raw.as_deref(), &corrupt)?.unwrap_or_default();
    if patterns.len() > MAX_HTTP_PERMISSION_RESOURCES {
        return Err(PermissionHttpError::ResourceBound {
            max: MAX_HTTP_PERMISSION_RESOURCES,
        });
    }
    Ok(patterns)
}

pub(super) fn admit_rule_patterns(
    tx: &grapt_store::GovernedTransaction<'_>,
    project: &ProjectId,
    action: &str,
    proposed: &[String],
) -> Result<(), PermissionHttpError> {
    let existing = {
        let mut statement = tx.prepare(
            "SELECT resource FROM permission_http_rule
             WHERE project_id = ?1 AND action = ?2 ORDER BY resource LIMIT ?3",
        )?;
        let rows = statement.query_map(
            rusqlite::params![
                project.as_str(),
                action,
                i64::try_from(MAX_HTTP_PERMISSION_RESOURCES + 1).map_err(|_| {
                    PermissionHttpError::ResourceBound {
                        max: MAX_HTTP_PERMISSION_RESOURCES,
                    }
                })?
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if existing.len() > MAX_HTTP_PERMISSION_RESOURCES {
        return Err(PermissionHttpError::ResourceBound {
            max: MAX_HTTP_PERMISSION_RESOURCES,
        });
    }
    let additions = proposed
        .iter()
        .filter(|pattern| !existing.contains(pattern))
        .count();
    if existing.len().saturating_add(additions) > MAX_HTTP_PERMISSION_RESOURCES {
        return Err(PermissionHttpError::ResourceBound {
            max: MAX_HTTP_PERMISSION_RESOURCES,
        });
    }
    for resource in proposed {
        if revocation::active_origins(tx, project, action, resource)?.len()
            >= crate::event::MAX_RULE_ORIGINS
        {
            return Err(PermissionHttpError::ResourceBound {
                max: crate::event::MAX_RULE_ORIGINS,
            });
        }
    }
    Ok(())
}

pub(super) fn exact_retry(
    tx: &grapt_store::GovernedTransaction<'_>,
    command: &PermissionReplyCommand<'_>,
) -> Result<bool, PermissionHttpError> {
    let raw = tx.query_row(
        "SELECT data FROM event
         WHERE type = 'permission.replied.2'
           AND json_extract(data, '$.requestID') = ?1
         ORDER BY seq DESC LIMIT 1",
        [command.id.as_str()],
        |row| row.get::<_, String>(0),
    );
    let raw = match raw {
        Ok(raw) => raw,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(false),
        Err(error) => return Err(PermissionHttpError::Sqlite(error)),
    };
    let value: Value =
        serde_json::from_str(&raw).map_err(|error| PermissionHttpError::Corruption {
            id: command.id.clone(),
            reason: error.to_string(),
        })?;
    let decoded =
        crate::PermissionEvent::decode("permission.replied", 2, &value).map_err(|error| {
            PermissionHttpError::Corruption {
                id: command.id.clone(),
                reason: error.to_string(),
            }
        })?;
    let Some(crate::PermissionEvent::Replied {
        request,
        session,
        reply,
        attribution: Some(attribution),
    }) = decoded
    else {
        return Err(PermissionHttpError::Corruption {
            id: command.id.clone(),
            reason: "v2 reply did not decode to attributed reply".to_owned(),
        });
    };
    Ok(request == *command.id
        && session == *command.session
        && reply == command.reply.canonical()
        && attribution.principal == command.owner.subject
        && attribution.client == *command.owner.client
        && attribution.request == command.decision.request
        && attribution.nonce == command.decision.nonce
        && attribution.source == PermissionReplySource::DirectReply)
}

pub(super) fn cascade_reject(
    tx: &grapt_store::GovernedTransaction<'_>,
    held: &grapt_store::HeldWriters,
    command: &PermissionReplyCommand<'_>,
    mint: &mut impl FnMut() -> Result<DecisionIds, PermissionHttpError>,
    pending: &[PermissionId],
) -> Result<Vec<crate::tool_authority::CommittedPermissionReply>, PermissionHttpError> {
    let mut committed = Vec::new();
    let mut after = String::new();
    loop {
        let siblings = {
            let mut statement = tx.prepare(
                "SELECT p.id FROM permission_request p
                 JOIN permission_http_request h ON h.id = p.id
                 WHERE p.session_id = ?1 AND p.status IN ('pending', 'suspended')
                   AND p.id != ?2 AND p.id > ?3
                   AND h.owner_subject = ?4 AND h.owner_client = ?5
                 ORDER BY p.id LIMIT ?6",
            )?;
            let rows = statement.query_map(
                rusqlite::params![
                    command.session.as_str(),
                    command.id.as_str(),
                    after,
                    command.owner.subject,
                    command.owner.client.as_str(),
                    i64::try_from(MAX_HTTP_PERMISSION_ROWS).map_err(|_| {
                        PermissionHttpError::ApprovalBound {
                            max: MAX_HTTP_PERMISSION_ROWS,
                        }
                    })?
                ],
                |row| row.get::<_, String>(0),
            )?;
            rows.map(|row| {
                let raw = row?;
                raw.parse().map_err(|error: grapt_types::IdError| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })
            .collect::<Result<Vec<PermissionId>, rusqlite::Error>>()?
        };
        if siblings.is_empty() {
            return Ok(committed);
        }
        for sibling in &siblings {
            let sibling_command = PermissionReplyCommand {
                owner: command.owner.clone(),
                session: command.session,
                id: sibling,
                reply: PermissionHttpReply::Reject,
                project: None,
                created_ms: command.created_ms,
                decision: mint()?,
            };
            append_v2_reply(tx, held, &sibling_command, &sibling_command.decision)?;
            if pending.contains(sibling) {
                committed.push(crate::tool_authority::CommittedPermissionReply::new(
                    sibling.clone(),
                    command.session.clone(),
                    PermissionReply::Reject,
                ));
            }
        }
        let Some(last) = siblings.last() else {
            return Ok(committed);
        };
        after.clear();
        after.push_str(last.as_str());
        if siblings.len() < MAX_HTTP_PERMISSION_ROWS {
            return Ok(committed);
        }
    }
}

pub(super) fn append_v2_reply(
    tx: &grapt_store::GovernedTransaction<'_>,
    held: &grapt_store::HeldWriters,
    command: &PermissionReplyCommand<'_>,
    ids: &DecisionIds,
) -> Result<(), PermissionHttpError> {
    let aggregate = AggregateId::Session(command.session.clone());
    let writer = held.token(&aggregate)?;
    let version = EventVersion::new(2).ok_or(PermissionError::Version)?;
    let data = json!({
        "sessionID": command.session.as_str(), "requestID": command.id.as_str(), "reply": reply_as_str(command.reply.canonical()),
        "principal": command.owner.subject, "clientID": command.owner.client.as_str(),
        "replyRequestID": ids.request.as_str(), "nonce": ids.nonce.as_str(), "source": "direct-reply",
    });
    EventStore::append_within(
        tx,
        AppendInput {
            id: &ids.event,
            aggregate,
            writer,
            kind_name: "permission.replied",
            version,
            created_ms: command.created_ms,
            data: &data,
        },
        &mut [&mut PermissionProjector],
    )?;
    Ok(())
}

pub(super) type RawHttpRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

pub(super) fn decode_row(raw: RawHttpRow) -> Result<PermissionHttpRequest, PermissionHttpError> {
    let id: PermissionId =
        raw.0.parse().map_err(
            |error: grapt_types::IdError| PermissionHttpError::Corruption {
                id: fallback_id(),
                reason: error.to_string(),
            },
        )?;
    let corrupt = |reason: String| PermissionHttpError::Corruption {
        id: id.clone(),
        reason,
    };
    let session = raw
        .1
        .parse()
        .map_err(|error: grapt_types::IdError| corrupt(error.to_string()))?;
    let agent = AgentId::new(raw.5.clone())
        .ok_or_else(|| corrupt("permission agent identity is invalid".to_owned()))?;
    let lifecycle = match (raw.3.as_str(), raw.4.as_deref()) {
        ("pending", None) => PermissionHttpLifecycle::Pending,
        ("suspended", Some("shutdown")) => {
            PermissionHttpLifecycle::Suspended(PermissionSuspensionReason::Shutdown)
        }
        ("suspended", Some("process-loss")) => {
            PermissionHttpLifecycle::Suspended(PermissionSuspensionReason::ProcessLoss)
        }
        ("pending", Some(_)) => {
            return Err(corrupt("pending row carries a suspended reason".to_owned()))
        }
        ("suspended", None) => return Err(corrupt("suspended row has no reason".to_owned())),
        ("replied" | "cancelled", _) => {
            return Err(corrupt(
                "terminal row reached the open-row decoder".to_owned(),
            ))
        }
        (other, _) => return Err(corrupt(format!("unknown status {other:?}"))),
    };
    let resources: Vec<String> = decode_json(&raw.6, &corrupt)?;
    let save: Option<Vec<String>> = decode_optional_json(raw.7.as_deref(), &corrupt)?;
    if resources.len() > MAX_HTTP_PERMISSION_RESOURCES
        || save
            .as_ref()
            .is_some_and(|patterns| patterns.len() > MAX_HTTP_PERMISSION_RESOURCES)
    {
        return Err(PermissionHttpError::ResourceBound {
            max: MAX_HTTP_PERMISSION_RESOURCES,
        });
    }
    Ok(PermissionHttpRequest {
        id: id.clone(),
        session,
        agent,
        action: raw.2,
        resources,
        save,
        metadata: decode_optional_json(raw.8.as_deref(), &corrupt)?,
        source: decode_source(raw.9.as_deref(), &corrupt)?,
        lifecycle,
    })
}

pub(super) fn decode_json<T: serde::de::DeserializeOwned>(
    raw: &str,
    corrupt: &impl Fn(String) -> PermissionHttpError,
) -> Result<T, PermissionHttpError> {
    serde_json::from_str(raw).map_err(|error| corrupt(error.to_string()))
}

pub(super) fn decode_optional_json<T: serde::de::DeserializeOwned>(
    raw: Option<&str>,
    corrupt: &impl Fn(String) -> PermissionHttpError,
) -> Result<Option<T>, PermissionHttpError> {
    raw.map(|value| decode_json(value, corrupt)).transpose()
}

pub(super) fn fallback_id() -> PermissionId {
    match "per_corrupt".parse() {
        Ok(id) => id,
        Err(_) => unreachable!("static permission fallback identity is valid"),
    }
}
