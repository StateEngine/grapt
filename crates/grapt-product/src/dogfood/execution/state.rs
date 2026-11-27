//! Execution exposes only its confirmation port to session read projection.
use super::*;
use serde_json::{Map, Value};
impl crate::dogfood::ExecutionBackend {
    pub(in crate::dogfood) fn session_state_fields(
        &self,
        connection: &rusqlite::Connection,
        session: &SessionId,
    ) -> Map<String, Value> {
        crate::dogfood::session_status::state_fields(&self.0.active, connection, session, None)
    }

    pub(in crate::dogfood) fn active_state_row(
        &self,
        connection: &rusqlite::Connection,
        session: &SessionId,
        execution: &ExecutionId,
    ) -> Value {
        let mut fields = crate::dogfood::session_status::state_fields(
            &self.0.active,
            connection,
            session,
            Some(execution),
        );
        fields.insert("type".into(), serde_json::json!("running"));
        fields.insert("executionID".into(), serde_json::json!(execution.as_str()));
        Value::Object(fields)
    }
}
