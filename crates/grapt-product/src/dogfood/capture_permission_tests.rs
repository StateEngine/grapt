use super::*;

fn project(kind: grapt_event::EventKind, data: Value) -> Result<Value, String> {
    let aggregate = "ses_capture_permission"
        .parse::<grapt_types::SessionId>()
        .map_err(|e| e.to_string())?;
    let coordinate = CaptureCoordinate {
        aggregate: aggregate.clone(),
        seq: grapt_types::EventSeq::new(3),
        fencing: grapt_types::EventFencing::Fenced {
            epoch: grapt_types::WriterEpoch::FIRST,
        },
    };
    let event = DecodedEvent {
        id: "evt_capture_permission"
            .parse()
            .map_err(|e: grapt_types::IdError| e.to_string())?,
        aggregate_id: aggregate,
        seq: coordinate.seq,
        created_ms: 1,
        kind,
        canonical_hash: grapt_event::canonical_hash_hex(&data),
        data,
    };
    match map_event(coordinate, event).map_err(|e| format!("{e:?}"))? {
        CapturePageRow::Event { event, .. } => {
            serde_json::to_value(event).map_err(|e| e.to_string())
        }
        _ => Err("permission event was skipped".into()),
    }
}

#[test]
fn capture_replays_the_stored_preview_and_cancellation_with_exact_coordinates() -> Result<(), String>
{
    let preview = serde_json::json!({"type":"patch", "format":"apply-patch", "patch":"*** Begin Patch\n*** End Patch", "truncated":false});
    let asked = project(
        grapt_event::EventKind::PermissionAskedV2,
        serde_json::json!({
        "id":"per_capture_permission","sessionID":"ses_capture_permission", "agentID":"agt_preview",
        "projectID":"prj_preview", "action":"patch", "resources":["/repo/file"], "save":null,
        "metadata":{"request":{"approvalPreview":preview}}, "source":null,
        "ownerSubject":"subject_preview", "ownerClient":"client_preview"}),
    )?;
    assert_eq!(asked["type"], "permission.asked");
    assert_eq!(asked["data"]["preview"], preview);
    assert_eq!(asked["durable"]["seq"], 3);
    assert_eq!(asked["durable"]["version"], 2);
    for reason in ["interrupted", "approval-unavailable"] {
        let cancelled = project(
            grapt_event::EventKind::PermissionCancelledV1,
            serde_json::json!({
            "sessionID":"ses_capture_permission", "requestID":"per_capture_permission", "reason":reason}),
        )?;
        assert_eq!(cancelled["type"], "permission.cancelled");
        assert_eq!(cancelled["data"]["requestID"], "per_capture_permission");
        assert_eq!(cancelled["durable"]["seq"], 3);
        assert_eq!(cancelled["durable"]["version"], 1);
        assert_eq!(cancelled["data"]["reason"], reason);
    }
    Ok(())
}
