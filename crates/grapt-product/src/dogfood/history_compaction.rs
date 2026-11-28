//! Completed compactions as public Session history.
//!
//! Each committed `session.compaction.ended.2` descriptor is one
//! `compaction` message at its own durable sequence. The descriptor retains
//! history by reference rather than copying a transcript: `recent` is the
//! oracle's bounded public value for the compacted window, re-materialized at
//! replay through the same projection the live path publishes, so history
//! agrees with the live notice. The durable identity the descriptor joins
//! (compaction, context window, epoch and trigger) is carried in `metadata`.

use grapt_api::u3a::SessionMessageCompactionCompletedDto;
use grapt_types::{CaptureDisposition, CompactionSummary, CompactionTrigger, SessionId};
use serde_json::json;

use super::continuation::recent::MAX_RECENT_ID_BYTES;
use super::ServerApplication;

pub(super) struct ReportedCompaction {
    recent: serde_json::Value,
    estimates: Option<super::continuation::recent::TokenEstimates>,
}

/// Summary text is bounded like the retained context that reads it back.
const MAX_PUBLIC_SUMMARY_BYTES: u64 = 256 * 1024;
const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;

/// A committed compaction descriptor as history indexes it: located and
/// identified, read and rendered only when a page selects it.
pub(super) struct CommittedCompaction {
    pub(super) seq: u64,
    /// The public row id, `msg_` and the event identity's suffix.
    pub(super) id: String,
    event: String,
    created: i64,
    data: Option<String>,
}

impl ServerApplication {
    /// One selected descriptor, independent of the number of prior compactions.
    pub(super) fn compaction_at(
        &self,
        session: &SessionId,
        sequence: u64,
    ) -> Result<Option<CommittedCompaction>, String> {
        let sequence = i64::try_from(sequence).map_err(|error| error.to_string())?;
        let rows = {
            let inbox = self.lock_inbox();
            let visible = grapt_store::history_visibility::HistoryVisibility::read(
                inbox.store().connection(),
                session,
            )
            .map_err(|error| error.to_string())?
            .sql("seq");
            let mut statement = inbox
                .store()
                .connection()
                .prepare(&format!(
                    "SELECT COALESCE(length(CAST(id AS BLOB)), -1), id, seq, created,
                     CASE WHEN length(CAST(data AS BLOB)) <= {MAX_DESCRIPTOR_BYTES} THEN data
                     ELSE NULL END
                     FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
                     AND {visible} AND type = 'session.compaction.ended.2' AND seq=?2 LIMIT 1"
                ))
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map((session.as_str(), sequence), |row| {
                    let event_id_len = row.get::<_, i64>(0)?;
                    let id = if event_id_len <= MAX_RECENT_ID_BYTES as i64 {
                        Some(row.get::<_, String>(1)?)
                    } else {
                        None
                    };
                    Ok((
                        id,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })
                .map_err(|error| error.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        let mut rows = rows.into_iter()
            .map(|(event, seq, created, data)| {
                let event = event.ok_or_else(|| {
                    format!(
                        "recent compaction event identity exceeds its {MAX_RECENT_ID_BYTES}-byte read bound"
                    )
                })?;
                let seq = u64::try_from(seq)
                    .map_err(|_| "compaction descriptor has a negative sequence".to_owned())?;
                let suffix = event
                    .strip_prefix("evt_")
                    .ok_or("compaction event identity has no prefix")?;
                Ok(CommittedCompaction {
                    seq,
                    id: format!("msg_{suffix}"),
                    event,
                    created,
                    data,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(rows.pop())
    }

    /// One committed compaction as its public `compaction` row.
    pub(super) fn compaction_json(
        &self,
        session: &SessionId,
        committed: &CommittedCompaction,
        recent: &std::collections::HashMap<String, ReportedCompaction>,
    ) -> Result<String, String> {
        super::history_json::rendered_row();
        let created = committed.created;
        let data = committed
            .data
            .as_deref()
            .ok_or("compaction descriptor exceeds its public bound")?;
        let value: serde_json::Value =
            serde_json::from_str(data).map_err(|error| error.to_string())?;
        let descriptor = grapt_event::session_compaction::decode(&value)
            .map_err(|error| format!("compaction descriptor is unreadable: {error}"))?;
        if descriptor.source_session() != session {
            return Err("compaction descriptor names another Session".to_owned());
        }
        // A provider compaction has no readable text: its row states the
        // fact and its counts, never a silent gap.
        let summary = match descriptor.summary() {
            CompactionSummary::ClientReadable { artifact } => {
                if artifact.length() > MAX_PUBLIC_SUMMARY_BYTES {
                    return Err("compaction summary exceeds its public bound".to_owned());
                }
                let text = super::continuation::read_text(
                    self.executor.artifacts(),
                    artifact,
                    CaptureDisposition::Complete,
                    "compaction summary",
                )?;
                super::continuation::se_summary_notice(text.as_str())
            }
            CompactionSummary::ProviderBound { .. } => {
                super::continuation::provider_compaction_notice(
                    self.executor.artifacts(),
                    &descriptor,
                )?
            }
        };
        let reason = match descriptor.trigger() {
            CompactionTrigger::UserRequested => "manual",
            _ => "auto",
        };
        let reported = recent
            .get(&committed.event)
            .ok_or("compaction recent lineage is missing a committed descriptor")?;
        let mut row = json!({
            "type": "compaction",
            "status": "completed",
            "id": committed.id,
            "metadata": {
                "compactionID": descriptor.id().as_str(),
                "contextWindowID": descriptor.target_window().as_str(),
                "epoch": descriptor.target_epoch().value(),
                "trigger": trigger_name(descriptor.trigger()),
            },
            "time": {"created": created},
            "reason": reason,
            "summary": summary.as_str(),
            "recent": reported.recent.clone(),
        });
        if let Some(estimates) = &reported.estimates {
            let mut totals =
                json!({"before": estimates.before.to_string(), "basis": estimates.basis});
            if let Some(after) = estimates.after {
                totals["after"] = after.to_string().into();
            }
            row["metadata"]["tokenEstimates"] = totals;
        }
        // The row keeps the lossless unit object for boundary values
        // and stays strict through the page pipeline; the frozen
        // string wire form is produced exactly once, by the final DTO
        // serialization. Emit the row text in frozen field order:
        // the public decoder is tag-first, and a bare Value would
        // sort its keys and lose the tag.
        let _message: SessionMessageCompactionCompletedDto = super::decode(row.clone())?;
        let id = serde_json::to_string(&row["id"]).map_err(|error| error.to_string())?;
        let metadata =
            serde_json::to_string(&row["metadata"]).map_err(|error| error.to_string())?;
        let summary = serde_json::to_string(&row["summary"]).map_err(|error| error.to_string())?;
        let recent = serde_json::to_string(&row["recent"]).map_err(|error| error.to_string())?;
        Ok(format!(
            "{{\"type\":\"compaction\",\"status\":\"completed\",\"id\":{id},\"metadata\":{metadata},\"time\":{{\"created\":{created}}},\"reason\":\"{reason}\",\"summary\":{summary},\"recent\":{recent}}}"
        ))
    }
}

const fn trigger_name(trigger: CompactionTrigger) -> &'static str {
    match trigger {
        CompactionTrigger::ContextOverflow => "context-overflow",
        CompactionTrigger::PolicyThreshold => "policy-threshold",
        CompactionTrigger::UserRequested => "user-requested",
        CompactionTrigger::EmergencyHeadroom => "emergency-headroom",
        CompactionTrigger::StructuralBound => "structural-bound",
    }
}

/// A page retains only its selected lineage values and one replay position.
/// Later descriptors are read only when the next candidate reaches them.
pub(super) struct PageRecent {
    cursor: super::continuation::recent::ChainCursor,
    selected: Vec<u64>,
    values: std::collections::HashMap<String, ReportedCompaction>,
}

impl PageRecent {
    pub(super) fn new(selected: impl Iterator<Item = u64>) -> Self {
        Self {
            cursor: super::continuation::recent::ChainCursor::new(),
            selected: selected.collect(),
            values: std::collections::HashMap::new(),
        }
    }

    pub(super) fn through(
        &mut self,
        application: &ServerApplication,
        session: &SessionId,
        through: u64,
    ) -> Result<&std::collections::HashMap<String, ReportedCompaction>, String> {
        let steps = {
            let inbox = application.lock_inbox();
            self.cursor
                .selected_through(
                    &inbox,
                    application.executor.artifacts(),
                    session,
                    &self.selected,
                    through,
                )
                .map_err(|error| format!("compaction recent lineage is unreadable: {error}"))?
        };
        for step in steps {
            let value = match step.recent.as_str() {
                Some(text) => serde_json::Value::String(text.into_owned()),
                None => json!({"secode.unit16": step.recent.internal_b64()}),
            };
            self.values.insert(
                step.event,
                ReportedCompaction {
                    recent: value,
                    estimates: step.token_estimates,
                },
            );
        }
        Ok(&self.values)
    }
}
