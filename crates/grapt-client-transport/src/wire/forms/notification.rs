//! Notification-driven elicitation handling.
// Copyright OpenAI. Licensed under the Apache License, Version 2.0.

use grapt_api::u3a::{FormCancelledData, FormCreatedData, FormRepliedData, FormSuspendedData};
use serde_json::Value;

/// A validated hint to re-read, never a replacement for durable form rows.
pub fn global_form_changed(payload: &str, directory: &str) -> bool {
    let Ok(event) = serde_json::from_str::<Value>(payload) else {
        return false;
    };
    let data = &event["data"];
    let Some(actual_directory) = event["location"]["directory"].as_str() else {
        return false;
    };
    let (Ok(actual), Ok(selected)) = (
        grapt_types::LocationRef::new(actual_directory, None),
        grapt_types::LocationRef::new(directory, None),
    ) else {
        return false;
    };
    if actual.canonical_id() != selected.canonical_id()
        || !event["location"]["workspaceID"].is_null()
    {
        return false;
    }
    if data["sessionID"] != "global"
        || event["created"]
            .as_f64()
            .is_none_or(|time| !time.is_finite() || time < 0.0)
        || serde_json::from_value::<super::EventWireId>(event["id"].clone()).is_err()
        || event["durable"]["version"] != 2
        || event["durable"]["seq"].as_u64().is_none()
    {
        return false;
    }
    let id = match event["type"].as_str() {
        Some("form.created") => {
            if serde_json::from_value::<FormCreatedData>(data.clone()).is_err()
                || data["form"]["sessionID"] != "global"
            {
                return false;
            }
            &data["form"]["id"]
        }
        Some("form.replied") if serde_json::from_value::<FormRepliedData>(data.clone()).is_ok() => {
            &data["id"]
        }
        Some("form.cancelled")
            if serde_json::from_value::<FormCancelledData>(data.clone()).is_ok() =>
        {
            &data["id"]
        }
        Some("form.suspended")
            if serde_json::from_value::<FormSuspendedData>(data.clone()).is_ok() =>
        {
            &data["formID"]
        }
        _ => return false,
    };
    event["durable"]["aggregateID"] == *id
}

/// Session form facts belong to the Session aggregate, but do not fold into
/// the conversation cursor: the canonical list owns their rendering state.
pub fn session_form_changed(payload: &str, session: &super::SessionWireId) -> bool {
    let Ok(event) = serde_json::from_str::<Value>(payload) else {
        return false;
    };
    let data = &event["data"];
    let owns = |value: &Value| {
        serde_json::from_value::<super::SessionWireId>(value.clone())
            .is_ok_and(|owner| &owner == session)
    };
    if !owns(&data["sessionID"])
        || !owns(&event["durable"]["aggregateID"])
        || event["durable"]["version"] != 1
        || event["durable"]["seq"].as_u64().is_none()
        || event["created"]
            .as_f64()
            .is_none_or(|time| !time.is_finite() || time < 0.0)
        || serde_json::from_value::<super::EventWireId>(event["id"].clone()).is_err()
    {
        return false;
    }
    match event["type"].as_str() {
        Some("form.created") => {
            owns(&data["form"]["sessionID"])
                && serde_json::from_value::<FormCreatedData>(data.clone()).is_ok()
        }
        Some("form.replied") => serde_json::from_value::<FormRepliedData>(data.clone()).is_ok(),
        Some("form.cancelled") => serde_json::from_value::<FormCancelledData>(data.clone()).is_ok(),
        Some("form.suspended") => serde_json::from_value::<FormSuspendedData>(data.clone()).is_ok(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_hints_validate_owner_aggregate_and_all_four_payloads(
    ) -> Result<(), super::super::WireError> {
        let selected = crate::wire::session_id("ses_a")?;
        let foreign = crate::wire::session_id("ses_b")?;
        for (kind, data) in [
            (
                "form.created",
                json!({"sessionID":"ses_a","form":{"id":"frm_a","sessionID":"ses_a","title":"Name?","fields":[{"key":"name","type":"string"}]}}),
            ),
            (
                "form.replied",
                json!({"sessionID":"ses_a","id":"frm_a","answer":{}}),
            ),
            ("form.cancelled", json!({"sessionID":"ses_a","id":"frm_a"})),
            (
                "form.suspended",
                json!({"sessionID":"ses_a","formID":"frm_a","reason":"shutdown"}),
            ),
        ] {
            let mut event = json!({"type":kind,"id":"evt_a","created":1,
                "durable":{"aggregateID":"ses_a","seq":1,"version":1},"data":data});
            assert!(
                session_form_changed(&event.to_string(), &selected),
                "{kind}"
            );
            assert!(!session_form_changed(&event.to_string(), &foreign));
            event["durable"]["aggregateID"] = json!("ses_b");
            assert!(!session_form_changed(&event.to_string(), &selected));
        }
        Ok(())
    }

    #[test]
    fn only_valid_global_form_changes_request_a_read() {
        let base = json!({"id":"evt_000000000001", "created":1,
            "location":{"directory":"/work"},
            "durable":{"aggregateID":"frm_000000000001","seq":1,"version":2}});
        for (kind, data) in [
            (
                "form.created",
                json!({"sessionID":"global","form":{"id":"frm_000000000001","sessionID":"global","title":"Name?","fields":[{"key":"name","type":"string"}]}}),
            ),
            (
                "form.replied",
                json!({"sessionID":"global","id":"frm_000000000001","answer":{}}),
            ),
            (
                "form.cancelled",
                json!({"sessionID":"global","id":"frm_000000000001"}),
            ),
            (
                "form.suspended",
                json!({"sessionID":"global","formID":"frm_000000000001","reason":"shutdown"}),
            ),
        ] {
            let mut event = base.clone();
            event["type"] = json!(kind);
            event["data"] = data;
            assert!(global_form_changed(&event.to_string(), "/work"), "{kind}");
            assert!(!global_form_changed(&event.to_string(), "/other"));
            let mut foreign = event.clone();
            foreign["data"]["sessionID"] = json!("ses_000000000001");
            assert!(!global_form_changed(&foreign.to_string(), "/work"));
            let mut mismatched = event.clone();
            mismatched["durable"]["aggregateID"] = json!("frm_000000000002");
            assert!(!global_form_changed(&mismatched.to_string(), "/work"));
            event["durable"]["version"] = json!(1);
            assert!(!global_form_changed(&event.to_string(), "/work"));
        }
        assert!(!global_form_changed(
            r#"{"type":"form.created","data":{"sessionID":"global"}}"#,
            "/work"
        ));
    }
}
