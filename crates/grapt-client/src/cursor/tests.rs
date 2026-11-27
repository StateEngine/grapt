//! Witnesses for durable convergence.
//!
//! Each law here has a positive witness AND a counterexample that breaks it in
//! the direction opposite to the obvious one. A gap check that only caught
//! sequences jumping FORWARD is satisfied by a transport that replays
//! backwards; an idempotence check that only replayed the LAST event is
//! satisfied by a cursor that duplicates everything else.

use super::*;
use crate::contract::EventEnvelope;

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
/// A second, distinct witness session identity.
const SES_OTHER: &str = "ses_000000000002";
fn durable_frame(member: EventMember, aggregate: &str, seq: u64) -> EventEnvelope {
    let payload = serde_json::json!({
        "id": format!("evt_{aggregate}_{seq}"),
        "type": member.as_str(),
        "created": 1_700_000_000_000u64,
        "durable": {"aggregateID": aggregate, "seq": seq, "version": 1},
        "data": {"sessionID": aggregate}
    })
    .to_string();
    match EventEnvelope::decode(&payload) {
        Ok(envelope) => envelope,
        Err(error) => panic!("witness frame must decode: {error}"),
    }
}

fn live_frame(member: EventMember, aggregate: &str) -> EventEnvelope {
    let mut object = serde_json::Map::new();
    object.insert("id".into(), serde_json::json!("evt_live"));
    object.insert("type".into(), serde_json::json!(member.as_str()));
    object.insert("data".into(), serde_json::json!({"sessionID": aggregate}));
    if member.requires_created() {
        object.insert("created".into(), serde_json::json!(1_700_000_000_000u64));
    }
    let payload = serde_json::Value::Object(object).to_string();
    match EventEnvelope::decode(&payload) {
        Ok(envelope) => envelope,
        Err(error) => panic!("witness frame must decode: {error}"),
    }
}

/// A cursor that has been re-listed up to `seq` for one aggregate.
fn live_cursor(aggregate: &str, seq: u64) -> DurableCursor {
    let mut cursor = DurableCursor::new();
    cursor.adopt_baseline(&sid(aggregate), seq);
    cursor
}

#[test]
fn a_cold_cursor_is_reconciling_and_not_live() {
    // The decision the module exists for. A client that started live would
    // render a suffix and look perfectly healthy doing it.
    let cursor = DurableCursor::new();
    assert_eq!(
        cursor.state(),
        &StreamState::Reconciling {
            reason: GapReason::Cold
        }
    );
    assert!(!cursor.state().is_live());
    assert_eq!(cursor.watermark(&sid(SES)), None);
}

#[test]
fn durable_frames_are_deferred_until_a_baseline_exists() {
    // The cold-start defect, asserted directly: without this, the first frame
    // seen would silently become the baseline.
    let mut cursor = DurableCursor::new();
    let frame = durable_frame(EventMember::TextStarted, SES, 42);
    assert_eq!(cursor.admit(&frame), Admission::Deferred);
    assert_eq!(
        cursor.watermark(&sid(SES)),
        None,
        "a deferred frame must not establish a watermark"
    );
    assert!(!cursor.state().is_live());
}

#[test]
fn only_a_re_list_moves_a_cursor_to_live() {
    let mut cursor = DurableCursor::new();
    assert!(!cursor.state().is_live());
    cursor.adopt_baseline(&sid(SES), 7);
    assert!(cursor.state().is_live());
    assert_eq!(cursor.watermark(&sid(SES)), Some(7));
}

#[test]
fn the_next_contiguous_sequence_applies_and_advances() {
    let mut cursor = live_cursor(SES, 7);
    let frame = durable_frame(EventMember::TextStarted, SES, 8);
    assert_eq!(cursor.admit(&frame), Admission::Apply);
    assert_eq!(cursor.watermark(&sid(SES)), Some(8));
    assert!(cursor.state().is_live());
}

#[test]
fn re_applying_durable_history_is_a_duplicate_and_never_rewinds() {
    // Reconciliation re-reads history the client has already seen, so this is
    // the ORDINARY path, not an exceptional one. If it were destructive every
    // reconnect would duplicate the transcript.
    let mut cursor = live_cursor(SES, 7);
    for seq in [1u64, 5, 7] {
        let frame = durable_frame(EventMember::TextEnded, SES, seq);
        assert_eq!(
            cursor.admit(&frame),
            Admission::Duplicate,
            "seq {seq} is at or behind the watermark"
        );
    }
    assert_eq!(
        cursor.watermark(&sid(SES)),
        Some(7),
        "a duplicate must not rewind the watermark"
    );
    assert!(cursor.state().is_live(), "a duplicate is not a gap");
}

#[test]
fn a_forward_jump_is_applied_because_sequences_are_monotonic_with_gaps() {
    // THIS WITNESS ASSERTED THE OPPOSITE, and the rule it asserted was invented
    // here rather than taken from the contract.
    //
    // `Seq` is a position in the aggregate's DURABLE LOG, not a count of what
    // the stream delivered (packages/schema/src/event.ts:15-19), and the
    // ratified constraint is explicit that within-aggregate sequences are
    // "monotonic-with-gaps (never assume contiguity)"
    // (SECODE_RUST_RECONSTRUCTION_DISPOSITION_2026-08-07.md:175-179).
    //
    // Against a real product one prompt delivered seq 1,2,3,4,5,6,9 — entirely
    // legal — and the old rule called 9 a gap, dropped out of live, and
    // swallowed the `session.execution.succeeded` frame that the client was
    // waiting for. That is how a client with a healthy subscription came to
    // report every execution from durable history and never once show a live
    // one.
    let mut cursor = live_cursor(SES, 7);
    let frame = durable_frame(EventMember::TextStarted, SES, 9);
    assert_eq!(
        cursor.admit(&frame),
        Admission::Apply,
        "a legal forward jump must be applied, not called loss"
    );
    assert_eq!(
        cursor.watermark(&sid(SES)),
        Some(9),
        "and the watermark follows the observed position"
    );
    assert!(
        cursor.state().is_live(),
        "a gap in the durable log is not a gap in delivery"
    );
}

#[test]
fn real_loss_still_drops_the_cursor_out_of_live() {
    // The half that must NOT change. The contract's loss signals are the
    // TRANSPORT's: `/api/event` is volatile, a slow consumer overflows and
    // fails the stream, and events during disconnection are missed
    // (packages/protocol/src/groups/event.ts:44-47). Removing the invented
    // sequence rule must not remove these.
    let mut closed = live_cursor(SES, 7);
    closed.stream_closed();
    assert!(!closed.state().is_live(), "a closed stream is real loss");

    let mut overflowed = live_cursor(SES, 7);
    overflowed.buffer_overflowed();
    assert!(
        !overflowed.state().is_live(),
        "a refused frame is real loss"
    );
}

#[test]
fn a_backwards_sequence_is_a_duplicate_rather_than_a_gap() {
    // The opposite direction. A gap check that fired on any non-contiguous
    // sequence would send the client into a reconciliation loop every time a
    // re-list replayed older history — which is every reconnect.
    let mut cursor = live_cursor(SES, 7);
    let frame = durable_frame(EventMember::TextStarted, SES, 3);
    assert_eq!(cursor.admit(&frame), Admission::Duplicate);
    assert!(
        cursor.state().is_live(),
        "replayed history must not be mistaken for a gap"
    );
}

#[test]
fn a_live_frame_never_advances_a_watermark() {
    // The asymmetry that makes the durable/live split load-bearing. A
    // transport that stamped `durable` onto live frames could otherwise walk
    // the watermark over history the client never received — and the envelope
    // check in `contract` is what stops it arriving here at all.
    let mut cursor = live_cursor(SES, 7);
    for member in [
        EventMember::TextDelta,
        EventMember::ReasoningDelta,
        EventMember::UsageUpdated,
        EventMember::SessionIdle,
    ] {
        let frame = live_frame(member, SES);
        assert_eq!(
            cursor.admit(&frame),
            Admission::Advisory,
            "{} is advisory",
            member.as_str()
        );
    }
    assert_eq!(
        cursor.watermark(&sid(SES)),
        Some(7),
        "no advisory frame moved the watermark"
    );
}

#[test]
fn a_live_frame_never_heals_a_gap() {
    // The gap is created by a REAL loss signal — the volatile stream ending —
    // rather than by a forward sequence jump, which the contract says is
    // ordinary and no longer a gap at all. The principle is unchanged and is
    // the point of the witness: only re-reading durable truth heals loss, and
    // an advisory frame arriving afterwards must not make the client look
    // current again.
    let mut cursor = live_cursor(SES, 7);
    cursor.stream_closed();
    assert!(!cursor.state().is_live());

    let delta = live_frame(EventMember::TextDelta, SES);
    assert_eq!(cursor.admit(&delta), Admission::Deferred);
    assert!(
        !cursor.state().is_live(),
        "an advisory frame cannot end a reconciliation"
    );

    cursor.adopt_baseline(&sid(SES), 9);
    assert!(cursor.state().is_live(), "only a re-list heals a gap");
}

#[test]
fn server_connected_is_observable_while_reconciling() {
    // It is how the client learns a subscription exists at all, so deferring
    // it would leave the UI unable to say "connected, catching up" — which is
    // the truthful state and the one the user needs to see.
    let mut cursor = DurableCursor::new();
    let frame = live_frame(EventMember::ServerConnected, SES);
    assert_eq!(cursor.admit(&frame), Admission::Advisory);
    assert!(
        !cursor.state().is_live(),
        "observing the connection is not the same as holding durable truth"
    );
}

#[test]
fn a_closed_stream_and_an_overflow_take_the_same_recovery() {
    // A dropped frame and a dropped connection have identical consequences for
    // correctness, so they must not have different recoveries.
    let mut closed = live_cursor(SES, 7);
    closed.stream_closed();
    assert_eq!(
        closed.state(),
        &StreamState::Reconciling {
            reason: GapReason::StreamClosed
        }
    );

    let mut overflowed = live_cursor(SES, 7);
    overflowed.buffer_overflowed();
    assert_eq!(
        overflowed.state(),
        &StreamState::Reconciling {
            reason: GapReason::BufferOverflow
        }
    );

    // Both retain their watermark: the client knows what it had, it just no
    // longer knows that it has everything.
    assert_eq!(closed.watermark(&sid(SES)), Some(7));
    assert_eq!(overflowed.watermark(&sid(SES)), Some(7));
}

#[test]
fn an_unknown_aggregate_on_a_live_cursor_hydrates_rather_than_adopting_silently() {
    // The subtle one. The cursor is live for `ses1`, and a frame arrives for
    // `ses2` that no re-list covered. Adopting its sequence silently would
    // claim a complete prefix the client never read.
    let mut cursor = live_cursor(SES, 7);
    let frame = durable_frame(EventMember::SessionCreated, SES_OTHER, 1);
    match cursor.admit(&frame) {
        Admission::Gap {
            reason: GapReason::NoBaseline { ref aggregate, .. },
        } => assert_eq!(aggregate, &sid(SES_OTHER)),
        other => panic!("an uncovered aggregate must be a gap, got {other:?}"),
    }
    assert_eq!(cursor.watermark(&sid(SES_OTHER)), None);
}

#[test]
fn watermarks_are_per_aggregate_and_do_not_interfere() {
    let mut cursor = DurableCursor::new();
    cursor.adopt_baseline(&sid(SES), 5);
    cursor.adopt_baseline(&sid(SES_OTHER), 100);
    let frame = durable_frame(EventMember::TextStarted, SES, 6);
    assert_eq!(cursor.admit(&frame), Admission::Apply);
    assert_eq!(cursor.watermark(&sid(SES)), Some(6));
    assert_eq!(
        cursor.watermark(&sid(SES_OTHER)),
        Some(100),
        "one aggregate's progress must not touch another's"
    );
}

#[test]
fn a_re_list_never_rewinds_a_watermark_that_live_frames_moved_further() {
    // A re-list is a snapshot taken at some instant. If live frames have
    // already carried the client past it, adopting it verbatim would rewind
    // and re-render content the user has already seen.
    let mut cursor = live_cursor(SES, 7);
    let frame = durable_frame(EventMember::TextStarted, SES, 8);
    assert_eq!(cursor.admit(&frame), Admission::Apply);
    assert_eq!(cursor.watermark(&sid(SES)), Some(8));

    cursor.adopt_baseline(&sid(SES), 7);
    assert_eq!(
        cursor.watermark(&sid(SES)),
        Some(8),
        "an older re-list must not rewind a further-advanced watermark"
    );
    assert!(cursor.state().is_live());
}

#[test]
fn a_long_contiguous_run_stays_live_throughout() {
    // The positive control for every gap witness above. A cursor that reported
    // a gap on everything would satisfy each negative test individually.
    let mut cursor = live_cursor(SES, 0);
    for seq in 1..=200u64 {
        let frame = durable_frame(EventMember::TextStarted, SES, seq);
        assert_eq!(
            cursor.admit(&frame),
            Admission::Apply,
            "contiguous seq {seq} must apply"
        );
    }
    assert_eq!(cursor.watermark(&sid(SES)), Some(200));
    assert!(cursor.state().is_live());
}

#[test]
fn continuity_hydration_is_advisory_while_history_reconciles(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut cursor = live_cursor(SES, 7);
    cursor.stream_closed();
    for reason in [
        "transport",
        "capability",
        "server_unavailable",
        "incomplete",
        "rebound",
        "timed_out",
    ] {
        let payload = serde_json::json!({
            "id": "evt_continuity_hydration", "created": 1,
            "type": "session.continuity.attention",
            "data": {"sessionID": SES, "status": {"type": "degraded", "reason": reason}}
        });
        let envelope = EventEnvelope::decode(&payload.to_string())?;
        assert_eq!(cursor.admit(&envelope), Admission::Advisory, "{reason}");
        assert_eq!(cursor.watermark(&sid(SES)), Some(7));
        assert!(
            !cursor.state().is_live(),
            "an advisory cannot settle history"
        );
    }
    Ok(())
}
