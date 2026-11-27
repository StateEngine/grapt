//! Session information projection, separate from application composition.
use super::usage::{aggregate_usage, CompleteUsage};
use super::{session_cost, usage::usage_float, DogfoodApplication, SessionMetrics};
use grapt_types::SessionId;
use serde_json::{json, Value};

/// Keep metadata storage failures typed while refusing structurally incomplete carry.
pub(crate) fn lookup(
    application: &DogfoodApplication,
    session: &SessionId,
) -> Result<grapt_api::u3a::SessionResponseDto, grapt_api::u3a::SessionLookupApplicationError> {
    use grapt_api::u3a::SessionLookupApplicationError as Error;
    let record = {
        let inbox = application.lock_inbox();
        grapt_session::SessionStore::over(inbox.store()).record(session)
    }
    .map_err(|error| match error {
        grapt_session::SessionStoreError::Unknown { .. } => {
            Error::SessionNotFound(super::not_found(session))
        }
        _ => Error::Unavailable,
    })?;
    application
        .verify_session_carry_for_read(session)
        .map_err(|error| Error::InvalidRequest(super::invalid(error)))?;
    application
        .session_response(&record)
        .map_err(|_| Error::Unavailable)
}

impl DogfoodApplication {
    pub(crate) fn active_session_response(
        &self,
    ) -> Result<grapt_api::u3a::SessionActiveResponseDto, String> {
        let inbox = self.lock_inbox();
        let data: std::collections::BTreeMap<String, Value> = self
            .executor
            .active_executions()
            .iter()
            .map(|(session, execution)| {
                (
                    session.as_str().to_owned(),
                    self.executor
                        .active_state_row(inbox.store().connection(), session, execution),
                )
            })
            .collect();
        super::decode(json!({"data": data}))
    }

    pub(in crate::dogfood) fn session_metrics(
        &self,
        session: &SessionId,
    ) -> Result<SessionMetrics, String> {
        use grapt_session::history::HistoryReadError;
        let record = self.get_record(session)?;
        // A metrics shortfall never decides whether the row exists: past the
        // bound the totals are unknown, and only corruption refuses.
        let history = {
            let inbox = self.lock_inbox();
            let store = grapt_session::SessionHistoryStore::over(inbox.store());
            match store.fold_bounded(session, self.metrics_bound.rows, self.metrics_bound.bytes) {
                Ok(history) => history,
                Err(HistoryReadError::WindowTooLong { .. })
                | Err(HistoryReadError::WindowDataTooLong { .. }) => {
                    let newest = store
                        .newest_commit_ms(session)
                        .map_err(|error| error.to_string())?;
                    return Ok(SessionMetrics::ExceedsBound {
                        updated_ms: newest
                            .map_or(record.created_ms, |newest| newest.max(record.created_ms)),
                    });
                }
                Err(error) => return Err(error.to_string()),
            }
        };
        let messages = history.messages().map_err(|error| error.to_string())?;
        let mut usage = Some(CompleteUsage::ZERO);
        // An unknown contribution makes the total unknown; it never becomes
        // observed zero and never makes the Session unavailable.
        for (_, observations) in history.usage_attempts() {
            usage = usage.and_then(|held| {
                let next = aggregate_usage(observations).ok()??;
                held.checked_add(next).ok()
            });
        }
        if messages.iter().any(|message| message.usage.is_empty()) {
            usage = None;
        }
        let mut latest = record.created_ms;
        for message in &messages {
            if let Some(updated_ms) = message.updated_ms {
                latest = latest.max(updated_ms);
            }
        }
        Ok(SessionMetrics::Known {
            usage,
            updated_ms: latest,
            accounting: session_cost::accounting(self, session).ok(),
        })
    }

    pub(crate) fn session_info(
        &self,
        record: &grapt_session::SessionRecord,
    ) -> Result<Value, String> {
        let metrics = self.session_metrics(&record.session)?;
        let updated_ms = match &metrics {
            SessionMetrics::Known { updated_ms, .. }
            | SessionMetrics::ExceedsBound { updated_ms } => *updated_ms,
        };
        let mut info = metadata(record, updated_ms)?;
        let object = info
            .as_object_mut()
            .ok_or_else(|| "session projection is not an object".to_owned())?;
        object.extend({
            let inbox = self.lock_inbox();
            self.executor
                .session_state_fields(inbox.store().connection(), &record.session)
        });
        object.extend(session_cost::project(
            self,
            match &metrics {
                SessionMetrics::Known { accounting, .. } => accounting.as_ref(),
                SessionMetrics::ExceedsBound { .. } => None,
            },
            matches!(&metrics, SessionMetrics::Known { .. }),
        ));
        match metrics {
            SessionMetrics::Known { usage, .. } => {
                let tokens = usage.and_then(|usage| {
                    Some(json!({
                        "input": usage_float(usage.input, "input").ok()?,
                        "output": usage_float(usage.output, "output").ok()?,
                        "reasoning": usage_float(usage.reasoning, "reasoning").ok()?,
                        "cache": {
                            "read": usage_float(usage.cache_read, "cache-read-input").ok()?,
                            "write": usage_float(usage.cache_write, "cache-write-input").ok()?,
                        },
                    }))
                });
                if let Some(tokens) = tokens {
                    object.insert("tokens".to_owned(), tokens);
                }
            }
            // Accepted deviation (compat/openapi/DEVIATIONS.md, SC-HIST-01).
            SessionMetrics::ExceedsBound { .. } => {
                object.insert(
                    "metrics".to_owned(),
                    json!({
                        "state": "exceeds-bound",
                        "rows": self.metrics_bound.rows,
                        "bytes": self.metrics_bound.bytes,
                    }),
                );
            }
        }
        Ok(info)
    }
}

/// Shared row metadata; list reads deliberately do not project lifetime totals.
pub(crate) fn metadata(
    record: &grapt_session::SessionRecord,
    updated_ms: i64,
) -> Result<Value, String> {
    let mut info = json!({
        "id": record.session.as_str(),
        "projectID": record.project.as_str(),
        "time": {
            "created": record.created_ms as f64,
            "updated": updated_ms as f64,
        },
        "title": record.title,
        "location": {"directory": record.placement.directory()},
    });
    let object = info
        .as_object_mut()
        .ok_or_else(|| "session projection is not an object".to_owned())?;
    if let Some(agent) = &record.agent {
        object.insert("agent".to_owned(), json!(agent));
    }
    if let Some(model) = &record.model {
        object.insert("model".to_owned(), model.clone());
    }
    if let Some(parent) = &record.parent {
        object.insert("parentID".to_owned(), json!(parent.as_str()));
    }
    if let Some(preset) = &record.preset {
        object.insert("preset".to_owned(), json!(preset));
    }
    if let Some(workspace) = &record.workspace {
        object.insert(
            "location".to_owned(),
            json!({"directory":record.placement.directory(), "workspaceID":workspace.as_str()}),
        );
    }
    Ok(info)
}
