//! session.status frames decode only in their frozen envelope.

use super::*;

/// One conformant session.status frame, per the frozen envelope law.
fn status_payload(status: serde_json::Value) -> String {
    serde_json::json!({
        "id": "evt_1",
        "type": "session.status",
        "created": 1u64,
        "data": {"sessionID": SES, "status": status}
    })
    .to_string()
}

#[test]
fn session_status_frames_decode_with_their_frozen_envelope() {
    for status in [
        serde_json::json!({"type": "idle"}),
        serde_json::json!({"type": "busy"}),
        serde_json::json!({"type": "retry", "attempt": 0, "message": "provider unavailable", "next": 1}),
        serde_json::json!({
            "type": "retry", "attempt": 2, "message": "provider unavailable", "next": 30,
            "action": {"reason": "rate-limit", "provider": "example", "title": "Wait",
                       "message": "try again", "label": "Retry"}
        }),
    ] {
        let envelope = EventEnvelope::decode(&status_payload(status))
            .unwrap_or_else(|error| panic!("conformant frame refused: {error}"));
        assert_eq!(envelope.member, Some(EventMember::SessionStatus));
        assert!(envelope.durable.is_none(), "session.status is live");
        assert_eq!(
            envelope.session_id.as_ref().map(|id| id.spelled()),
            Some(SES)
        );
    }
}

#[test]
fn session_status_refuses_an_unknown_status_variant() {
    match EventEnvelope::decode(&status_payload(serde_json::json!({"type": "paused"}))) {
        Err(ContractError::SessionStatusMalformed) => {}
        other => panic!("an unknown variant must be refused, got {other:?}"),
    }
}

#[test]
fn session_status_refuses_a_missing_or_unparsable_status() {
    let missing = serde_json::json!({
        "id": "evt_1", "type": "session.status", "created": 1u64,
        "data": {"sessionID": SES}
    })
    .to_string();
    match EventEnvelope::decode(&missing) {
        Err(ContractError::SessionStatusMalformed) => {}
        other => panic!("a frame with no status payload must be refused, got {other:?}"),
    }
    // A retry that dropped its required message is not a retry.
    let short = status_payload(serde_json::json!({
        "type": "retry", "attempt": 1, "next": 1
    }));
    match EventEnvelope::decode(&short) {
        Err(ContractError::SessionStatusMalformed) => {}
        other => panic!("a retry missing its message must be refused, got {other:?}"),
    }
}

#[test]
fn session_status_refuses_an_action_missing_its_label() {
    let payload = status_payload(serde_json::json!({
        "type": "retry", "attempt": 1, "message": "provider unavailable", "next": 1,
        "action": {"reason": "rate-limit", "provider": "example", "title": "Wait",
                   "message": "try again"}
    }));
    match EventEnvelope::decode(&payload) {
        Err(ContractError::SessionStatusMalformed) => {}
        other => panic!("an action missing its label must be refused, got {other:?}"),
    }
}

#[test]
fn session_status_refuses_a_durable_block_and_a_missing_created() {
    let durable = serde_json::json!({
        "id": "evt_1", "type": "session.status", "created": 1u64,
        "durable": {"aggregateID": SES, "seq": 1, "version": 1},
        "data": {"sessionID": SES, "status": {"type": "idle"}}
    })
    .to_string();
    assert_eq!(
        EventEnvelope::decode(&durable),
        Err(ContractError::DurableForbidden {
            member: "session.status"
        })
    );
    let no_created = serde_json::json!({
        "id": "evt_1", "type": "session.status",
        "data": {"sessionID": SES, "status": {"type": "idle"}}
    })
    .to_string();
    assert_eq!(
        EventEnvelope::decode(&no_created),
        Err(ContractError::CreatedMissing {
            member: "session.status"
        })
    );
}

#[test]
fn session_status_refuses_a_frame_without_a_session() {
    // A status edge with no session cannot be attributed to a view; folding
    // it into whichever session happened to be open would report one
    // session's quiescence for another.
    let payload = serde_json::json!({
        "id": "evt_1", "type": "session.status", "created": 1u64,
        "data": {"status": {"type": "idle"}}
    })
    .to_string();
    match EventEnvelope::decode(&payload) {
        Err(ContractError::SessionStatusMalformed) => {}
        other => panic!("a session-less status frame must be refused, got {other:?}"),
    }
}

#[test]
fn session_status_refuses_a_non_string_session_identity() {
    // the reviewer SQ-H4: the presence check
    // admitted `null` and numbers, which decoded to `session_id: None` and
    // let a fold mutate an unrelated view. The identity is required and
    // typed: the same wire helper that refuses `sesABC` refuses these.
    for (name, session_id) in [
        ("null", serde_json::Value::Null),
        ("numeric", serde_json::json!(42)),
        ("object", serde_json::json!({"type": "session"})),
    ] {
        let payload = serde_json::json!({
            "id": "evt_1", "type": "session.status", "created": 1u64,
            "data": {"sessionID": session_id, "status": {"type": "idle"}}
        })
        .to_string();
        match EventEnvelope::decode(&payload) {
            Err(ContractError::SessionStatusMalformed) => {}
            other => panic!("{name} session identity must be refused, got {other:?}"),
        }
    }
    for raw in ["sesABC", "ses_", "session_000000000001"] {
        let payload = serde_json::json!({
            "id": "evt_1", "type": "session.status", "created": 1u64,
            "data": {"sessionID": raw, "status": {"type": "idle"}}
        })
        .to_string();
        match EventEnvelope::decode(&payload) {
            Err(ContractError::SessionStatusMalformed) => {}
            other => panic!("{raw} session identity must be refused, got {other:?}"),
        }
    }
}
