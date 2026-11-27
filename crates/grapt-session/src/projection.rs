//! Last-write-wins session projection.

use crate::event::SessionEvent;
use grapt_types::{SessionId, SessionLaunch};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionRow {
    pub seeded: bool,
    pub slug: Option<String>,
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub model: Option<Value>,
    pub preset: Option<String>,
    /// How the launch was recorded, when it was recorded at all.
    ///
    /// `None` is an ABSENT fact, not "windowed": an old client that never
    /// stated a mode, or a Session created before this fact existed. The stop
    /// rule needs that three-way distinction, so this is not a `bool`.
    pub launch: Option<SessionLaunch>,
    /// From `info.time` — the seed's own timestamp, not the event row's.
    pub time_created_ms: Option<i64>,
}

#[derive(Debug, Default)]
pub struct SessionProjection {
    pub rows: HashMap<SessionId, SessionRow>,
    /// Aggregates first seen via an update event — their `session.created`
    /// is outside the log (head compaction). Measured fact, not failure.
    pub seedless: u64,
}

impl SessionProjection {
    fn row(&mut self, session: &SessionId) -> &mut SessionRow {
        if !self.rows.contains_key(session) {
            self.seedless += 1;
            self.rows.insert(session.clone(), SessionRow::default());
        }
        // Key inserted above if absent; index is safe for all callers.
        match self.rows.get_mut(session) {
            Some(row) => row,
            None => unreachable!("row inserted above"),
        }
    }

    pub fn apply(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Created { session, info } => {
                let get_str =
                    |field: &str| info.get(field).and_then(Value::as_str).map(str::to_owned);
                let row = SessionRow {
                    seeded: true,
                    slug: get_str("slug"),
                    project_id: get_str("projectID"),
                    title: get_str("title"),
                    agent: get_str("agent"),
                    model: info.get("model").filter(|v| !v.is_null()).cloned(),
                    preset: None,
                    // Absent until a `session.launched` fact says otherwise.
                    launch: None,
                    // info.time is an object {created, updated} (measured).
                    time_created_ms: info
                        .get("time")
                        .and_then(|t| t.get("created"))
                        .and_then(Value::as_i64),
                };
                self.rows.insert(session, row);
            }
            SessionEvent::Renamed { session, title } => {
                self.row(&session).title = Some(title);
            }
            SessionEvent::AgentSelected { session, agent } => {
                self.row(&session).agent = Some(agent);
            }
            SessionEvent::ModelSelected { session, model } => {
                self.row(&session).model = Some(model);
            }
            SessionEvent::PresetSelected { session, preset } => {
                self.row(&session).preset = preset;
            }
            SessionEvent::Launched { session, launch } => {
                self.row(&session).launch = Some(launch);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode(kind: &str, data: serde_json::Value) -> SessionEvent {
        match SessionEvent::decode(kind, 1, &data) {
            Ok(Some(e)) => e,
            other => panic!("decode {kind}: {other:?}"),
        }
    }

    #[test]
    fn seed_then_last_write_wins() -> Result<(), grapt_types::IdError> {
        let mut p = SessionProjection::default();
        p.apply(decode(
            "session.created",
            json!({"sessionID":"ses_a","info":{"id":"ses_a","slug":"quick-otter",
                "projectID":"global","title":"t0","agent":"build",
                "model":{"id":"m1","providerID":"p1","variant":"high"},
                "time":{"created":1784090756114i64,"updated":1784090756114i64}}}),
        ));
        p.apply(decode(
            "session.renamed",
            json!({"sessionID":"ses_a","title":"t1"}),
        ));
        p.apply(decode(
            "session.renamed",
            json!({"sessionID":"ses_a","title":"t2"}),
        ));
        p.apply(decode(
            "session.preset.selected",
            json!({"sessionID":"ses_a","preset":"inspect"}),
        ));
        let session: SessionId = "ses_a".parse()?;
        let row = &p.rows[&session];
        assert!(row.seeded);
        assert_eq!(row.title.as_deref(), Some("t2"));
        assert_eq!(row.slug.as_deref(), Some("quick-otter"));
        assert_eq!(row.preset.as_deref(), Some("inspect"));
        assert_eq!(row.time_created_ms, Some(1784090756114));
        assert_eq!(p.seedless, 0);
        Ok(())
    }

    #[test]
    fn update_before_seed_is_measured_as_seedless() -> Result<(), grapt_types::IdError> {
        let mut p = SessionProjection::default();
        p.apply(decode(
            "session.renamed",
            json!({"sessionID":"ses_b","title":"x"}),
        ));
        assert_eq!(p.seedless, 1);
        let session: SessionId = "ses_b".parse()?;
        assert!(!p.rows[&session].seeded);
        Ok(())
    }
}
