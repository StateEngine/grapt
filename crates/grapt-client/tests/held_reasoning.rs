//! The service holds a finished reasoning block uncommitted, so its end
//! arrives only once it is durable, after the next block has opened. The
//! next start must not overlap the held block, and every delta and end must
//! reach the block it names.
use grapt_client::{
    bounds::Bounds,
    contract::EventEnvelope,
    projection::{Applied, SessionView, SliceKind},
    wire,
};
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const SESSION: &str = "ses_held_reasoning";

fn frame(step: &str, ordinal: u64, delta: Option<&str>) -> TestResult<EventEnvelope> {
    let mut value = json!({
        "id": "evt_held_reasoning", "type": format!("session.reasoning.{step}"), "created": 1,
        "data": {"sessionID": SESSION, "assistantMessageID": "msg_held_reasoning", "ordinal": ordinal},
    });
    match delta {
        Some(delta) => value["data"]["delta"] = json!(delta),
        None => value["durable"] = json!({"aggregateID": SESSION, "seq": 1, "version": 1}),
    }
    Ok(EventEnvelope::decode(&value.to_string())?)
}

#[test]
fn a_held_reasoning_block_does_not_refuse_the_next_one() -> TestResult {
    let mut view = SessionView::new(&wire::session_id(SESSION)?);
    let mut placer = grapt_client::double::Placer::default();
    for envelope in [
        frame("started", 0, None)?,
        frame("delta", 0, Some("first"))?,
        // R0 is finished but held: no end yet. R1 opens beside it.
        frame("started", 1, None)?,
        frame("delta", 1, Some("second"))?,
        frame("delta", 0, Some(" late"))?,
        // The held block commits first, then the newer one.
        frame("ended", 0, None)?,
        frame("ended", 1, None)?,
    ] {
        let kind = envelope.kind_raw.clone();
        assert_eq!(
            view.apply(&placer.place(envelope), Bounds::default()),
            Ok(Applied::Changed),
            "a held reasoning block made the stream relist at {kind}"
        );
    }
    let reasoning: Vec<(&str, bool)> = view
        .slices
        .iter()
        .filter(|slice| slice.kind == SliceKind::Reasoning)
        .map(|slice| (slice.text.as_str(), slice.closed))
        .collect();
    assert_eq!(reasoning, vec![("first late", true), ("second", true)]);
    Ok(())
}
