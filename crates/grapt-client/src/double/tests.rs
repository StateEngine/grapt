//! Witnesses for the contract double itself.
//!
//! The double is test infrastructure, so it needs its own witnesses for the
//! same reason a measuring instrument needs calibration: if it emitted frames
//! the frozen record forbids, every client law proved against it would be
//! proving conformance to the wrong contract. In particular it is held to the
//! SAME durable/live asymmetry the client enforces, so a bug in one cannot
//! mask a bug in the other.

use super::*;
use crate::contract::EventEnvelope;
use crate::wire::Spelled;

/// Create a session on a double, refusing to continue if the bound refused.
fn seed(double: &mut ContractDouble, title: &str) -> crate::wire::SessionWireId {
    double
        .create_session(title)
        .unwrap_or_else(|| panic!("witness session creation must not hit the bound"))
}

/// Raise an ask, refusing to continue if the ask bound refused.
#[allow(dead_code)]
fn seed_ask(
    double: &mut ContractDouble,
    session: &crate::wire::SessionWireId,
    title: &str,
) -> String {
    double
        .ask_permission(session, title)
        .unwrap_or_else(|| panic!("witness permission ask must not hit the bound"))
}

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
const SES_MISSING: &str = "ses_0000000000ff";

/// Drain every frame the double currently has, decoding each through the
/// client's own contract decoder.
fn drain(double: &mut ContractDouble) -> Vec<EventEnvelope> {
    let mut out = Vec::new();
    loop {
        match double.poll_frame() {
            Ok(FramePoll::Frame(frame) | FramePoll::Positioned { data: frame, .. }) => {
                match EventEnvelope::decode(&frame) {
                    Ok(envelope) => out.push(envelope),
                    Err(error) => {
                        panic!("the double emitted a frame the contract refuses: {error}")
                    }
                }
            }
            Ok(FramePoll::Idle) | Ok(FramePoll::Closed) => break,
            Ok(FramePoll::Gap) => panic!("the double unexpectedly lost live data"),
            Err(error) => panic!("the double must not fail a poll: {error}"),
        }
    }
    out
}

#[test]
fn every_frame_the_double_emits_satisfies_the_frozen_envelope() {
    // The calibration. `decode` enforces the durable/live asymmetry in both
    // directions, so this is a real check rather than a formality.
    let mut double = ContractDouble::new();
    let session = seed(&mut double, "first");
    double.run_turn(&session, "hello", "hello there");
    let frames = drain(&mut double);
    assert!(frames.len() > 5, "the turn produced frames");
    for frame in &frames {
        if let Some(member) = frame.member {
            assert_eq!(
                frame.durable.is_some(),
                member.requires_durable(),
                "{} durable presence",
                member.as_str()
            );
        }
    }
}

#[test]
fn the_double_serves_frozen_members_the_engine_has_no_emitter_for() {
    // The reason this double exists rather than a thin wrapper over U3a.
    let mut double = ContractDouble::new();
    let session = seed(&mut double, "first");
    double.run_turn(&session, "hello", "hi");
    let frames = drain(&mut double);
    let members: Vec<_> = frames.iter().filter_map(|f| f.member).collect();
    for unserved in [
        EventMember::SessionCreated,
        EventMember::ExecutionStarted,
        EventMember::SessionIdle,
    ] {
        assert!(
            !unserved.is_served_today(),
            "{} is one of the three the engine does not emit",
            unserved.as_str()
        );
        assert!(
            members.contains(&unserved),
            "the double must serve {} anyway",
            unserved.as_str()
        );
    }
}

#[test]
fn durable_sequences_are_contiguous_from_one() {
    // The client's gap detector is only meaningful if a conformant server
    // really does produce contiguous sequences.
    let mut double = ContractDouble::new();
    let session = seed(&mut double, "first");
    double.run_turn(&session, "hello", "a b");
    let frames = drain(&mut double);
    let seqs: Vec<u64> = frames
        .iter()
        .filter_map(|f| f.durable.as_ref().map(|d| d.seq))
        .collect();
    assert!(!seqs.is_empty());
    for (index, seq) in seqs.iter().enumerate() {
        assert_eq!(
            *seq,
            index as u64 + 1,
            "durable sequences must be contiguous from 1"
        );
    }
}

#[test]
fn the_double_is_deterministic_to_the_byte() {
    // Two independent doubles driven identically must produce identical
    // frames. Without this every snapshot witness would be flaky, and a
    // double reaching for a clock or randomness would also trip the
    // workspace's disallowed_methods gate.
    let render = || {
        let mut double = ContractDouble::new();
        let session = seed(&mut double, "first");
        double.run_turn(&session, "hello", "same input");
        let mut frames = Vec::new();
        while let Ok(FramePoll::Frame(frame)) = double.poll_frame() {
            frames.push(frame);
        }
        frames
    };
    assert_eq!(render(), render());
}

#[test]
fn the_re_list_returns_durable_truth_only() {
    // The re-list is the reconciliation source, so it must decode as the REAL
    // `SessionMessagesResponseDto` and carry durable messages rather than the
    // live decoration the stream sends.
    let mut double = ContractDouble::new();
    let session = seed(&mut double, "first");
    double.run_turn(&session, "hello", "hello");

    let Ok(request) = WireRequest::new(Operation::MessageList, Some(&session), None) else {
        panic!("the re-list must address");
    };
    let Ok(response) = double.call(&request) else {
        panic!("the re-list must answer");
    };
    let page = match crate::wire::decode_messages(&response.body) {
        Ok(page) => page,
        Err(error) => panic!("the re-list must decode as the real DTO: {error}"),
    };
    assert!(!page.messages.is_empty(), "durable history is present");
    assert!(
        page.next.is_some(),
        "every first page offers a cursor, exactly as the product's does"
    );
    assert_eq!(page.unsupported, 0, "every row is renderable");
}

#[test]
fn a_restart_loses_live_state_and_keeps_durable_truth() {
    // The restart law's setup, which is why the double owns a log rather than
    // only a queue.
    let mut double = ContractDouble::new();
    let session = seed(&mut double, "first");
    double.run_turn(&session, "hello", "hello");

    double.restart();
    assert_eq!(
        double.poll_frame(),
        Ok(FramePoll::Closed),
        "the subscription dies"
    );

    let Ok(request) = WireRequest::new(Operation::MessageList, Some(&session), None) else {
        panic!("the re-list must address");
    };
    let Ok(response) = double.call(&request) else {
        panic!("durable truth survives a restart");
    };
    assert!(response.is_success());
    assert!(
        response.body.len() > 2,
        "the durable log is still there after the restart"
    );
}

#[test]
fn the_close_fault_ends_the_subscription() {
    let mut double = ContractDouble::new();
    let session = seed(&mut double, "first");
    double.run_turn(&session, "hello", "hi");
    double.set_fault(Fault::CloseStream);
    assert_eq!(double.poll_frame(), Ok(FramePoll::Closed));
}

#[test]
fn the_skip_fault_withholds_durable_frames_so_a_gap_is_observable() {
    // A lossy transport, reproduced deterministically. This is the shape no
    // live server can be asked to produce on demand.
    let mut double = ContractDouble::new();
    let session = seed(&mut double, "first");
    double.run_turn(&session, "hello", "hi");

    // Consume the connection frame first so the skip lands on content.
    let _ = double.poll_frame();
    double.set_fault(Fault::SkipDurable(2));
    let frames = drain(&mut double);
    let seqs: Vec<u64> = frames
        .iter()
        .filter_map(|f| f.durable.as_ref().map(|d| d.seq))
        .collect();
    assert!(!seqs.is_empty(), "some durable frames still arrive");
    assert!(
        seqs.first().copied().unwrap_or_default() > 1,
        "the withheld frames left a visible hole at the start: {seqs:?}"
    );
}

#[test]
fn a_permission_settles_once_and_a_conflicting_second_reply_is_not_found() {
    // First answer wins; the existing public contract maps an already-settled
    // request onto its permission-not-found shape.
    let mut double = ContractDouble::new();
    let session = seed(&mut double, "first");
    let request_id = seed_ask(&mut double, &session, "write a file");
    let Some(permission_id) = crate::wire::PermissionWireId::parse(request_id.clone()) else {
        panic!("the double minted a typed permission identity");
    };
    assert_eq!(double.permissions(&session).len(), 1);

    let reply = |choice| {
        let Ok(request) = WireRequest::new(
            Operation::SessionPermissionReply,
            Some(&session),
            Some(&request_id),
        ) else {
            panic!("the reply must address");
        };
        request.with_body(
            crate::wire::encode_permission_reply(&permission_id, choice)
                .unwrap_or_else(|error| panic!("permission reply must encode: {error}")),
        )
    };

    let Ok(first) = double.call(&reply(crate::wire::PermissionReplyChoice::Once)) else {
        panic!("the first reply must answer");
    };
    assert!(first.is_success());

    let Ok(second) = double.call(&reply(crate::wire::PermissionReplyChoice::Reject)) else {
        panic!("the second reply must answer");
    };
    assert_eq!(second.status, 404, "the first answer stands");
}

#[test]
fn every_permission_reply_scope_round_trips_without_boolean_collapse() {
    let choices = [
        crate::wire::PermissionReplyChoice::Once,
        crate::wire::PermissionReplyChoice::Session,
        crate::wire::PermissionReplyChoice::Always,
        crate::wire::PermissionReplyChoice::Reject,
    ];
    for choice in choices {
        let mut double = ContractDouble::new();
        let session = seed(&mut double, "reply-vocabulary");
        let request_id = seed_ask(&mut double, &session, "write a file");
        let Some(permission_id) = crate::wire::PermissionWireId::parse(request_id.clone()) else {
            panic!("the double minted a typed permission identity");
        };
        let Ok(request) = WireRequest::new(
            Operation::SessionPermissionReply,
            Some(&session),
            Some(&request_id),
        ) else {
            panic!("the reply must address");
        };
        let body = crate::wire::encode_permission_reply(&permission_id, choice)
            .unwrap_or_else(|error| panic!("permission reply must encode: {error}"));
        let Ok(response) = double.call(&request.with_body(body)) else {
            panic!("the double must answer");
        };
        assert_eq!(response.status, 204, "{choice:?} must reach the route");
        assert_eq!(
            double.permissions(&session)[0].settled,
            Some(choice),
            "the double must retain the exact reply scope"
        );
    }
}

#[test]
fn an_unknown_session_is_a_404_rather_than_an_empty_success() {
    // An empty success would render as "this session has no history", which
    // is a different and much more confusing claim than "no such session".
    let mut double = ContractDouble::new();
    let Ok(request) = WireRequest::new(Operation::SessionGet, Some(&sid(SES_MISSING)), None) else {
        panic!("the request must address");
    };
    let Ok(response) = double.call(&request) else {
        panic!("the double must answer");
    };
    assert_eq!(response.status, 404);
}

#[test]
fn session_create_then_list_round_trips() {
    let mut double = ContractDouble::new();
    let first = seed(&mut double, "first");
    let second = seed(&mut double, "first");
    let Ok(request) = WireRequest::new(Operation::SessionList, None, None) else {
        panic!("the list must address");
    };
    let Ok(response) = double.call(&request) else {
        panic!("the list must answer");
    };
    let rows = match crate::wire::decode_sessions(&response.body) {
        Ok(rows) => rows,
        Err(error) => panic!("the list must decode as the real DTO: {error}"),
    };
    let ids: Vec<String> = rows.iter().map(|r| r.id.as_inner().to_string()).collect();
    assert!(ids.contains(&first.spelled().to_owned()), "{ids:?}");
    assert!(ids.contains(&second.spelled().to_owned()), "{ids:?}");
}

#[test]
fn interrupting_produces_a_started_and_an_interrupted_terminal() {
    let mut double = ContractDouble::new();
    let session = seed(&mut double, "first");
    let _ = drain(&mut double);
    let Ok(request) = WireRequest::new(Operation::SessionInterrupt, Some(&session), None) else {
        panic!("interrupt must address");
    };
    let Ok(response) = double.call(&request) else {
        panic!("interrupt must answer");
    };
    assert!(response.is_success());
    let frames = drain(&mut double);
    let members: Vec<_> = frames.iter().filter_map(|f| f.member).collect();
    assert!(members.contains(&EventMember::ExecutionStarted));
    assert!(members.contains(&EventMember::ExecutionInterrupted));
}

#[test]
fn each_session_and_kind_places_its_own_blocks_from_zero() {
    // Two fresh sessions reuse the same assistant message id, and each turn
    // streams a reasoning block and a text block, each its kind's first.
    let mut double = ContractDouble::new();
    let first = seed(&mut double, "first");
    let second = seed(&mut double, "second");
    double.run_turn(&first, "one", "red green");
    double.run_turn(&second, "two", "blue cyan magenta");
    let mut sent: BTreeMap<(String, String, String, u64), u64> = BTreeMap::new();
    let mut messages = BTreeMap::new();
    let mut deltas = 0;
    for _ in 0..256 {
        let data = match double.poll_frame() {
            Ok(FramePoll::Positioned { data, .. }) => data,
            Ok(FramePoll::Idle) => break,
            other => panic!("a positioned frame: {other:?}"),
        };
        let frame: serde_json::Value =
            serde_json::from_str(&data).unwrap_or_else(|error| panic!("frame: {error}"));
        let kind = frame["type"].as_str().unwrap_or_default().to_owned();
        if !kind.ends_with(".delta") {
            continue;
        }
        let body = &frame["data"];
        let field = |name: &str| body[name].as_str().unwrap_or_default().to_owned();
        messages.insert(field("sessionID"), field("assistantMessageID"));
        let key = (
            field("sessionID"),
            field("assistantMessageID"),
            kind,
            body["ordinal"].as_u64().unwrap_or(u64::MAX),
        );
        let offset = sent.entry(key.clone()).or_insert(0);
        assert_eq!(body["from"].as_u64(), Some(*offset), "{key:?}");
        assert_eq!(key.3, 0, "each kind numbers its own blocks: {key:?}");
        *offset += body["delta"].as_str().map_or(0, str::len) as u64;
        deltas += 1;
    }
    assert!(deltas > 4, "both turns streamed");
    assert_eq!(sent.len(), 4, "two kinds in each of two sessions");
    assert_eq!(
        messages.get(first.spelled()),
        messages.get(second.spelled()),
        "the witness needs the reused message id"
    );
}

#[test]
fn a_page_keeps_every_row_the_product_byte_bound_keeps() -> Result<(), crate::wire::WireError> {
    // Rows sized so a page of `count` fills the bound but for about a
    // kilobyte, counted exactly: a flat framing reserve would push the last
    // row to the next page, where the product keeps it.
    let bound = grapt_api::u3a::U3A_PRODUCTION_LIMITS.page_bytes;
    let limits = crate::wire::limits()?;
    let row = |index: usize, len: usize| {
        crate::wire::assistant_message(
            &format!("msg_{index:012x}"),
            &[crate::wire::MessageItem::Text("x".repeat(len))],
            Some(crate::wire::DurableFinish::Stop),
            None,
            None,
        )
    };
    let overhead = grapt_api::u3a::encode_page(&row(0, 0)?, limits)?.len();
    let count = 100usize;
    let framing = crate::wire::encode_messages(Vec::new(), Some("h-desc-0"))?.len() + 8;
    let len = (bound - framing - count - 1_000) / count - overhead;
    let rows = (0..=count)
        .map(|index| row(index, len))
        .collect::<Result<Vec<_>, _>>()?;
    let indices: Vec<usize> = (0..=count).rev().collect();
    let kept = history_pages::rows_within_page_bytes(&rows, &indices, false)?;
    assert_eq!(kept, count, "every row that fits stays on the page");
    let page = crate::wire::encode_messages(rows[..kept].to_vec(), Some("h-desc-1"))?;
    assert!(page.len() <= bound, "and the page is within the bound");
    Ok(())
}
