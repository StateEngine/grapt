use super::*;
const SES: &str = "ses_000000000001";
fn bounds() -> Bounds {
    Bounds::PRODUCTION
}
fn sid(raw: &str) -> crate::wire::SessionWireId {
    crate::wire::session_id(raw).unwrap_or_else(|error| panic!("fixture id: {error}"))
}
fn event(member: EventMember, data: serde_json::Value) -> EventEnvelope {
    let mut row =
        serde_json::json!({"id":"evt_feedback","type":member.as_str(),"created":1,"data":data});
    if member.requires_durable() {
        row["durable"] = serde_json::json!({"aggregateID":SES,"seq":1,"version":1});
    }
    EventEnvelope::decode(&row.to_string())
        .unwrap_or_else(|error| panic!("fixture envelope: {error}"))
}
use serde_json::json;
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn local_capacity_looking_message_does_not_reclassify_a_provider_retry() -> Result {
    let mut view = SessionView::new(&sid(SES));
    let provider_retry = event(
        EventMember::SessionStatus,
        json!({"sessionID":SES,"status":{
            "type":"retry", "attempt":2, "message":"Waiting for capacity (waited 37s)", "next":1000,
            "action":{"reason":"rate_limited", "provider":"fixture", "title":"Wait",
                "message":"Wait for the provider", "label":"Wait"}
        }}),
    );
    view.apply(&provider_retry, bounds())?;
    assert!(
        matches!(view.feedback, Some(Feedback::Retry { attempt: 2, .. })),
        "display text overrode the producer's closed retry reason"
    );
    Ok(())
}

#[test]
fn retry_is_live_not_terminal_and_idle_cannot_erase_failure() -> Result {
    let mut view = SessionView::new(&sid(SES));
    let execution = json!({"sessionID":SES,"executionID":"exe_feedback"});
    view.apply(
        &event(EventMember::ExecutionStarted, execution.clone()),
        bounds(),
    )?;
    let retry = event(
        EventMember::SessionStatus,
        json!({"sessionID":SES,"status":{"type":"retry","attempt":2,"message":"HTTP429 temporarily busy","next":123,"action":{"reason":"rate_limited","provider":"fixture","title":"Wait","message":"Wait for the service","label":"Wait"}}}),
    );
    view.apply(&retry, bounds())?;
    assert!(matches!(view.execution, ExecutionState::Running { .. }));
    assert_eq!(
        view.feedback,
        Some(Feedback::Retry {
            attempt: 2,
            message: "HTTP429 temporarily busy".into(),
            action: Some("Wait for the service".into())
        })
    );
    let mut foreign = retry.clone();
    foreign.session_id = Some(sid("ses_foreign"));
    let held = view.clone();
    view.apply(&foreign, bounds())?;
    assert_eq!(view, held);
    let failed = event(
        EventMember::ExecutionFailed,
        json!({"sessionID":SES,"executionID":"exe_feedback","error":{"type":"ExecutionFailed","message":"Inspect the Session before retrying."}}),
    );
    view.apply(&failed, bounds())?;
    let notice = view.feedback.clone();
    view.apply(
        &event(EventMember::SessionIdle, json!({"sessionID":SES})),
        bounds(),
    )?;
    assert_eq!(view.feedback, notice);
    view.apply(
        &event(
            EventMember::ExecutionStarted,
            json!({"sessionID":SES,"executionID":"exe_next"}),
        ),
        bounds(),
    )?;
    assert_eq!(view.feedback, None);
    view.apply(&event(EventMember::SessionStatus,json!({"sessionID":SES,"status":{"type":"retry","attempt":0,"message":"busy","next":1}})),bounds())?;
    view.invalidate_live_quiescence();
    assert_eq!(view.feedback, None);
    Ok(())
}

#[test]
fn retry_details_are_bounded_and_success_clears_them() -> Result {
    let mut view = SessionView::new(&sid(SES));
    let retry = event(
        EventMember::SessionStatus,
        json!({"sessionID":SES,"status":{"type":"retry","attempt":0,"message":"中".repeat(1000),"next":1}}),
    );
    view.apply(&retry, bounds())?;
    let Some(Feedback::Retry { message, .. }) = &view.feedback else {
        return Err("missing retry".into());
    };
    assert!(message.len() <= 2051);
    assert!(message.ends_with('…'));
    view.apply(
        &event(
            EventMember::ExecutionSucceeded,
            json!({"sessionID":SES,"executionID":"exe_feedback"}),
        ),
        bounds(),
    )?;
    assert_eq!(view.feedback, None);
    Ok(())
}

#[test]
fn tagged_durable_failure_reopens_but_ordinary_system_text_is_not_classified() -> Result {
    let bytes=br#"{"data":[{"type":"system","id":"msg_failure","time":{"created":1},"metadata":{"executionFailure":true},"text":"Execution failed: inspect the Session."}],"cursor":{}}"#;
    let page = crate::wire::decode_messages(bytes)?;
    assert!(matches!(
        &page.messages[0],
        crate::wire::DurableMessage::ExecutionFailure { .. }
    ));
    let mut view = SessionView::new(&sid(SES));
    view.apply_durable(&page.messages[0], bounds())?;
    assert!(matches!(view.feedback, Some(Feedback::Failed { .. })));
    assert_eq!(view.slices.len(), 1);
    view.apply_durable(
        &crate::wire::DurableMessage::User {
            text: "Continue from the results above".into(),
        },
        bounds(),
    )?;
    assert_eq!(view.feedback, None);
    view.apply_durable(
        &crate::wire::DurableMessage::System {
            text: "HTTP429 quota exceeded (quoted text)".into(),
        },
        bounds(),
    )?;
    assert_eq!(view.feedback, None);
    Ok(())
}

#[test]
fn a_provider_slot_wait_is_not_a_retry_and_ends_on_busy_or_disconnect() -> Result {
    let mut view = SessionView::new(&sid(SES));
    let waiting = event(
        EventMember::SessionStatus,
        json!({"sessionID":SES,"status":{
            "type":"retry","attempt":0,"message":"Waiting for provider slot","next":1,
            "action":{"reason":"provider_slot","provider":"fixture","title":"Waiting for provider slot","message":"Wait for capacity; Esc cancels.","label":"Wait"}
        }}),
    );
    for disconnect in [false, true] {
        view.apply(&waiting, bounds())?;
        assert!(matches!(
            view.feedback,
            Some(Feedback::WaitingForProviderSlot { .. })
        ));
        if disconnect {
            view.invalidate_live_quiescence();
        } else {
            view.apply(
                &event(
                    EventMember::SessionStatus,
                    json!({"sessionID":SES,"status":{"type":"busy"}}),
                ),
                bounds(),
            )?;
        }
        assert_eq!(view.feedback, None);
    }
    Ok(())
}
