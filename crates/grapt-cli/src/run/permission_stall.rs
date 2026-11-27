//! The stall detail for [`super::RunError::TerminalTimeout`]: the pending
//! permission requests the session is still waiting on, marked when the same
//! classifier `--auto` itself uses would refuse them, plus the two unblocks.

use grapt_types::SessionId;

use super::auto_reply_for;
use crate::AutoReply;

/// The stall detail for [`super::RunError::TerminalTimeout`]: the pending
/// permission requests the session is still waiting on, and the two ways to
/// unblock it. Each request is marked `(refused by --auto)` when
/// [`auto_reply_for`] — the classifier the run's own `--auto` replies with —
/// would refuse it, so a re-run suggestion never oversells what it would
/// clear. Empty when the last permission snapshot named no pending request,
/// so the plain timeout stays the whole message.
pub(super) fn pending_permission_stall(
    last_permissions: &Option<Vec<u8>>,
    session: &SessionId,
) -> String {
    let Some(canonical) = last_permissions.as_ref() else {
        return String::new();
    };
    let Ok(rows) = serde_json::from_slice::<serde_json::Value>(canonical) else {
        return String::new();
    };
    let Some(items) = rows["data"].as_array() else {
        return String::new();
    };
    let mut named: Vec<String> = Vec::new();
    let mut refused = 0_usize;
    for row in items {
        let Some(id) = row["id"].as_str() else {
            continue;
        };
        if row["status"] != "pending" {
            continue;
        }
        let action = row["action"].as_str().unwrap_or("unknown action");
        let resources: Vec<&str> = row["resources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect();
        let resources = if resources.is_empty() {
            "no resources named".to_owned()
        } else {
            resources.join(", ")
        };
        // A re-run with --auto classifies with auto_reply_for too: an
        // unclassified request, an owner-floor change, or a removal under
        // --auto is refused there as well, so say so per request.
        if auto_reply_for(action, AutoReply::Auto, row.get("metadata")) == "reject" {
            refused += 1;
            named.push(format!("{id}: {action} on {resources} (refused by --auto)"));
        } else {
            named.push(format!("{id}: {action} on {resources}"));
        }
    }
    if named.is_empty() {
        return String::new();
    }
    let auto_tail = if refused == 0 {
        "which approves all of them.".to_owned()
    } else {
        format!("which approves the unmarked ones and still refuses the {refused} marked one(s).")
    };
    format!(
        ". The session is waiting on {} pending permission request(s): {}. This run leaves them unanswered; reply to the request(s) from another client via the service API (POST /api/session/{}/permission/<requestID>/reply) or re-run with --auto, {auto_tail}",
        named.len(),
        named.join("; "),
        session
    )
}

#[cfg(test)]
mod tests {
    use super::pending_permission_stall;
    use grapt_types::SessionId;
    use std::str::FromStr;

    fn session() -> SessionId {
        match SessionId::from_str("ses_0123456789abcdef0123456789abcdef") {
            Ok(id) => id,
            Err(error) => panic!("test fixture session id is invalid: {error}"),
        }
    }

    fn stall(rows: serde_json::Value) -> (String, SessionId) {
        let session = session();
        let canonical = serde_json::json!({"data": rows, "hasMore": false})
            .to_string()
            .into_bytes();
        (
            pending_permission_stall(&Some(canonical), &session),
            session,
        )
    }

    #[test]
    fn a_stalled_run_names_the_pending_permission_request_and_its_unblocks() {
        let (stall, session) = stall(serde_json::json!([
            {"id": "per_001", "action": "fs.modify", "resources": ["money.py"],
             "status": "pending"},
            {"id": "per_002", "action": "fs.remove", "resources": ["old.txt"],
             "status": "suspended", "suspendedReason": "shutdown"}
        ]));
        assert!(
            stall.contains("per_001"),
            "the live request id must be named: {stall}"
        );
        assert!(
            stall.contains("fs.modify"),
            "the request's action must be named: {stall}"
        );
        assert!(
            stall.contains("money.py"),
            "the request's resources must be named: {stall}"
        );
        assert!(
            !stall.contains("per_002"),
            "a suspended request is not a live blocker: {stall}"
        );
        assert!(
            stall.contains("1 pending permission request"),
            "the count must match the live rows: {stall}"
        );
        assert!(
            stall.contains(&format!(
                "/api/session/{session}/permission/<requestID>/reply"
            )),
            "the reply endpoint for this session must be named: {stall}"
        );
        assert!(
            stall.contains("--auto"),
            "the re-run unblock must be named: {stall}"
        );
        // No policy metadata: the classifier refuses unclassified requests,
        // so the only named request must be marked.
        assert!(
            stall.contains("per_001: fs.modify on money.py (refused by --auto)"),
            "an unclassified request must be marked: {stall}"
        );
    }

    #[test]
    fn auto_refusals_are_marked_per_request() {
        let (stall, _session) = stall(serde_json::json!([
            {"id": "per_001", "action": "fs.modify", "resources": ["a.txt"],
             "status": "pending",
             "metadata": {"policy": {"ownerFloor": false, "removesFiles": false}}},
            {"id": "per_002", "action": "fs.delete", "resources": ["b.txt"],
             "status": "pending",
             "metadata": {"policy": {"ownerFloor": false, "removesFiles": false}}},
            {"id": "per_003", "action": "fs.move", "resources": ["c.txt"],
             "status": "pending",
             "metadata": {"policy": {"ownerFloor": false, "removesFiles": false}}},
            {"id": "per_004", "action": "patch", "resources": ["d.txt"],
             "status": "pending",
             "metadata": {"policy": {"ownerFloor": false, "removesFiles": true}}},
            {"id": "per_005", "action": "shell", "resources": [],
             "status": "pending",
             "metadata": {"policy": {"ownerFloor": true, "removesFiles": false}}},
            {"id": "per_006", "action": "shell", "resources": [], "status": "pending"}
        ]));
        assert!(
            stall.contains("per_001: fs.modify on a.txt")
                && !stall.contains("per_001: fs.modify on a.txt (refused by --auto)"),
            "a classified ordinary request must stay unmarked: {stall}"
        );
        for marked in [
            "per_002: fs.delete on b.txt (refused by --auto)",
            "per_003: fs.move on c.txt (refused by --auto)",
            "per_004: patch on d.txt (refused by --auto)",
            "per_005: shell on no resources named (refused by --auto)",
            "per_006: shell on no resources named (refused by --auto)",
        ] {
            assert!(stall.contains(marked), "{marked} must be marked: {stall}");
        }
        assert!(
            stall.contains("6 pending permission request(s)"),
            "the count must match the live rows: {stall}"
        );
        assert!(
            stall.contains("still refuses the 5 marked one(s)"),
            "the marked count must be named: {stall}"
        );
    }

    #[test]
    fn an_all_approvable_stall_says_auto_approves_them() {
        let (stall, _session) = stall(serde_json::json!([
            {"id": "per_001", "action": "fs.modify", "resources": ["a.txt"],
             "status": "pending",
             "metadata": {"policy": {"ownerFloor": false, "removesFiles": false}}}
        ]));
        assert!(
            !stall.contains("(refused by --auto)"),
            "no request may be marked: {stall}"
        );
        assert!(
            stall.contains("which approves all of them."),
            "the unmarked re-run must be stated plainly: {stall}"
        );
    }

    #[test]
    fn a_stall_without_pending_requests_keeps_the_plain_timeout() {
        let session = session();
        let empty: Vec<u8> = serde_json::json!({"data": [], "hasMore": false})
            .to_string()
            .into_bytes();
        assert!(pending_permission_stall(&Some(empty), &session).is_empty());
        let suspended_only: Vec<u8> = serde_json::json!({
            "data": [{"id": "per_002", "action": "fs.remove", "resources": ["old.txt"],
                      "status": "suspended", "suspendedReason": "shutdown"}],
            "hasMore": false
        })
        .to_string()
        .into_bytes();
        assert!(pending_permission_stall(&Some(suspended_only), &session).is_empty());
        assert!(pending_permission_stall(&None, &session).is_empty());
        let garbage: Vec<u8> = b"not json".to_vec();
        assert!(pending_permission_stall(&Some(garbage), &session).is_empty());
    }
}
