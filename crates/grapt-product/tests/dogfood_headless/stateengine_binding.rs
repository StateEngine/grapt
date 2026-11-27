//! Portable HTTP fixture helpers for a product-owned native Session.
use super::*;

pub(super) fn put_no_content(address: SocketAddr, path: &str, body: &str) {
    let (status, reply) = request(address, "PUT", path, body);
    assert_eq!(status, 204, "fixture PUT failed for {path}");
    assert!(reply.is_empty(), "fixture PUT returned unexpected content");
}

pub(super) fn bind(address: SocketAddr, session: &str, capability: bool) {
    let path = format!("/api/session/{session}");
    put_no_content(
        address,
        &format!("{path}/instructions/entries/stateengine-identity"),
        &serde_json::json!({"value": {"scope":"project", "project_id":"native-fixture", "agent_id":"agt_native_fixture"}}).to_string(),
    );
    if capability {
        put_no_content(
            address,
            &format!("{path}/stateengine/capability"),
            r#"{"capability":"NATIVE_FIXTURE_CAPABILITY"}"#,
        );
    }
}

pub(super) fn create_native(address: SocketAddr, root: &Path) -> String {
    json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({
            "location": {"directory": root}
        })
        .to_string(),
    )["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("native fixture Session missing"))
        .to_owned()
}
