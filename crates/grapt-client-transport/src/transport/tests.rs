//! Witnesses for the egress seam and the SSE reader.
//!
//! The reader is driven with transcripts a real socket would rarely produce
//! and never on demand: a frame delivered one byte at a time, a stream that
//! stops between a `data:` line and its blank line, a frame that exceeds the
//! byte bound. That is the point of keeping it a pure state machine — these
//! are the interleavings that break a reader written against the happy path,
//! and none of them can be scheduled against a live server.

use super::*;
use crate::bounds::Bounds;
use crate::contract::Operation;

/// A valid session identity for witnesses.
///
/// `ses1` and `ses2` — what these witnesses used before this commit — are NOT
/// valid `grapt_types::SessionId` values. Every witness here was passing
/// against an identity the domain refuses, and a `starts_with("ses")` check
/// could not tell. The constants below are real ids.
fn sid(raw: &str) -> crate::wire::SessionWireId {
    crate::wire::session_id(raw)
        .unwrap_or_else(|error| panic!("witness session id `{raw}` must parse: {error}"))
}

/// The canonical witness session identity.
const SES: &str = "ses_000000000001";
fn small_bounds() -> Bounds {
    // Same SHAPE as production, small enough for a witness to reach the
    // overflow branch. Constructed through the same validating constructor, so
    // the branch exercised here is the branch production takes.
    let bounds = Bounds {
        max_frame_bytes: 32,
        max_pending_events: 2,
        ..Bounds::PRODUCTION
    };
    match bounds.validate() {
        Ok(bounds) => bounds,
        Err(error) => panic!("witness bounds must validate: {error}"),
    }
}

fn frame(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

#[test]
fn a_request_resolves_its_path_from_the_frozen_table() {
    let Ok(request) = WireRequest::new(Operation::SessionPrompt, Some(&sid(SES)), None) else {
        panic!("a session-scoped request with a session must address");
    };
    assert_eq!(request.path(), format!("/api/session/{SES}/prompt"));
    assert_eq!(request.operation(), Operation::SessionPrompt);
    assert_eq!(request.body(), None);
}

#[test]
fn a_request_that_cannot_be_addressed_is_refused_at_construction() {
    // Not at call time. A caller that could build an unaddressable request
    // would discover the defect as a server 404, which is indistinguishable
    // from an unavailable route.
    match WireRequest::new(Operation::SessionPrompt, None, None) {
        Err(TransportError::Contract(_)) => {}
        other => panic!("expected a contract refusal, got {other:?}"),
    }
}

#[test]
fn a_body_is_carried_as_bytes_and_nothing_else() {
    // The boundary claim, asserted rather than commented: the only way to put
    // a value on the wire is to encode it first.
    let Ok(request) = WireRequest::new(Operation::SessionCreate, None, None) else {
        panic!("unscoped request must address");
    };
    let request = request.with_body(b"{\"title\":\"x\"}".to_vec());
    assert_eq!(request.body(), Some(&b"{\"title\":\"x\"}"[..]));
}

#[test]
fn a_complete_frame_is_extracted() {
    let mut reader = SseReader::new(small_bounds());
    let Ok(count) = reader.push(&frame("hello")) else {
        panic!("a bounded frame must be accepted");
    };
    assert_eq!(count, 1);
    assert_eq!(reader.take().as_deref(), Some("hello"));
    assert_eq!(reader.take(), None);
}

#[test]
fn a_frame_delivered_one_byte_at_a_time_is_identical_to_one_delivered_whole() {
    // Chunk boundaries are not observable to the caller. A reader that
    // buffered per-push rather than per-frame would pass the whole-frame case
    // and fail here.
    let whole = {
        let mut reader = SseReader::new(small_bounds());
        let _ = reader.push(&frame("abc"));
        reader.take()
    };
    let piecemeal = {
        let mut reader = SseReader::new(small_bounds());
        for byte in frame("abc").chars() {
            let mut buffer = [0u8; 4];
            let _ = reader.push(byte.encode_utf8(&mut buffer));
        }
        reader.take()
    };
    assert_eq!(whole, piecemeal);
    assert_eq!(whole.as_deref(), Some("abc"));
}

#[test]
fn a_stream_that_stops_mid_frame_yields_nothing_rather_than_a_partial() {
    // The failure this prevents is a half-frame reaching the decoder, which
    // would surface as a contract violation attributed to the server.
    let mut reader = SseReader::new(small_bounds());
    let Ok(count) = reader.push("data: incomplete\n") else {
        panic!("a partial frame must be buffered, not refused");
    };
    assert_eq!(count, 0);
    assert_eq!(reader.take(), None);
    assert!(!reader.overflowed());
}

#[test]
fn crlf_framing_is_accepted() {
    let mut reader = SseReader::new(small_bounds());
    let Ok(count) = reader.push("data: x\r\n\r\n") else {
        panic!("CRLF framing must be accepted");
    };
    assert_eq!(count, 1);
    assert_eq!(reader.take().as_deref(), Some("x"));
}

#[test]
fn multi_line_data_is_joined_with_newlines_per_the_grammar() {
    let mut reader = SseReader::new(small_bounds());
    let Ok(_) = reader.push("data: a\ndata: b\n\n") else {
        panic!("multi-line data must be accepted");
    };
    assert_eq!(reader.take().as_deref(), Some("a\nb"));
}

#[test]
fn a_frame_without_a_data_line_is_not_counted() {
    let mut reader = SseReader::new(small_bounds());
    let Ok(count) = reader.push(":heartbeat\n\n") else {
        panic!("a comment frame must be tolerated");
    };
    assert_eq!(count, 0, "a comment is not an event");
    assert_eq!(reader.take(), None);
}

#[test]
fn completed_bursts_release_spare_capacity_without_losing_queued_or_partial_frames(
) -> Result<(), TransportError> {
    let mut reader = SseReader::new(Bounds::PRODUCTION);
    let payload = "中文 👩\u{200d}💻 e\u{301}".repeat(1_000);
    let large = format!("data: {payload}\r\n\r\n");
    for part in large.split_inclusive(' ') {
        reader.push(part)?;
    }
    assert!(reader.buffer.is_empty());
    assert!(reader.buffer.capacity() <= 8_192);
    reader.push("data: second\n\ndata: par")?;
    assert_eq!(reader.take().as_deref(), Some(payload.as_str()));
    assert_eq!(reader.take().as_deref(), Some("second"));
    assert_eq!(reader.take(), None);
    assert_eq!(reader.buffer, b"data: par");
    let capacity = reader.buffer.capacity();
    reader.push("tial\n\n")?;
    assert_eq!(reader.take().as_deref(), Some("partial"));
    assert_eq!(
        reader.buffer.capacity(),
        capacity,
        "small frames reuse capacity"
    );
    assert!(!reader.overflowed());
    Ok(())
}

#[test]
fn incomplete_bursts_keep_their_content_until_completion_or_visible_overflow(
) -> Result<(), TransportError> {
    let bounds = Bounds::PRODUCTION;
    let mut reader = SseReader::new(bounds);
    let partial = format!("data: {}", "x".repeat(bounds.max_frame_bytes / 2));
    reader.push(&partial)?;
    assert_eq!(reader.buffer, partial.as_bytes());
    assert!(reader.buffer.capacity() > 8_192);
    assert_eq!(reader.take(), None);
    assert_eq!(
        reader.push(&"x".repeat(bounds.max_frame_bytes)),
        Err(TransportError::FrameTooLarge {
            bound: bounds.max_frame_bytes
        })
    );
    assert!(reader.buffer.is_empty());
    assert!(reader.buffer.capacity() <= 8_192);
    assert!(reader.overflowed());
    reader.push(&format!(":{}\n\n", "x".repeat(32_768)))?;
    assert_eq!(reader.take(), None);
    assert!(reader.buffer.is_empty());
    assert!(reader.buffer.capacity() <= 8_192);
    assert!(
        reader.overflowed(),
        "releasing spare capacity preserves sticky overflow"
    );
    Ok(())
}

#[test]
fn an_oversized_frame_is_refused_before_allocation_and_is_sticky() {
    let bounds = small_bounds();
    let mut reader = SseReader::new(bounds);
    let oversized = frame(&"x".repeat(bounds.max_frame_bytes * 2));
    match reader.push(&oversized) {
        Err(TransportError::FrameTooLarge { bound }) => {
            assert_eq!(bound, bounds.max_frame_bytes);
        }
        other => panic!("an oversized frame must be refused, got {other:?}"),
    }
    // Sticky and visible. A reader that quietly resumed would leave the
    // projection short exactly one event with nothing saying so.
    assert!(
        reader.overflowed(),
        "overflow must remain observable after the refusal"
    );
    assert_eq!(reader.take(), None);
}

#[test]
fn the_pending_count_bound_overflows_visibly_rather_than_dropping_quietly() {
    // The other bound, and the other direction: this one is reached by many
    // small frames rather than one large one. A count-bounded queue holding
    // unbounded values would not be bounded, and a byte-bounded queue with no
    // count bound would not be either.
    let bounds = small_bounds();
    let mut reader = SseReader::new(bounds);
    for index in 0..(bounds.max_pending_events + 2) {
        let Ok(_) = reader.push(&frame(&format!("e{index}"))) else {
            panic!("each individual frame is within the byte bound");
        };
    }
    assert!(
        reader.overflowed(),
        "exceeding the pending bound must be visible"
    );
    let mut drained = 0;
    while reader.take().is_some() {
        drained += 1;
    }
    assert_eq!(
        drained, bounds.max_pending_events,
        "the queue never exceeded its count bound"
    );
}

#[test]
fn a_healthy_reader_never_reports_overflow() {
    // The positive control. Every bound witness above asserts a refusal, and a
    // reader that refused everything would satisfy all of them.
    let mut reader = SseReader::new(small_bounds());
    let Ok(count) = reader.push(&frame("ok")) else {
        panic!("a small frame must be accepted");
    };
    assert_eq!(count, 1);
    assert!(!reader.overflowed());
}

#[test]
fn a_stream_ending_is_an_outcome_and_not_an_error() {
    // Asserted structurally: there is no `TransportError` variant meaning
    // "closed", so a transport cannot report an ordinary end as a failure.
    let closed = FramePoll::Closed;
    assert_ne!(closed, FramePoll::Idle);
    match closed {
        FramePoll::Closed => {}
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn success_is_the_2xx_family_and_nothing_else() {
    for status in [200u16, 201, 204, 299] {
        assert!(WireResponse {
            status,
            body: Vec::new()
        }
        .is_success());
    }
    for status in [100u16, 199, 300, 400, 404, 500] {
        assert!(!WireResponse {
            status,
            body: Vec::new()
        }
        .is_success());
    }
}

#[test]
fn split_line_scans_preserve_unicode_crlf_and_multiple_frames() {
    let source = ":heartbeat\r\n\r\ndata: 中👩🏽‍💻\r\ndata: next\r\n\r\ndata: last\n\n";
    for split in 0..=source.len() {
        let mut reader = SseReader::new(Bounds::PRODUCTION);
        let first = reader.push_bytes(&source.as_bytes()[..split]);
        let second = reader.push_bytes(&source.as_bytes()[split..]);
        assert!(first.is_ok() && second.is_ok(), "split {split}");
        assert_eq!(reader.take().as_deref(), Some("中👩🏽‍💻\nnext"));
        assert_eq!(reader.take().as_deref(), Some("last"));
        assert_eq!(reader.take(), None);
        assert!(!reader.overflowed());
    }
}

#[test]
fn line_scans_keep_exact_frame_and_queue_bounds_across_chunks() {
    let bounds = small_bounds();
    let exact = frame(&"x".repeat(bounds.max_frame_bytes - "data: \n\n".len()));
    for split in 0..=exact.len() {
        let mut reader = SseReader::new(bounds);
        assert!(reader.push(&exact[..split]).is_ok());
        assert!(reader.push(&exact[split..]).is_ok());
        assert!(reader.take().is_some());
        assert!(!reader.overflowed());
    }
    let mut reader = SseReader::new(bounds);
    assert_eq!(reader.push(&frame("ok")), Ok(1));
    assert!(reader.push("data: ").is_ok());
    assert_eq!(
        reader.push(&"中".repeat(bounds.max_frame_bytes)),
        Err(TransportError::FrameTooLarge {
            bound: bounds.max_frame_bytes
        })
    );
    assert_eq!(reader.take().as_deref(), Some("ok"));
    assert!(reader.overflowed());
    assert!(reader.buffer.is_empty());

    let mut reader = SseReader::new(bounds);
    let frames = frame("one") + &frame("two") + &frame("three");
    assert_eq!(reader.push(&frames), Ok(bounds.max_pending_events));
    assert!(reader.overflowed());
    assert_eq!(reader.take().as_deref(), Some("one"));
    assert_eq!(reader.take().as_deref(), Some("two"));
    assert_eq!(reader.take(), None);
}

#[test]
fn mcp_requests_carry_the_selected_session_without_changing_other_catalog_queries() {
    for operation in [Operation::McpList, Operation::McpInspect] {
        let request = WireRequest::new(operation, Some(&sid(SES)), None)
            .unwrap_or_else(|error| panic!("MCP request must address: {error}"));
        assert_eq!(request.query(), &[("session".to_owned(), SES.to_owned())]);
        let unselected = WireRequest::new(operation, None, None)
            .unwrap_or_else(|error| panic!("unselected MCP request must address: {error}"));
        assert!(unselected.query().is_empty());
    }
    let project = WireRequest::new(Operation::ProjectCurrent, Some(&sid(SES)), None)
        .unwrap_or_else(|error| panic!("project request must address: {error}"));
    assert!(project.query().is_empty());
}
