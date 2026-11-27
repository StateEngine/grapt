//! The J03 stream journeys of EVENT-STREAM-REPLAY-CURSOR-01, split out of
//! `capture_tests.rs` by the module-size ratchet (the zero-exception baseline
//! of `no_god_files`): a pure move, no behaviour change. Declared as
//! `mod journey` from the capture test module, so the fixture helpers above
//! the split stay reachable through that module's own glob import.

use super::*;
#[tokio::test]
async fn the_stream_opens_with_the_connected_frame() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::new());
    port.push_page(page_fixture(vec![], None));
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        content_type.as_deref(),
        Some("text/event-stream"),
        "the content type was {content_type:?}"
    );
    let start_token = CaptureCursor::start(CaptureInterest::All).encode();
    let text = frame_text(next_frame_ok(&mut body).await, "connected frame");
    let expected = format!(
            "event: message\ndata: {{\"type\":\"capture.connected\",\"version\":{CAPTURE_PROTOCOL_VERSION},\"interest\":\"all\",\"cursor\":\"{start_token}\""
        );
    assert!(
        text.starts_with(&expected),
        "the connected frame was {text}"
    );
    // The empty replay crosses straight into the live join and waits.
    assert_pending(&mut body, "the open stream").await;
}

#[tokio::test]
async fn replay_pages_cross_to_live_without_a_gap() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::with_hub(Arc::clone(&hub)));
    let continuation =
        match CaptureCursor::new(CaptureInterest::All, vec![coordinate_fixture("ses_a", 1)]) {
            Ok(value) => value,
            Err(error) => panic!("fixture cursor was refused: {error:?}"),
        };
    port.push_page_committing(
        page_fixture(vec![event_row("ses_a", 1)], Some(continuation)),
        vec![],
    );
    // The live frame commits in the same instant the second page is
    // served: that is the gap between the last replay row and the join.
    port.push_page_committing(
        page_fixture(vec![event_row("ses_b", 1)], None),
        vec![(slice_started("ses_b", 1), None)],
    );
    // The wake re-enters the durable replay from the fence; the fenced
    // row is excluded, so the page the double owes is empty.
    port.push_page(page_fixture(vec![], None));
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);

    let connected = frame_text(next_frame_ok(&mut body).await, "connected frame");
    assert!(connected.contains("\"type\":\"capture.connected\""));
    let first = frame_text(next_frame_ok(&mut body).await, "first replay row");
    assert!(first.contains("evt_ses_a_1"));
    let second = frame_text(next_frame_ok(&mut body).await, "second replay row");
    assert!(second.contains("evt_ses_b_1"));
    // The queued live frame for ses_b,1 is fenced: nothing more is
    // presented, and the stream stays open.
    assert_pending(&mut body, "the crossover stream").await;

    hub.invalidate();
    let terminated = frame_text(next_frame_ok(&mut body).await, "terminal frame");
    let (reason, cursor) = terminated_fields(&terminated, "terminal frame");
    assert_eq!(reason, "invalidated");
    let resumed = decode_fixture(&cursor, &CaptureInterest::All);
    let coordinates: Vec<(String, u64)> = resumed
        .coordinates()
        .iter()
        .map(|coordinate| {
            (
                coordinate.aggregate.as_str().to_owned(),
                coordinate.seq.value(),
            )
        })
        .collect();
    assert_eq!(
        coordinates,
        vec![("ses_a".to_owned(), 1), ("ses_b".to_owned(), 1)]
    );
    assert_eof(&mut body, "the terminated stream").await;
}

#[tokio::test]
async fn a_cursor_token_frame_mints_every_64_presented_rows() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::with_hub(Arc::clone(&hub)));
    let mut rows = Vec::new();
    for seq in 1..=32 {
        rows.push(event_row("ses_t1", seq));
    }
    for seq in 1..=32 {
        rows.push(event_row("ses_t2", seq));
    }
    let continuation = match CaptureCursor::new(
        CaptureInterest::All,
        vec![
            coordinate_fixture("ses_t1", 32),
            coordinate_fixture("ses_t2", 32),
        ],
    ) {
        Ok(value) => value,
        Err(error) => panic!("fixture cursor was refused: {error:?}"),
    };
    port.push_page(page_fixture(rows, Some(continuation)));
    port.push_page(page_fixture(vec![event_row("ses_t2", 33)], None));

    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);

    let connected = frame_text(next_frame_ok(&mut body).await, "connected frame");
    assert!(connected.contains("\"type\":\"capture.connected\""));
    for index in 1..=32 {
        let text = frame_text(next_frame_ok(&mut body).await, "first aggregate row");
        assert!(text.contains(&format!("evt_ses_t1_{index}")));
    }
    for index in 1..=32 {
        let text = frame_text(next_frame_ok(&mut body).await, "second aggregate row");
        assert!(text.contains(&format!("evt_ses_t2_{index}")));
    }
    // The 64th presented row minted the explicit token frame.
    let cursor_frame = frame_text(next_frame_ok(&mut body).await, "cursor frame");
    let value = frame_data(&cursor_frame, "cursor frame");
    assert_eq!(
        value.get("type").and_then(|entry| entry.as_str()),
        Some("capture.cursor")
    );
    let minted = match value.get("cursor").and_then(|entry| entry.as_str()) {
        Some(token) => decode_fixture(token, &CaptureInterest::All),
        None => panic!("the cursor frame carries no token"),
    };
    let coordinates: Vec<(String, u64)> = minted
        .coordinates()
        .iter()
        .map(|coordinate| {
            (
                coordinate.aggregate.as_str().to_owned(),
                coordinate.seq.value(),
            )
        })
        .collect();
    assert_eq!(
        coordinates,
        vec![("ses_t1".to_owned(), 32), ("ses_t2".to_owned(), 32)]
    );
    // The 65th row follows the token, not before it.
    let row65 = frame_text(next_frame_ok(&mut body).await, "row 65");
    assert!(row65.contains("evt_ses_t2_33"));
    assert_pending(&mut body, "the token stream").await;

    hub.invalidate();
    let terminated = frame_text(next_frame_ok(&mut body).await, "terminal frame");
    let (_reason, cursor) = terminated_fields(&terminated, "terminal frame");
    let resumed = decode_fixture(&cursor, &CaptureInterest::All);
    let coordinates: Vec<(String, u64)> = resumed
        .coordinates()
        .iter()
        .map(|coordinate| {
            (
                coordinate.aggregate.as_str().to_owned(),
                coordinate.seq.value(),
            )
        })
        .collect();
    assert_eq!(
        coordinates,
        vec![("ses_t1".to_owned(), 32), ("ses_t2".to_owned(), 33)]
    );
    assert_eof(&mut body, "the token stream").await;
}

#[tokio::test]
async fn a_replay_fenced_live_frame_is_presented_exactly_once() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::with_hub(Arc::clone(&hub)));
    // The live frame commits in the same instant the page returns: the
    // replay row and the live frame carry the same coordinate. The wake
    // re-serves the row from the durable page, and EVR3's fence is what
    // keeps the transcript at exactly one presentation.
    port.push_page_committing(
        page_fixture(vec![event_row("ses_f", 1)], None),
        vec![(slice_started("ses_f", 1), None)],
    );
    port.push_page(page_fixture(vec![event_row("ses_f", 1)], None));
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);

    let mut transcript = vec![];
    transcript.push(frame_text(
        next_frame_ok(&mut body).await,
        "connected frame",
    ));
    transcript.push(frame_text(next_frame_ok(&mut body).await, "replay row"));
    // The queued live frame for the same coordinate is fenced.
    assert_pending(&mut body, "the fenced stream").await;
    hub.invalidate();
    transcript.push(frame_text(next_frame_ok(&mut body).await, "terminal frame"));
    assert_eof(&mut body, "the fenced stream").await;

    let presented = transcript
        .iter()
        .filter(|frame| frame.contains("evt_ses_f_1"))
        .count();
    assert_eq!(
        presented, 1,
        "the coordinate must be presented exactly once: {transcript:?}"
    );
}

#[tokio::test]
async fn overflow_terminates_the_stream_with_the_named_reason() {
    let hub = hub_fixture(limits_fixture(2, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::with_hub(Arc::clone(&hub)));
    port.push_page(page_fixture(vec![], None));
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);
    let connected = frame_text(next_frame_ok(&mut body).await, "connected frame");
    assert!(connected.contains("\"type\":\"capture.connected\""));

    // pending = 2: two frames fit, the third overflows the bounded queue.
    assert_eq!(
        publish_receipt(&hub, &slice_started("ses_o", 1)).delivered,
        1
    );
    assert_eq!(
        publish_receipt(&hub, &slice_started("ses_o", 2)).delivered,
        1
    );
    let third = publish_receipt(&hub, &slice_started("ses_o", 3));
    assert_eq!(third.overflowed, 1);

    let terminated = frame_text(next_frame_ok(&mut body).await, "terminal frame");
    let (reason, _cursor) = terminated_fields(&terminated, "terminal frame");
    assert_eq!(reason, "overflow");
    assert!(
        !terminated.contains("evt_ses_o_1"),
        "an overflowed stream presents no queued frame: {terminated}"
    );
    assert_eof(&mut body, "the overflowed stream").await;
}

#[tokio::test]
async fn reconnect_from_the_minted_token_resumes_past_the_fence() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::with_hub(Arc::clone(&hub)));
    port.push_page(page_fixture(
        vec![
            event_row("ses_r", 1),
            event_row("ses_r", 2),
            event_row("ses_r", 3),
        ],
        None,
    ));
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);
    let connected = frame_text(next_frame_ok(&mut body).await, "connected frame");
    assert!(connected.contains("\"type\":\"capture.connected\""));
    for seq in 1..=3 {
        let text = frame_text(next_frame_ok(&mut body).await, "replay row");
        assert!(text.contains(&format!("evt_ses_r_{seq}")));
    }
    assert_pending(&mut body, "the first stream").await;
    hub.invalidate();
    let terminated = frame_text(next_frame_ok(&mut body).await, "terminal frame");
    let (reason, token) = terminated_fields(&terminated, "terminal frame");
    assert_eq!(reason, "invalidated");
    assert_eof(&mut body, "the first stream").await;

    // The reconnect presents only truth strictly after the fence; the
    // double re-serves the fenced row too, which the stream must drop.
    port.push_page(page_fixture(
        vec![event_row("ses_r", 3), event_row("ses_r", 4)],
        None,
    ));
    let uri = format!("{CAPTURE_EVENT_PATH}?cursor={token}");
    let response = open_capture(
        &uri,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);
    let connected = frame_text(next_frame_ok(&mut body).await, "second connected frame");
    let value = frame_data(&connected, "second connected frame");
    assert_eq!(
        value.get("cursor").and_then(|entry| entry.as_str()),
        Some(token.as_str()),
        "the reconnect must name the token it opened with"
    );
    let row = frame_text(next_frame_ok(&mut body).await, "second stream row");
    assert!(
        row.contains("evt_ses_r_4") && !row.contains("evt_ses_r_3"),
        "the fenced row must not be re-presented: {row}"
    );
    assert_pending(&mut body, "the second stream").await;
    hub.invalidate();
    let terminated = frame_text(next_frame_ok(&mut body).await, "second terminal frame");
    let (reason, cursor) = terminated_fields(&terminated, "second terminal frame");
    assert_eq!(reason, "invalidated");
    let resumed = decode_fixture(&cursor, &CaptureInterest::All);
    let coordinates: Vec<(String, u64)> = resumed
        .coordinates()
        .iter()
        .map(|coordinate| {
            (
                coordinate.aggregate.as_str().to_owned(),
                coordinate.seq.value(),
            )
        })
        .collect();
    assert_eq!(coordinates, vec![("ses_r".to_owned(), 4)]);
    assert_eof(&mut body, "the second stream").await;
}

#[tokio::test]
async fn a_restart_preserves_the_same_coordinate_set() {
    async fn run_once() -> Vec<String> {
        let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
        let port = Arc::new(PortDouble::with_hub(Arc::clone(&hub)));
        port.push_page_committing(
            page_fixture(vec![event_row("ses_a", 1), skipped_row("ses_b", 1)], None),
            vec![(slice_started("ses_c", 1), None)],
        );
        // The wake for the live row re-enters the durable replay and
        // presents it from the page, so a restart sees the same frames.
        port.push_page(page_fixture(vec![event_row("ses_c", 1)], None));
        let response = open_capture(
            CAPTURE_EVENT_PATH,
            Some(fixture_principal("delta", "seat-delta")),
            &hub,
            &port,
        )
        .await;
        let (status, _content_type, mut body) = response_parts(response);
        assert_eq!(status, StatusCode::OK);
        let mut transcript = vec![];
        for what in ["connected frame", "event row", "skipped row", "live row"] {
            transcript.push(frame_text(next_frame_ok(&mut body).await, what));
        }
        assert_pending(&mut body, "the run").await;
        hub.invalidate();
        transcript.push(frame_text(next_frame_ok(&mut body).await, "terminal frame"));
        assert_eof(&mut body, "the run").await;
        transcript
    }
    let first = run_once().await;
    let second = run_once().await;
    assert!(!first.is_empty());
    assert_eq!(
        first, second,
        "a restart must present the same coordinates in the same order"
    );
}

#[tokio::test]
async fn a_location_interest_only_presents_its_own_frames() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::with_hub(Arc::clone(&hub)));
    port.push_page(page_fixture(vec![event_row("ses_l", 1)], None));
    // The wake for the scoped frame re-enters the durable replay; the
    // row it names is served from the page, not the hub's bytes.
    port.push_page(page_fixture(vec![event_row("ses_l", 2)], None));
    // The unscoped wake re-enters the replay as well; the empty page owes
    // the interest nothing more, so the stream goes back to pending.
    port.push_page(page_fixture(vec![], None));
    let inside = location_fixture("/work/one");
    let outside = location_fixture("/work/two");
    let uri = format!(
        "{CAPTURE_EVENT_PATH}?location[directory]={}",
        pct("/work/one")
    );
    let response = open_capture(
        &uri,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);
    let connected = frame_text(next_frame_ok(&mut body).await, "connected frame");
    assert!(
        connected.contains("\"interest\":{\"directory\":\"/work/one\"}"),
        "the connected frame must echo the location interest: {connected}"
    );
    let replayed = frame_text(next_frame_ok(&mut body).await, "replay row");
    assert!(replayed.contains("evt_ses_l_1"));
    // A frame scoped to this location is presented.
    assert_eq!(
        publish_at_receipt(&hub, &slice_started("ses_l", 2), &inside).delivered,
        1
    );
    let scoped = frame_text(next_frame_ok(&mut body).await, "scoped row");
    assert!(scoped.contains("evt_ses_l_2"));
    // A frame scoped elsewhere never reaches this subscriber's queue: its
    // truth is never owed to this interest.
    assert_eq!(
        publish_at_receipt(&hub, &slice_started("ses_l", 3), &outside).delivered,
        0,
        "a frame scoped to another location must never be delivered to this interest"
    );
    // An unscoped frame is wake material for a location interest: the
    // durable page, not the hub, applies the location filter, and the fence
    // advances only on rows the page presents. The wake re-enters the
    // replay, the empty page owes nothing, and the stream goes back to
    // pending with the same token.
    assert_eq!(
        publish_receipt(&hub, &slice_started("ses_l", 4)).delivered,
        1
    );
    assert_pending(&mut body, "the location stream").await;
    hub.invalidate();
    let terminated = frame_text(next_frame_ok(&mut body).await, "terminal frame");
    let (_reason, cursor) = terminated_fields(&terminated, "terminal frame");
    let resumed = decode_fixture(&cursor, &interest_fixture("/work/one"));
    let coordinates: Vec<(String, u64)> = resumed
        .coordinates()
        .iter()
        .map(|coordinate| {
            (
                coordinate.aggregate.as_str().to_owned(),
                coordinate.seq.value(),
            )
        })
        .collect();
    // Only coordinates this interest honestly served are in the token.
    assert_eq!(coordinates, vec![("ses_l".to_owned(), 2)]);
    assert_eof(&mut body, "the location stream").await;
}

#[tokio::test]
async fn reconcile_drains_the_queued_frames_then_closes() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::with_hub(Arc::clone(&hub)));
    port.push_page(page_fixture(vec![], None));
    // Each wake re-enters the durable replay and serves its row from a
    // page; the reconcile boundary then drains and closes.
    port.push_page(page_fixture(vec![event_row("ses_q", 1)], None));
    port.push_page(page_fixture(vec![event_row("ses_q", 2)], None));
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);
    let connected = frame_text(next_frame_ok(&mut body).await, "connected frame");
    assert!(connected.contains("\"type\":\"capture.connected\""));
    assert_eq!(
        publish_receipt(&hub, &slice_started("ses_q", 1)).delivered,
        1
    );
    assert_eq!(
        publish_receipt(&hub, &slice_started("ses_q", 2)).delivered,
        1
    );
    hub.reconcile();
    let first = frame_text(next_frame_ok(&mut body).await, "first queued row");
    assert!(first.contains("evt_ses_q_1"));
    let second = frame_text(next_frame_ok(&mut body).await, "second queued row");
    assert!(second.contains("evt_ses_q_2"));
    let terminated = frame_text(next_frame_ok(&mut body).await, "terminal frame");
    let (reason, _cursor) = terminated_fields(&terminated, "terminal frame");
    assert_eq!(reason, "reconciled");
    assert_eof(&mut body, "the reconciled stream").await;
}

#[tokio::test]
async fn invalidate_closes_immediately_without_a_drain() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::with_hub(Arc::clone(&hub)));
    port.push_page(page_fixture(vec![], None));
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);
    let connected = frame_text(next_frame_ok(&mut body).await, "connected frame");
    assert!(connected.contains("\"type\":\"capture.connected\""));
    // One frame is queued; invalidation must not drain it.
    assert_eq!(
        publish_receipt(&hub, &slice_started("ses_i", 1)).delivered,
        1
    );
    hub.invalidate();
    let terminated = frame_text(next_frame_ok(&mut body).await, "terminal frame");
    let (reason, _cursor) = terminated_fields(&terminated, "terminal frame");
    assert_eq!(reason, "invalidated");
    assert!(
        !terminated.contains("evt_ses_i_1"),
        "the queued frame must not drain on invalidation: {terminated}"
    );
    assert_eof(&mut body, "the invalidated stream").await;
}

#[tokio::test]
async fn an_oversized_row_terminates_with_frame_bound_without_advancing_the_fence() {
    let limits = production_limits_fixture();
    let start_token = CaptureCursor::start(CaptureInterest::All).encode();
    let connected_size = encode_sse_fixture(
        &ConnectedFrame {
            frame_type: "capture.connected",
            version: CAPTURE_PROTOCOL_VERSION,
            interest: InterestEcho(CaptureInterest::All),
            cursor: start_token.clone(),
        },
        limits,
    )
    .len();
    let event = slice_started("ses_x", 1);
    let row_size = encode_sse_fixture(
        &ReplayEventFrame {
            frame_type: "capture.event",
            coordinate: coordinate_dto(&coordinate_fixture("ses_x", 1)),
            event: &event,
        },
        limits,
    )
    .len();
    assert!(
        row_size > connected_size,
        "fixture premise: the event frame must outgrow the connected frame"
    );
    // The frame bound sits between the two: control frames fit, the
    // event row does not.
    let bound = connected_size + 1;
    assert!(
        bound < row_size,
        "fixture premise: the event row must not fit"
    );
    let limits = match SseLimits::try_new(SseLimitValues {
        subscribers: 16,
        pending_events: 4,
        event_bytes: MAX_SSE_EVENT_BYTES,
        frame_bytes: bound,
    }) {
        Ok(limits) => limits,
        Err(error) => panic!("SSE limits fixture failed: {error}"),
    };
    let hub = hub_fixture(limits);
    let port = Arc::new(PortDouble::with_hub(Arc::clone(&hub)));
    port.push_page(page_fixture(vec![event_row("ses_x", 1)], None));
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);
    let connected = frame_text(next_frame_ok(&mut body).await, "connected frame");
    assert!(connected.contains("\"type\":\"capture.connected\""));
    let terminated = frame_text(next_frame_ok(&mut body).await, "terminal frame");
    let (reason, cursor) = terminated_fields(&terminated, "terminal frame");
    assert_eq!(reason, "frame-bound");
    // The fence never advanced: the resume token is the start token.
    assert_eq!(cursor, start_token);
    assert_eof(&mut body, "the frame-bound stream").await;
}

#[tokio::test]
async fn a_non_v2_row_between_v2_wakes_is_presented_from_the_durable_page() {
    // EVR6: a non-v2 row committed between two v2 wakes is owed to the
    // reader as a typed skip — presentation comes from the durable page,
    // never from the hub's v2 bytes, so the fence and the wire agree.
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::with_hub(Arc::clone(&hub)));
    port.push_page(page_fixture(vec![], None));
    port.push_page(page_fixture(vec![event_row("ses_n", 1)], None));
    let skip = CapturePageRow::Skipped {
        coordinate: coordinate_fixture("ses_n", 2),
        kind: dto_string_fixture("content.appended"),
    };
    port.push_page(page_fixture(vec![skip, event_row("ses_n", 3)], None));
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);
    let connected = frame_text(next_frame_ok(&mut body).await, "connected frame");
    assert!(connected.contains("\"type\":\"capture.connected\""));

    // First v2 wake: the row is presented from the durable page.
    publish_receipt(&hub, &slice_started("ses_n", 1));
    let first = frame_text(next_frame_ok(&mut body).await, "woken row");
    assert!(
        first.contains("evt_ses_n_1"),
        "the woken row must be presented from the page: {first}"
    );

    // Second v2 wake: the non-v2 row between the wakes is owed as a typed
    // skip before the woken row itself.
    publish_receipt(&hub, &slice_started("ses_n", 3));
    let skipped = frame_text(next_frame_ok(&mut body).await, "skipped row");
    assert!(
        skipped.contains("\"type\":\"capture.skipped\"") && skipped.contains("ses_n"),
        "the non-v2 row must surface as a typed skip: {skipped}"
    );
    let second = frame_text(next_frame_ok(&mut body).await, "second woken row");
    assert!(
        second.contains("evt_ses_n_3"),
        "the woken row must be presented from the page: {second}"
    );

    hub.reconcile();
    let terminated = frame_text(next_frame_ok(&mut body).await, "terminal frame");
    let (reason, cursor) = terminated_fields(&terminated, "terminal frame");
    assert_eq!(reason, "reconciled");
    let live_coordinates: Vec<(String, u64)> = decode_fixture(&cursor, &CaptureInterest::All)
        .coordinates()
        .iter()
        .map(|coordinate| {
            (
                coordinate.aggregate.as_str().to_owned(),
                coordinate.seq.value(),
            )
        })
        .collect();
    assert_eq!(live_coordinates, vec![("ses_n".to_owned(), 3)]);
    assert_eof(&mut body, "the wake stream").await;

    // A pure replay of the same truth presents the same coordinate set:
    // the event, the skip, and the second event, in order.
    let skip_again = CapturePageRow::Skipped {
        coordinate: coordinate_fixture("ses_n", 2),
        kind: dto_string_fixture("content.appended"),
    };
    port.push_page(page_fixture(
        vec![event_row("ses_n", 1), skip_again, event_row("ses_n", 3)],
        None,
    ));
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    let (status, _content_type, mut body) = response_parts(response);
    assert_eq!(status, StatusCode::OK);
    let _connected = frame_text(next_frame_ok(&mut body).await, "second connected frame");
    let rows = [
        frame_text(next_frame_ok(&mut body).await, "replay row one"),
        frame_text(next_frame_ok(&mut body).await, "replay skip"),
        frame_text(next_frame_ok(&mut body).await, "replay row two"),
    ];
    assert!(rows[0].contains("evt_ses_n_1"), "{rows:?}");
    assert!(rows[1].contains("\"type\":\"capture.skipped\""), "{rows:?}");
    assert!(rows[2].contains("evt_ses_n_3"), "{rows:?}");
    hub.invalidate();
    let terminated = frame_text(next_frame_ok(&mut body).await, "second terminal frame");
    let (_reason, cursor) = terminated_fields(&terminated, "second terminal frame");
    let replay_coordinates: Vec<(String, u64)> = decode_fixture(&cursor, &CaptureInterest::All)
        .coordinates()
        .iter()
        .map(|coordinate| {
            (
                coordinate.aggregate.as_str().to_owned(),
                coordinate.seq.value(),
            )
        })
        .collect();
    assert_eq!(
        live_coordinates, replay_coordinates,
        "the live join and the pure replay must mint the same coordinate set"
    );
    assert_eof(&mut body, "the replay stream").await;
}
