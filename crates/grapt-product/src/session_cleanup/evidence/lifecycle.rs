use std::collections::BTreeSet;

use grapt_session::{SessionEvent, SessionSeed};
use grapt_types::{ExecutionId, ProjectId, SessionId, WorkspaceId};
use serde::Deserialize;
use serde_json::Value;

use super::super::RetentionReason;
use super::work::WorkEvidence;

#[derive(Debug, Default)]
pub(crate) struct SessionEvidence {
    pub(super) seq: Option<u64>,
    pub(super) last_event_ms: i64,
    pub(super) seeded: bool,
    pub(super) deleted: bool,
    pub(super) parent: Option<SessionId>,
    pub(super) retained: Option<RetentionReason>,
    pub(super) open_execution: Option<ExecutionId>,
    executions: BTreeSet<ExecutionId>,
    work: WorkEvidence,
    unfamiliar_work: bool,
}

impl SessionEvidence {
    pub(crate) fn retain(&mut self, reason: RetentionReason) {
        let _ = self.retained.get_or_insert(reason);
    }

    pub(super) fn finish(&mut self) -> Result<(), String> {
        // Partial folds cannot certify unfamiliar histories. They also must not
        // turn an already-retained mail/legacy session into a service-wide error.
        if !self.deleted && !self.unfamiliar_work && self.work.unresolved()? {
            self.retain(RetentionReason::UnresolvedWork);
        }
        Ok(())
    }

    pub(super) fn observe(
        &mut self,
        session: &SessionId,
        position: (u64, i64),
        kind: &str,
        raw: Option<String>,
        cutoff: i64,
    ) -> Result<(), String> {
        let (seq, created) = position;
        if created < 0 || self.seq.map_or(Some(0), |last| last.checked_add(1)) != Some(seq) {
            return Err("negative timestamp or incomplete/duplicate session sequence".into());
        }
        self.seq = Some(seq);
        self.last_event_ms = self.last_event_ms.max(created);
        let data: Value = serde_json::from_str(&super::required(raw)?)
            .map_err(|error| format!("invalid session JSON: {error}"))?;
        if self.deleted {
            return Err("session contains activity after its tombstone".into());
        }
        if matches!(kind, "session.deleted.1" | "session.deleted.2") {
            self.deleted = true;
            return Ok(());
        }
        if kind.starts_with("session.created.") && kind != "session.created.1" {
            // A future seed might describe lineage differently. It can protect
            // any candidate, so refusing only this aggregate would not suffice.
            return Err("unfamiliar seed version prevents complete lineage inspection".into());
        }
        let understood = matches!(
            kind,
            "session.created.1"
                | "session.renamed.1"
                | "session.agent.selected.1"
                | "session.model.selected.1"
                | "session.preset.selected.1"
                | "session.launched.1"
                | "session.execution.started.1"
                | "session.execution.succeeded.1"
                | "session.execution.failed.1"
                | "session.execution.interrupted.1"
        );
        if !understood {
            if !self.seeded {
                return Err("session work has no seed".into());
            }
            if !self.work.observe(session, seq, kind, &data)? {
                self.unfamiliar_work = true;
                self.retain(RetentionReason::UnfamiliarOrWorkEvidence);
            }
            // v3 terminals reserve their event timestamp before provider I/O;
            // their explicit completion observation can be substantially later.
            self.last_event_ms = self.last_event_ms.max(self.work.observed_ms);
            return Ok(());
        }
        if data.get("sessionID").and_then(Value::as_str) != Some(session.as_str()) {
            return Err("session payload identity differs from its aggregate".into());
        }
        if kind.starts_with("session.execution.") {
            return self.execution(session, kind, data);
        }
        let name = kind
            .strip_suffix(".1")
            .ok_or("unsupported lifecycle version")?;
        let event = SessionEvent::decode(name, 1, &data)
            .map_err(|error| error.to_string())?
            .ok_or("unsupported lifecycle event")?;
        if let SessionEvent::Created { info, .. } = event {
            if self.seeded || seq != 0 {
                return Err("duplicate or misplaced session seed".into());
            }
            self.parent = parent(&info)?;
            validate_seed(session, &info)?;
            for field in ["created", "updated"] {
                let stamp = info
                    .get("time")
                    .and_then(|time| time.get(field))
                    .and_then(Value::as_i64)
                    .filter(|stamp| *stamp >= 0)
                    .ok_or("missing or invalid seed timestamp")?;
                if stamp >= cutoff {
                    self.retain(RetentionReason::NotOld);
                }
            }
            self.seeded = true;
            if self.parent.is_some() {
                self.retain(RetentionReason::Family);
            }
        } else if !self.seeded {
            return Err("session lifecycle update has no seed".into());
        }
        Ok(())
    }

    fn execution(&mut self, session: &SessionId, kind: &str, data: Value) -> Result<(), String> {
        let data: ExecutionData =
            serde_json::from_value(data).map_err(|error| error.to_string())?;
        let execution = ExecutionId::new(data.execution).ok_or("empty execution identity")?;
        if !self.seeded || data.session != session.as_str() {
            return Err("execution has no matching session seed".into());
        }
        let failed = kind == "session.execution.failed.1";
        let interrupted = kind == "session.execution.interrupted.1";
        if data.error.is_some() != failed
            || data.reason.is_some() != interrupted
            || data.error.as_ref().is_some_and(|failure| {
                failure.name.is_empty() || failure.message.is_empty() || failure.message.len() > 512
            })
            || data
                .reason
                .as_deref()
                .is_some_and(|reason| !matches!(reason, "user" | "shutdown" | "superseded"))
        {
            return Err("execution terminal payload is malformed".into());
        }
        if kind == "session.execution.started.1" {
            if self.open_execution.is_some() || !self.executions.insert(execution.clone()) {
                return Err("overlapping or duplicated execution start".into());
            }
            self.open_execution = Some(execution);
        } else if self.open_execution.as_ref() != Some(&execution) {
            return Err("execution terminal has no matching open execution".into());
        } else {
            self.open_execution = None;
            self.work
                .execution_settled(self.seq.ok_or("execution has no sequence")?);
        }
        Ok(())
    }
}

fn parent(info: &Value) -> Result<Option<SessionId>, String> {
    info.get("parentID")
        .map(|raw| {
            raw.as_str()
                .ok_or_else(|| "invalid session parent identity".to_owned())?
                .parse()
                .map_err(|error| format!("invalid session parent: {error}"))
        })
        .transpose()
}

fn validate_seed(session: &SessionId, info: &Value) -> Result<(), String> {
    let string = |field| {
        info.get(field)
            .and_then(Value::as_str)
            .ok_or("missing seed field")
    };
    if string("id")? != session.as_str() {
        return Err("seed info.id differs from its aggregate".into());
    }
    let project = ProjectId::new(string("projectID")?).ok_or("invalid seed project")?;
    let mut seed = SessionSeed::new(project, string("directory")?, string("title")?)
        .map_err(|error| error.to_string())?;
    if let Some(workspace) = info.get("workspaceID") {
        let workspace = WorkspaceId::new(workspace.as_str().ok_or("invalid workspace")?)
            .ok_or("empty workspace identity")?;
        seed = seed
            .with_workspace(workspace)
            .map_err(|error| error.to_string())?;
    }
    if let Some(slug) = info.get("slug") {
        seed = seed
            .with_slug(slug.as_str().ok_or("invalid slug")?)
            .map_err(|error| error.to_string())?;
    }
    if let Some(agent) = info.get("agent") {
        seed = seed
            .with_agent(agent.as_str().ok_or("invalid agent")?)
            .map_err(|error| error.to_string())?;
    }
    if let Some(model) = info.get("model") {
        seed.with_model(model.clone())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionData {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    error: Option<ExecutionFailure>,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionFailure {
    #[serde(rename = "type")]
    name: String,
    message: String,
}
