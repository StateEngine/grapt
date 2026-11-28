//! A delta retained from before a snapshot is superseded only by the exact
//! block the snapshot holds durably: anything else on that row is a gap.
use super::*;
use crate::projection::Applied;

/// The view a snapshot installs for one finished assistant row: a reasoning
/// block, then a text block, each first of its kind.
fn held() -> Result<SessionView, Box<dyn std::error::Error>> {
    let session = crate::wire::session_id("ses_000000000001")?;
    let row = crate::wire::assistant_message(
        "msg_000000000001",
        &[
            crate::wire::MessageItem::Reasoning("considering".into()),
            crate::wire::MessageItem::Text("done".into()),
        ],
        Some(crate::wire::DurableFinish::Stop),
        None,
        None,
    )?;
    let page = crate::wire::decode_messages(&crate::wire::encode_messages(vec![row], None)?)?;
    Ok(history::tail_window(&session, &page, Bounds::PRODUCTION)?)
}

fn delta(
    kind: &str,
    ordinal: u64,
    from: u64,
    text: &str,
) -> Result<EventEnvelope, Box<dyn std::error::Error>> {
    Ok(EventEnvelope::decode(
        &serde_json::json!({
            "id": "evt_retained", "type": format!("session.{kind}.delta"), "created": 1,
            "data": {"sessionID": "ses_000000000001", "assistantMessageID": "msg_000000000001",
                "ordinal": ordinal, "from": from, "delta": text}
        })
        .to_string(),
    )?)
}

#[test]
fn a_delta_inside_the_held_block_is_superseded() -> Result<(), Box<dyn std::error::Error>> {
    for (kind, text) in [("reasoning", "consider"), ("text", "done")] {
        let mut view = held()?;
        assert_eq!(
            view.apply(&delta(kind, 0, 0, text)?, Bounds::PRODUCTION),
            Ok(Applied::Ignored),
            "{kind}"
        );
    }
    Ok(())
}

#[test]
fn an_absent_block_or_bytes_past_the_held_block_stay_a_gap(
) -> Result<(), Box<dyn std::error::Error>> {
    for (kind, ordinal, from, text) in [
        // An ordinal the durable row does not hold, of either kind.
        ("text", 1, 0, "more"),
        ("reasoning", 1, 0, "more"),
        // The held block, but bytes it never held.
        ("text", 0, 2, "ne and more"),
    ] {
        let mut view = held()?;
        let before = view.slices.clone();
        assert!(
            view.apply(&delta(kind, ordinal, from, text)?, Bounds::PRODUCTION)
                .is_err(),
            "{kind} {ordinal} at {from} must be a gap"
        );
        assert_eq!(view.slices, before, "and change nothing shown");
    }
    Ok(())
}
