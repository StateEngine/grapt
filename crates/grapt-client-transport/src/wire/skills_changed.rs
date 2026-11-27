//! A location-scoped hint, outside the Session cursor and durable history.
pub fn skills_changed(payload: &str, directory: &str) -> bool {
    let Ok(event) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    if event["type"] != "skills.changed"
        || !event["durable"].is_null()
        || !event["data"]
            .as_object()
            .is_some_and(|data| data.is_empty())
        || serde_json::from_value::<super::EventWireId>(event["id"].clone()).is_err()
        || !event["created"]
            .as_f64()
            .is_some_and(|time| time.is_finite() && time >= 0.0 && time.fract() == 0.0)
        || !event["location"]["workspaceID"].is_null()
    {
        return false;
    }
    let Some(actual) = event["location"]["directory"].as_str() else {
        return false;
    };
    match (
        grapt_types::LocationRef::new(actual, None),
        grapt_types::LocationRef::new(directory, None),
    ) {
        (Ok(actual), Ok(selected)) => actual.canonical_id() == selected.canonical_id(),
        _ => false,
    }
}
