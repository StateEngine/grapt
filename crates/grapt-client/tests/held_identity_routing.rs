//! Held blocks retain their own identities through interleaving and eviction.
use grapt_client::{
    bounds::Bounds,
    contract::EventEnvelope,
    projection::{Applied, SessionView, SliceKind},
    wire,
};
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn frame(
    kind: &str,
    phase: &str,
    message: &str,
    ordinal: u64,
    text: &str,
) -> TestResult<EventEnvelope> {
    let mut value = json!({"id":"evt_identity_routing", "type":format!("session.{kind}.{phase}"), "created":1,
        "data":{"sessionID":"ses_identity_routing", "assistantMessageID":message, "ordinal":ordinal}});
    if phase == "delta" {
        value["data"]["delta"] = json!(text);
    } else {
        value["durable"] =
            json!({"aggregateID":"ses_identity_routing", "seq":ordinal+1, "version":1});
    }
    Ok(EventEnvelope::decode(&value.to_string())?)
}

fn view() -> TestResult<SessionView> {
    Ok(SessionView::new(&wire::session_id("ses_identity_routing")?))
}

#[test]
fn sixteen_held_blocks_route_late_deltas_and_permuted_ends_without_crossing_kinds() -> TestResult {
    let mut view = view()?;
    let mut placer = grapt_client::double::Placer::default();
    let bounds = Bounds::default();
    for ordinal in 0..16 {
        view.apply(
            &placer.place(frame("reasoning", "started", "msg_held", ordinal, "")?),
            bounds,
        )?;
        view.apply(
            &placer.place(frame(
                "reasoning",
                "delta",
                "msg_held",
                ordinal,
                &format!("block-{ordinal}"),
            )?),
            bounds,
        )?;
    }
    view.apply(
        &placer.place(frame("text", "started", "msg_held", 0, "")?),
        bounds,
    )?;
    view.apply(
        &placer.place(frame("text", "delta", "msg_held", 0, "visible answer")?),
        bounds,
    )?;
    for ordinal in (0..16).rev() {
        assert_eq!(
            view.apply(
                &placer.place(frame("reasoning", "delta", "msg_held", ordinal, " Ω")?),
                bounds
            )?,
            Applied::Changed
        );
    }
    for ordinal in [7, 0, 15, 3, 12, 1, 14, 2, 13, 4, 11, 5, 10, 6, 9, 8] {
        view.apply(
            &placer.place(frame("reasoning", "ended", "msg_held", ordinal, "")?),
            bounds,
        )?;
    }
    for (ordinal, slice) in view.slices.iter().take(16).enumerate() {
        assert_eq!(slice.kind, SliceKind::Reasoning);
        assert_eq!(slice.text, format!("block-{ordinal} Ω"));
        assert!(slice.closed);
    }
    assert_eq!(view.slices[16].text, "visible answer");
    assert!(!view.slices[16].closed);
    Ok(())
}

#[test]
fn equal_ordinals_in_distinct_messages_stay_separate_and_unknown_frames_change_nothing(
) -> TestResult {
    for kind in ["reasoning", "text"] {
        let mut view = view()?;
        let mut placer = grapt_client::double::Placer::default();
        let bounds = Bounds::default();
        for message in ["msg_first", "msg_second"] {
            view.apply(
                &placer.place(frame(kind, "started", message, 0, "")?),
                bounds,
            )?;
            view.apply(
                &placer.place(frame(kind, "delta", message, 0, message)?),
                bounds,
            )?;
        }
        view.apply(
            &placer.place(frame(kind, "ended", "msg_second", 0, "")?),
            bounds,
        )?;
        let before: Vec<_> = view
            .slices
            .iter()
            .map(|s| (s.text.clone(), s.closed))
            .collect();
        for (message, phase) in [("msg_second", "delta"), ("msg_unknown", "delta")] {
            assert!(view
                .apply(
                    &placer.place(frame(kind, phase, message, 0, "wrong block")?),
                    bounds
                )
                .is_err());
            assert_eq!(
                view.slices
                    .iter()
                    .map(|s| (s.text.clone(), s.closed))
                    .collect::<Vec<_>>(),
                before
            );
        }
        view.apply(
            &placer.place(frame(kind, "delta", "msg_first", 0, " continues")?),
            bounds,
        )?;
        assert_eq!(view.slices[0].text, "msg_first continues");
        assert_eq!(view.slices[1].text, "msg_second");
    }
    Ok(())
}

#[test]
fn evicted_held_identity_cannot_claim_a_retained_sibling() -> TestResult {
    let bounds = Bounds {
        max_retained_slices: 2,
        ..Bounds::default()
    }
    .validate()?;
    let mut view = view()?;
    let mut placer = grapt_client::double::Placer::default();
    for ordinal in 0..3 {
        view.apply(
            &placer.place(frame("reasoning", "started", "msg_held", ordinal, "")?),
            bounds,
        )?;
        view.apply(
            &placer.place(frame(
                "reasoning",
                "delta",
                "msg_held",
                ordinal,
                &format!("block-{ordinal}"),
            )?),
            bounds,
        )?;
    }
    assert_eq!(view.dropped_slices, 1);
    let before: Vec<_> = view.slices.iter().map(|s| s.text.clone()).collect();
    assert!(view
        .apply(
            &placer.place(frame("reasoning", "delta", "msg_held", 0, "evicted")?),
            bounds
        )
        .is_err());
    assert_eq!(
        view.slices
            .iter()
            .map(|s| s.text.clone())
            .collect::<Vec<_>>(),
        before
    );
    view.apply(
        &placer.place(frame("reasoning", "delta", "msg_held", 1, " retained")?),
        bounds,
    )?;
    assert_eq!(view.slices[0].text, "block-1 retained");
    assert_eq!(view.slices[1].text, "block-2");
    Ok(())
}
