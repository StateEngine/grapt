use super::*;

fn event(data: Value) -> Result<(CaptureCoordinate, DecodedEvent), String> {
    let aggregate = "ses_capture_mail"
        .parse::<grapt_types::SessionId>()
        .map_err(|error| error.to_string())?;
    let coordinate = CaptureCoordinate {
        aggregate: aggregate.clone(),
        seq: grapt_types::EventSeq::new(1),
        fencing: grapt_types::EventFencing::Fenced {
            epoch: grapt_types::WriterEpoch::FIRST,
        },
    };
    let event = DecodedEvent {
        id: "evt_capture_mail"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?,
        aggregate_id: aggregate,
        seq: coordinate.seq,
        created_ms: 1,
        kind: grapt_event::EventKind::SessionInputAdmittedV1,
        canonical_hash: grapt_event::canonical_hash_hex(&data),
        data,
    };
    Ok((coordinate, event))
}

fn delivery() -> Value {
    serde_json::json!({"sessionID":"ses_capture_mail", "inputID":"msg_operation_result",
        "input":{"type":"operation-delivery", "operationID":"op_result", "subagentOperationID":"subagent_result",
        "claimID":"claim_result", "generation":1, "settlement":"failed", "projection":"child failed"}})
}

#[test]
fn operation_delivery_advances_capture_without_becoming_user_input() -> Result<(), String> {
    let (coordinate, event) = event(delivery())?;
    let expected = coordinate.clone();
    let result = map_event(coordinate, event).map_err(|error| format!("{error:?}"))?;
    assert!(matches!(result, CapturePageRow::Skipped { coordinate, .. } if coordinate == expected));
    Ok(())
}

#[test]
fn malformed_operation_delivery_is_corruption_instead_of_a_skip() -> Result<(), String> {
    let mut data = delivery();
    data["input"]["generation"] = serde_json::json!(-1);
    let (coordinate, event) = event(data)?;
    assert!(matches!(
        map_event(coordinate, event),
        Err(CapturePageError::CorruptRow { .. })
    ));
    Ok(())
}

#[test]
fn agent_message_is_non_user_and_invalid_sender_metadata_is_corruption() -> Result<(), String> {
    let data = serde_json::json!({"sessionID":"ses_capture_mail", "inputID":"msg_agent_message",
        "input":{"type":"agent-message", "rustAuthorized":true,
            "data":{"senderSessionID":"ses_sender", "callID":"call_sender", "text":"quoted mail"}}});
    let (coordinate, row) = event(data.clone())?;
    let expected = coordinate.clone();
    let result = map_event(coordinate, row).map_err(|error| format!("{error:?}"))?;
    assert!(matches!(result, CapturePageRow::Skipped { coordinate, .. } if coordinate == expected));
    let mut invalid = data;
    invalid["input"]["data"]["senderSessionID"] = serde_json::json!("not-a-session");
    let (coordinate, row) = event(invalid)?;
    assert!(matches!(
        map_event(coordinate, row),
        Err(CapturePageError::CorruptRow { .. })
    ));
    Ok(())
}
