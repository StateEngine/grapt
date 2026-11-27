//! J01 loss-window reproduction - campaign EVENT-STREAM-REPLAY-CURSOR-01.
//!
//! The health board (2026-09-22; diagnosis input, not authority) records the
//! defect class this campaign closes:
//!
//! > Event stream has no replay cursor by design; overflow disconnects -
//! > capture can drop turns.
//!
//! This test reproduces that loss window at the accepted base against the
//! existing bounded volatile hub and proves the stream alone cannot replay
//! events committed while a subscriber was overflowed or absent. The host
//! subscription shares the exact fan-out and overflow policy the HTTP SSE
//! body uses (`sse.rs:467-486` vs `sse.rs:539-543`), so the proof holds for
//! `/api/event` itself. The durable counterpart - the missed events remain
//! committed and are retrievable under a bounded cursor - is the J02/J03
//! contract; the machine-readable scenario lives in
//! `fixtures/event-stream-replay-cursor/`.

use grapt_api::u3a::{ServerConnectedEvent, MAX_SSE_EVENT_BYTES};
use grapt_api::{
    DogfoodEvent, DogfoodEventHub, DogfoodEventSubscription, PublishReceipt, SseError,
    SseLimitValues, SseLimits, WireId, MAX_SSE_FRAME_BYTES,
};

fn connected(suffix: &str) -> ServerConnectedEvent {
    let raw = format!("evt_{suffix}");
    let Some(id) = WireId::parse(raw) else {
        panic!("event identity fixture was invalid: {suffix}");
    };
    ServerConnectedEvent::new(id)
}

fn publish(hub: &DogfoodEventHub, event: &DogfoodEvent) -> PublishReceipt {
    match hub.publish(event) {
        Ok(receipt) => receipt,
        Err(error) => panic!("publish failed: {error}"),
    }
}

fn frame_text(subscription: &DogfoodEventSubscription, what: &str) -> String {
    match subscription.next_frame() {
        Ok(Some(bytes)) => match std::str::from_utf8(&bytes) {
            Ok(text) => text.to_owned(),
            Err(error) => panic!("{what} was not UTF-8: {error}"),
        },
        other => panic!("{what} was not a data frame: {other:?}"),
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_lost_subscriber_never_receives_the_missed_events_through_the_stream() {
    let limits = match SseLimits::try_new(SseLimitValues {
        subscribers: 4,
        pending_events: 2,
        event_bytes: MAX_SSE_EVENT_BYTES,
        frame_bytes: MAX_SSE_FRAME_BYTES,
    }) {
        Ok(limits) => limits,
        Err(error) => panic!("SSE limits failed: {error}"),
    };
    let hub = match DogfoodEventHub::try_new(limits, connected("initial")) {
        Ok(hub) => hub,
        Err(error) => panic!("event hub failed: {error}"),
    };

    // 1. A subscriber connects and presents its connection frame.
    let slow = match hub.subscribe_host() {
        Ok(subscription) => subscription,
        Err(error) => panic!("subscription failed: {error}"),
    };
    let connected_frame = frame_text(&slow, "connection frame");
    assert!(
        connected_frame.contains("evt_initial"),
        "connection frame lost its identity: {connected_frame}"
    );

    // 2. The publisher outruns the bounded queue: two publishes fill it, the
    //    third overflows. Overflow discards even the frames whose receipts
    //    said "delivered" - admission to a bounded queue is not presentation,
    //    and the typed refusal is sticky.
    let first = DogfoodEvent::ServerConnected(connected("first"));
    let second = DogfoodEvent::ServerConnected(connected("second"));
    let third = DogfoodEvent::ServerConnected(connected("third"));
    assert_eq!(
        publish(&hub, &first).delivered,
        1,
        "the first publish must be admitted to the bounded queue"
    );
    assert_eq!(
        publish(&hub, &second).delivered,
        1,
        "the second publish must fill the bounded queue"
    );
    let third_receipt = publish(&hub, &third);
    assert_eq!(
        third_receipt.overflowed, 1,
        "the bounded queue must overflow on the third publish"
    );
    match slow.next_frame() {
        Err(SseError::SubscriberOverflow) => {}
        other => panic!("overflow must surface as the typed refusal: {other:?}"),
    }
    match slow.next_frame() {
        Err(SseError::SubscriberOverflow) => {}
        other => panic!("the typed refusal must be sticky after overflow: {other:?}"),
    }

    // 3. Events committed while the subscriber is absent reach no queue at all.
    let missed_a = DogfoodEvent::ServerConnected(connected("missed_a"));
    let missed_b = DogfoodEvent::ServerConnected(connected("missed_b"));
    assert_eq!(
        publish(&hub, &missed_a),
        PublishReceipt {
            delivered: 0,
            overflowed: 0,
            disconnected: 0
        },
        "an absent subscriber delivers nothing, overflows nothing, and no queue exists to replay"
    );
    assert_eq!(
        publish(&hub, &missed_b),
        PublishReceipt {
            delivered: 0,
            overflowed: 0,
            disconnected: 0
        },
        "a second absent publish still reaches no queue"
    );

    // 4. A reconnect observes only the connection frame and publishes from
    //    that moment on.
    let fresh = match hub.subscribe_host() {
        Ok(subscription) => subscription,
        Err(error) => panic!("reconnect failed: {error}"),
    };
    let late = DogfoodEvent::ServerConnected(connected("late"));
    assert_eq!(
        publish(&hub, &late).delivered,
        1,
        "the reconnect must receive exactly the publish it is present for"
    );
    hub.reconcile();
    let reconnected_frame = frame_text(&fresh, "reconnected connection frame");
    let late_frame = frame_text(&fresh, "late event frame");
    match fresh.next_frame() {
        Ok(None) => {}
        other => panic!("the stream must close after its last drained frame: {other:?}"),
    }

    // 5. The loss window: no frame any stream presented carries a lost event.
    //    `/api/event` alone cannot replay them; only durable re-read can
    //    (the J02 contract).
    assert!(
        late_frame.contains("evt_late"),
        "the reconnect must observe the late publish: {late_frame}"
    );
    for marker in [
        "evt_first",
        "evt_second",
        "evt_third",
        "evt_missed_a",
        "evt_missed_b",
    ] {
        assert!(
            !reconnected_frame.contains(marker) && !late_frame.contains(marker),
            "the volatile stream replayed a lost event: {marker}"
        );
    }

    // 6. Protocol-v2 compatibility: the frames this stream handed over carry no
    //    SSE replay coordinates, so the frozen cursorless contract is intact.
    for frame in [&connected_frame, &reconnected_frame, &late_frame] {
        assert!(
            !frame.contains("\nid:"),
            "v2 frame carries an SSE id: {frame}"
        );
        assert!(
            !frame.contains("\nretry:"),
            "v2 frame carries an SSE retry: {frame}"
        );
        assert!(
            frame.starts_with("event: message\ndata: "),
            "v2 frame grammar changed: {frame}"
        );
    }
    drop(slow);
}
