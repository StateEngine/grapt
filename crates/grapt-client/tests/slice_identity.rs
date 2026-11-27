use grapt_client::{bounds::Bounds, contract::EventEnvelope, projection::SessionView, wire};
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const SESSION: &str = "ses_slice_identity";
const MESSAGE: &str = "msg_original_block";

fn frame(
    kind: &str,
    message: &str,
    ordinal: u64,
    delta: Option<&str>,
) -> TestResult<EventEnvelope> {
    let mut value = json!({
        "id": "evt_slice_identity", "type": format!("session.{kind}.{}", if delta.is_some() { "delta" } else { "started" }),
        "created": 1, "data": {"sessionID": SESSION, "assistantMessageID": message, "ordinal": ordinal},
    });
    if let Some(delta) = delta {
        value["data"]["delta"] = json!(delta);
    } else {
        value["durable"] = json!({"aggregateID": SESSION, "seq": 1, "version": 1});
    }
    Ok(EventEnvelope::decode(&value.to_string())?)
}

fn opened(kind: &str) -> TestResult<SessionView> {
    let mut view = SessionView::new(&wire::session_id(SESSION)?);
    view.apply(&frame(kind, MESSAGE, 0, None)?, Bounds::default())?;
    view.apply(
        &frame(kind, MESSAGE, 0, Some("original Ω"))?,
        Bounds::default(),
    )?;
    Ok(view)
}

fn rejects_mismatch(kind: &str, message: &str, ordinal: u64) -> TestResult {
    let mut view = opened(kind)?;
    let before = view.slices[0].text.clone();
    let result = view.apply(
        &frame(kind, message, ordinal, Some(" belongs elsewhere"))?,
        Bounds::default(),
    );
    assert!(result.is_err(), "delta for a different block was accepted");
    assert_eq!(view.slices.len(), 1);
    assert_eq!(
        view.slices[0].text, before,
        "refused delta changed the existing block"
    );
    Ok(())
}

#[test]
fn text_delta_cannot_attach_to_a_different_message() -> TestResult {
    rejects_mismatch("text", "msg_next_block", 0)
}

#[test]
fn reasoning_delta_cannot_attach_to_a_different_message() -> TestResult {
    rejects_mismatch("reasoning", "msg_next_block", 0)
}

#[test]
fn text_delta_cannot_attach_to_a_different_ordinal() -> TestResult {
    rejects_mismatch("text", MESSAGE, 1)
}

#[test]
fn reasoning_delta_cannot_attach_to_a_different_ordinal() -> TestResult {
    rejects_mismatch("reasoning", MESSAGE, 1)
}

#[test]
fn matching_text_and_reasoning_deltas_extend_their_blocks() -> TestResult {
    for kind in ["text", "reasoning"] {
        let mut view = opened(kind)?;
        view.apply(
            &frame(kind, MESSAGE, 0, Some(" continues"))?,
            Bounds::default(),
        )?;
        assert_eq!(view.slices.len(), 1);
        assert_eq!(view.slices[0].text, "original Ω continues");
    }
    Ok(())
}

fn rejects_unidentified_history(kind: &str) -> TestResult {
    let mut view = SessionView::new(&wire::session_id(SESSION)?);
    let item = if kind == "text" {
        wire::MessageItem::Text("recovered Ω".into())
    } else {
        wire::MessageItem::Reasoning("recovered Ω".into())
    };
    let message = wire::DurableMessage::Assistant {
        items: vec![item],
        finish: None,
        error: None,
        usage: None,
        cache: Box::default(),
        footer: None,
    };
    view.apply_durable(&message, Bounds::default())?;
    assert!(!view.slices[0].closed);
    let result = view.apply(
        &frame(kind, "msg_next_block", 0, Some(" belongs elsewhere"))?,
        Bounds::default(),
    );
    assert!(result.is_err(), "delta for a different block was accepted");
    assert_eq!(view.slices[0].text, "recovered Ω");
    Ok(())
}

#[test]
fn unidentified_history_text_cannot_adopt_an_unrelated_delta() -> TestResult {
    rejects_unidentified_history("text")
}

#[test]
fn unidentified_history_reasoning_cannot_adopt_an_unrelated_delta() -> TestResult {
    rejects_unidentified_history("reasoning")
}

#[test]
fn review_malformed_slice_identity_is_refused() -> TestResult {
    for kind in ["text", "reasoning"] {
        for (key, value) in [
            ("assistantMessageID", json!(null)),
            ("assistantMessageID", json!(7)),
            ("ordinal", json!("invalid")),
            ("ordinal", json!(-1)),
        ] {
            let mut view = opened(kind)?;
            let before = view.slices[0].text.clone();
            let mut raw = json!({
                "id":"evt_malformed_slice", "type":format!("session.{kind}.delta"), "created":2,
                "data":{"sessionID":SESSION,"assistantMessageID":"msg_other_block","ordinal":1,"delta":"unrelated Ω"},
            });
            raw["data"][key] = value;
            let refused = match EventEnvelope::decode(&raw.to_string()) {
                Err(_) => true,
                Ok(frame) => view.apply(&frame, Bounds::default()).is_err(),
            };
            assert!(
                refused,
                "malformed {key} bypassed the {kind} identity guard"
            );
            assert_eq!(view.slices[0].text, before, "refused identity changed text");
        }
    }
    Ok(())
}
