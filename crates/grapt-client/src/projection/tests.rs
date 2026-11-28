//! Witnesses for the transcript fold.
//!
//! Every negative here perturbs exactly one fact in an otherwise conformant
//! transcript, and asserts the REASON as well as the refusal: a reducer that
//! refused because the frame was malformed, rather than because the defect was
//! detected, would pass a test that only checked for an error.

use super::*;
use crate::contract::EventEnvelope;

/// A valid execution identity for witnesses.
fn eid(raw: &str) -> crate::wire::ExecutionWireId {
    crate::wire::execution_id(raw)
        .unwrap_or_else(|error| panic!("witness execution id `{raw}` must parse: {error}"))
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

/// The canonical witness session identity.
const SES: &str = "ses_000000000001";
/// A second, distinct witness session identity.
const SES_OTHER: &str = "ses_000000000002";
const SESSION: &str = SES;

#[test]
fn continuity_attention_is_a_replaced_level_cleared_on_disconnect() {
    use grapt_api::u3a::{ContinuityAttentionInfo, ContinuityDegradedReason};
    let mut view = SessionView::new(&sid(SESSION));
    let frame = |session: &str, status: serde_json::Value| {
        event(
            EventMember::ContinuityAttention,
            serde_json::json!({"sessionID": session, "status": status}),
        )
    };
    let degraded = serde_json::json!({"type": "degraded", "reason": "transport"});
    for (session, status) in [(SES_OTHER, degraded.clone()), (SES, degraded)] {
        view.apply(&frame(session, status), bounds())
            .unwrap_or_else(|e| panic!("continuity: {e}"));
    }
    assert_eq!(
        view.continuity_attention,
        Some(ContinuityAttentionInfo::Degraded {
            reason: ContinuityDegradedReason::Transport
        })
    );
    view.apply(&frame(SES, serde_json::json!({"type": "ok"})), bounds())
        .unwrap_or_else(|e| panic!("continuity: {e}"));
    assert_eq!(view.continuity_attention, Some(ContinuityAttentionInfo::Ok));
    view.invalidate_live_quiescence();
    assert_eq!(
        view.continuity_attention, None,
        "unknown after a disconnect"
    );
}

#[test]
fn mail_attention_is_independent_scoped_and_invalidated_on_disconnect() {
    use grapt_api::u3a::{MailAttentionInfo, MailPendingCount};
    let mut view = SessionView::new(&sid(SESSION));
    for (raw_session, status) in [
        (
            SES_OTHER,
            serde_json::json!({"type":"ready","pending":9,"incomplete":false}),
        ),
        (
            SES,
            serde_json::json!({"type":"ready","pending":0,"incomplete":true}),
        ),
        (SES, serde_json::json!({"type":"unavailable"})),
    ] {
        let frame = event(
            EventMember::MailAttention,
            serde_json::json!({"sessionID":raw_session,"status":status}),
        );
        view.apply(&frame, bounds())
            .unwrap_or_else(|e| panic!("attention: {e}"));
        assert_eq!(view.execution, ExecutionState::Idle);
        assert_eq!(view.idle, None);
        if raw_session == SES_OTHER {
            assert_eq!(view.mail_attention, None);
        }
    }
    assert_eq!(view.mail_attention, Some(MailAttentionInfo::Unavailable));
    let empty = event(
        EventMember::MailAttention,
        serde_json::json!({"sessionID":SES,"status":{"type":"ready","pending":0,"incomplete":false}}),
    );
    view.apply(&empty, bounds())
        .unwrap_or_else(|e| panic!("attention: {e}"));
    assert_eq!(
        view.mail_attention,
        Some(MailAttentionInfo::Ready {
            pending: MailPendingCount::new(0).unwrap_or_else(|| panic!("zero")),
            incomplete: false,
            decode_failures: Default::default(),
        })
    );
    let stale = event(
        EventMember::MailAttention,
        serde_json::json!({
            "sessionID": SES, "status": {"type":"ready","pending":9,"incomplete":false},
            "staleSince": 1760000000000_f64
        }),
    );
    view.apply(&stale, bounds())
        .unwrap_or_else(|e| panic!("stale attention: {e}"));
    assert_eq!(view.mail_attention, Some(MailAttentionInfo::Retrying));
    view.invalidate_live_quiescence();
    assert_eq!(view.mail_attention, None);
}

fn bounds() -> Bounds {
    Bounds::PRODUCTION
}

fn small_bounds(slices: usize, slice_bytes: usize) -> Bounds {
    let bounds = Bounds {
        max_retained_slices: slices,
        max_slice_bytes: slice_bytes,
        ..Bounds::PRODUCTION
    };
    match bounds.validate() {
        Ok(bounds) => bounds,
        Err(error) => panic!("witness bounds must validate: {error}"),
    }
}

fn event(member: EventMember, mut data: serde_json::Value) -> EventEnvelope {
    // Slice frames carry the identity the real wire requires; a case that
    // probes identity sets its own fields, which are kept.
    if member.as_str().starts_with("session.text.")
        || member.as_str().starts_with("session.reasoning.")
    {
        if let Some(fields) = data.as_object_mut() {
            fields
                .entry("assistantMessageID")
                .or_insert(serde_json::json!("msg_live_delta"));
            fields.entry("ordinal").or_insert(serde_json::json!(0));
        }
    }
    let mut object = serde_json::Map::new();
    object.insert("id".into(), serde_json::json!("evt_1"));
    object.insert("type".into(), serde_json::json!(member.as_str()));
    object.insert("data".into(), data);
    if member.requires_created() {
        object.insert("created".into(), serde_json::json!(1u64));
    }
    if member.requires_durable() {
        object.insert(
            "durable".into(),
            serde_json::json!({"aggregateID": SESSION, "seq": 1, "version": 1}),
        );
    }
    let payload = serde_json::Value::Object(object).to_string();
    match EventEnvelope::decode(&payload) {
        Ok(envelope) => envelope,
        Err(error) => panic!("witness frame must decode: {error}"),
    }
}

fn plain(member: EventMember) -> EventEnvelope {
    event(member, serde_json::json!({"sessionID": SESSION}))
}

fn delta(member: EventMember, text: &str) -> EventEnvelope {
    use grapt_api::u3a::{DtoString, SliceDeltaData, WireId};
    let message = WireId::parse("msg_live_delta".to_owned())
        .unwrap_or_else(|| panic!("witness assistant message identity must parse"));
    let text = DtoString::try_new(text)
        .unwrap_or_else(|error| panic!("witness delta text must fit the DTO: {error}"));
    let data = SliceDeltaData::new(sid(SESSION), message, 0u64.into(), text);
    let value = serde_json::to_value(data)
        .unwrap_or_else(|error| panic!("real delta DTO must serialize: {error}"));
    event(member, value)
}

#[test]
fn frc_live_delta_uses_serialized_wire_before_block_end() {
    for (start, member) in [
        (EventMember::TextStarted, EventMember::TextDelta),
        (EventMember::ReasoningStarted, EventMember::ReasoningDelta),
    ] {
        let mut view = SessionView::new(&sid(SESSION));
        let mut placer = crate::double::Placer::default();
        assert_eq!(view.apply(&plain(start), bounds()), Ok(Applied::Changed));
        assert_eq!(
            view.apply(&placer.place(delta(member, "frc-active α")), bounds()),
            Ok(Applied::Changed),
            "serialized live delta should apply before block end"
        );
        assert_eq!(view.slices[0].text, "frc-active α");
        assert!(!view.slices[0].closed, "visible text must precede closure");
    }
}

#[test]
fn frc_live_delta_missing_payload_is_not_empty_text() {
    for payload in [
        serde_json::json!({"sessionID": SESSION}),
        serde_json::json!({"sessionID": SESSION, "delta": null}),
        serde_json::json!({"sessionID": SESSION, "delta": 1}),
        serde_json::json!({"sessionID": SESSION, "text": "wrong wire field"}),
    ] {
        let mut view = SessionView::new(&sid(SESSION));
        let _ = view.apply(&plain(EventMember::TextStarted), bounds());
        assert_eq!(
            view.apply(&event(EventMember::TextDelta, payload), bounds()),
            Err(ProjectionError::DeltaPayloadMissing {
                kind: SliceKind::Text
            })
        );
        assert!(view.slices[0].text.is_empty());
    }
    let mut view = SessionView::new(&sid(SESSION));
    let _ = view.apply(&plain(EventMember::TextStarted), bounds());
    let mut placer = crate::double::Placer::default();
    assert_eq!(
        view.apply(&placer.place(delta(EventMember::TextDelta, "")), bounds()),
        Ok(Applied::Changed)
    );
}

fn execution(member: EventMember, id: &str) -> EventEnvelope {
    event(
        member,
        serde_json::json!({"sessionID": SESSION, "executionID": id}),
    )
}

/// Drive a conformant text turn. This is the positive witness every negative
/// perturbs, and it exists so the laws are not vacuously refusing everything.
fn conformant(view: &mut SessionView) {
    let script = [
        execution(EventMember::ExecutionStarted, "exe1"),
        plain(EventMember::ReasoningStarted),
        delta(EventMember::ReasoningDelta, "thinking"),
        plain(EventMember::ReasoningEnded),
        plain(EventMember::TextStarted),
        delta(EventMember::TextDelta, "hello "),
        delta(EventMember::TextDelta, "world"),
        plain(EventMember::TextEnded),
        event(
            EventMember::UsageUpdated,
            serde_json::json!({"sessionID": SESSION, "input": 10, "output": 4}),
        ),
        execution(EventMember::ExecutionSucceeded, "exe1"),
        plain(EventMember::SessionIdle),
    ];
    let mut placer = crate::double::Placer::default();
    for envelope in script {
        match view.apply(&placer.place(envelope), bounds()) {
            Ok(_) => {}
            Err(error) => panic!("conformant transcript must apply: {error}"),
        }
    }
}

#[test]
fn a_conformant_turn_projects_the_expected_transcript() {
    let mut view = SessionView::new(&sid(SESSION));
    conformant(&mut view);

    assert_eq!(view.slices.len(), 2);
    assert_eq!(view.slices[0].kind, SliceKind::Reasoning);
    assert_eq!(view.slices[0].text, "thinking");
    assert!(view.slices[0].closed);
    assert_eq!(view.slices[1].kind, SliceKind::Text);
    assert_eq!(view.slices[1].text, "hello world");
    assert!(view.slices[1].closed);

    assert_eq!(
        view.usage,
        Usage {
            input: Some(10),
            output: Some(4)
        }
    );
    assert_eq!(
        view.execution,
        ExecutionState::Settled {
            execution_id: Some(eid("exe1")),
            outcome: Outcome::Succeeded
        }
    );
    assert_eq!(view.idle, Some(true));
    assert_eq!(view.dropped_slices, 0);
    assert_eq!(view.slices[1].truncated_bytes, 0);
}

#[test]
fn the_fold_is_deterministic_across_replays() {
    // The foundation of every snapshot: the same transcript must produce
    // byte-identical state, with no dependence on iteration order, hashing or
    // a clock. The crate has no clock at all, which is why this holds.
    let mut first = SessionView::new(&sid(SESSION));
    let mut second = SessionView::new(&sid(SESSION));
    conformant(&mut first);
    conformant(&mut second);
    assert_eq!(first, second);
}

#[test]
fn a_delta_without_an_opened_block_is_refused() {
    let mut view = SessionView::new(&sid(SESSION));
    assert_eq!(
        view.apply(&delta(EventMember::TextDelta, "x"), bounds()),
        Err(ProjectionError::DeltaWithoutOpen {
            kind: SliceKind::Text
        })
    );
    assert!(view.slices.is_empty(), "a refused delta creates no slice");
}

#[test]
fn an_end_that_cannot_prove_its_text_is_refused() {
    let mut view = SessionView::new(&sid(SESSION));
    assert_eq!(
        view.apply(&plain(EventMember::TextEnded), bounds()),
        Err(ProjectionError::EndUnproven {
            kind: SliceKind::Text
        })
    );
    assert!(view.slices.is_empty(), "a refused end creates no slice");
}

#[test]
fn a_second_open_of_the_same_block_is_a_duplicate() {
    let mut view = SessionView::new(&sid(SESSION));
    let _ = view.apply(&plain(EventMember::TextStarted), bounds());
    assert_eq!(
        view.apply(&plain(EventMember::TextStarted), bounds()),
        Ok(Applied::Ignored)
    );
    assert_eq!(view.slices.len(), 1, "the retained start added no slice");
}

#[test]
fn text_and_reasoning_blocks_are_independent_channels() {
    // Opening a text block while a reasoning block is open is legal; they are
    // different channels. A reducer keyed on "one open block" would refuse.
    let mut view = SessionView::new(&sid(SESSION));
    for member in [EventMember::ReasoningStarted, EventMember::TextStarted] {
        match view.apply(&plain(member), bounds()) {
            Ok(Applied::Changed) => {}
            other => panic!("{} must open, got {other:?}", member.as_str()),
        }
    }
    assert_eq!(view.slices.len(), 2);
}

#[test]
fn a_second_terminal_for_one_execution_is_refused() {
    // A server nobody can build on purpose, which is exactly why the reducer
    // must be witnessable against a synthetic transcript.
    let mut view = SessionView::new(&sid(SESSION));
    let _ = view.apply(&execution(EventMember::ExecutionStarted, "exe1"), bounds());
    let _ = view.apply(
        &execution(EventMember::ExecutionSucceeded, "exe1"),
        bounds(),
    );
    assert_eq!(
        view.apply(&execution(EventMember::ExecutionFailed, "exe1"), bounds()),
        Err(ProjectionError::SecondTerminal {
            execution_id: eid("exe1")
        })
    );
    assert_eq!(
        view.execution,
        ExecutionState::Settled {
            execution_id: Some(eid("exe1")),
            outcome: Outcome::Succeeded
        },
        "the first terminal stands"
    );
}

#[test]
fn a_terminal_for_a_different_execution_is_refused() {
    // Global counting would be satisfied by one execution settling twice while
    // another settles never. Attribution is what tells them apart.
    let mut view = SessionView::new(&sid(SESSION));
    let _ = view.apply(&execution(EventMember::ExecutionStarted, "exe1"), bounds());
    assert_eq!(
        view.apply(
            &execution(EventMember::ExecutionSucceeded, "exe2"),
            bounds()
        ),
        Err(ProjectionError::TerminalAttributionMismatch {
            running: eid("exe1"),
            observed: eid("exe2")
        })
    );
}

#[test]
fn an_execution_member_without_an_execution_id_is_refused() {
    let mut view = SessionView::new(&sid(SESSION));
    let envelope = event(
        EventMember::ExecutionSucceeded,
        serde_json::json!({"sessionID": SESSION}),
    );
    assert_eq!(
        view.apply(&envelope, bounds()),
        Err(ProjectionError::ExecutionIdMissing)
    );
}

#[test]
fn idle_is_not_a_terminal_and_does_not_settle_a_running_execution() {
    // The mistake that shows a settled run which never settled.
    let mut view = SessionView::new(&sid(SESSION));
    let _ = view.apply(&execution(EventMember::ExecutionStarted, "exe1"), bounds());
    let _ = view.apply(&plain(EventMember::SessionIdle), bounds());
    assert_eq!(view.idle, Some(true));
    assert_eq!(
        view.execution,
        ExecutionState::Running {
            execution_id: eid("exe1")
        },
        "idle must not invent an outcome for a running execution"
    );
}

#[test]
fn session_status_idle_maps_to_quiescent() {
    let mut view = SessionView::new(&sid(SESSION));
    assert_eq!(view.idle, None);
    let idle = event(
        EventMember::SessionStatus,
        serde_json::json!({"sessionID": SESSION, "status": {"type": "idle"}}),
    );
    assert_eq!(view.apply(&idle, bounds()), Ok(Applied::Changed));
    assert_eq!(view.idle, Some(true));
}

#[test]
fn session_status_busy_and_retry_map_to_not_quiescent() {
    // The ruling: retry means the provider will try again, so the session is
    // not quiescent; it is parsed and mapped now, with no new rendering.
    for status in [
        serde_json::json!({"type": "busy"}),
        serde_json::json!({"type": "retry", "attempt": 0, "message": "provider unavailable", "next": 1}),
        serde_json::json!({
            "type": "retry", "attempt": 1, "message": "provider unavailable", "next": 30,
            "action": {"reason": "rate-limit", "provider": "example", "title": "Wait",
                       "message": "try again", "label": "Retry"}
        }),
    ] {
        let mut view = SessionView::new(&sid(SESSION));
        let frame = event(
            EventMember::SessionStatus,
            serde_json::json!({"sessionID": SESSION, "status": status}),
        );
        assert_eq!(view.apply(&frame, bounds()), Ok(Applied::Changed));
        assert_eq!(view.idle, Some(false), "a non-idle status is not quiescent");
    }
}

#[test]
fn session_status_does_not_settle_a_running_execution() {
    // Quiescence is a signal, not a terminal: the same law session.idle has.
    let mut view = SessionView::new(&sid(SESSION));
    let _ = view.apply(&execution(EventMember::ExecutionStarted, "exe1"), bounds());
    let busy = event(
        EventMember::SessionStatus,
        serde_json::json!({"sessionID": SESSION, "status": {"type": "busy"}}),
    );
    assert_eq!(view.apply(&busy, bounds()), Ok(Applied::Changed));
    assert_eq!(view.idle, Some(false));
    assert_eq!(
        view.execution,
        ExecutionState::Running {
            execution_id: eid("exe1")
        },
        "a status edge must not invent an outcome for a running execution"
    );
}

#[test]
fn a_status_frame_for_a_foreign_session_is_ignored() {
    let mut view = SessionView::new(&sid(SESSION));
    let foreign = event(
        EventMember::SessionStatus,
        serde_json::json!({"sessionID": SES_OTHER, "status": {"type": "idle"}}),
    );
    assert_eq!(view.apply(&foreign, bounds()), Ok(Applied::Ignored));
    assert_eq!(
        view.idle, None,
        "another session's quiescence is not this one's"
    );
}

#[test]
fn an_unclosed_slice_is_reported_open_rather_than_repaired() {
    // What an interrupted execution actually looks like. Closing it would
    // claim a completion that did not happen.
    let mut view = SessionView::new(&sid(SESSION));
    let mut placer = crate::double::Placer::default();
    let _ = view.apply(&placer.place(plain(EventMember::TextStarted)), bounds());
    let _ = view.apply(
        &placer.place(delta(EventMember::TextDelta, "partial")),
        bounds(),
    );
    let _ = view.apply(
        &execution(EventMember::ExecutionInterrupted, "exe1"),
        bounds(),
    );
    assert_eq!(view.slices.len(), 1);
    assert!(!view.slices[0].closed, "the slice is truthfully still open");
    assert_eq!(view.slices[0].text, "partial");
    assert_eq!(
        view.execution,
        ExecutionState::Settled {
            execution_id: Some(eid("exe1")),
            outcome: Outcome::Interrupted
        }
    );
}

#[test]
fn absent_usage_stays_absent_and_never_becomes_zero() {
    // "Absent is not zero" — coercing a missing field turns "the provider did
    // not say" into "the provider said none".
    let mut view = SessionView::new(&sid(SESSION));
    let envelope = event(
        EventMember::UsageUpdated,
        serde_json::json!({"sessionID": SESSION, "input": 5}),
    );
    let _ = view.apply(&envelope, bounds());
    assert_eq!(view.usage.input, Some(5));
    assert_eq!(view.usage.output, None, "absent output must not be zero");
}

#[test]
fn the_slice_retention_bound_evicts_visibly() {
    let bounds = small_bounds(2, 1024);
    let mut view = SessionView::new(&sid(SESSION));
    let mut placer = crate::double::Placer::default();
    for _ in 0..5 {
        let _ = view.apply(&placer.place(plain(EventMember::TextStarted)), bounds);
        let _ = view.apply(&placer.place(plain(EventMember::TextEnded)), bounds);
    }
    assert_eq!(view.slices.len(), 2, "retention bound held");
    assert_eq!(
        view.dropped_slices, 3,
        "eviction is counted, not silent — the view can say what is missing"
    );
    // The retained slices are the NEWEST, and their ordinals prove it.
    assert_eq!(view.slices[0].ordinal, 3);
    assert_eq!(view.slices[1].ordinal, 4);
}

#[test]
fn the_slice_byte_bound_truncates_visibly_and_on_a_character_boundary() {
    let bounds = small_bounds(64, 8);
    let mut view = SessionView::new(&sid(SESSION));
    let mut placer = crate::double::Placer::default();
    let _ = view.apply(&placer.place(plain(EventMember::TextStarted)), bounds);
    // Multi-byte characters so a naive byte cut would split one and produce
    // invalid UTF-8.
    let _ = view.apply(
        &placer.place(delta(EventMember::TextDelta, "ααααα")),
        bounds,
    );
    let slice = &view.slices[0];
    assert!(
        slice.text.len() <= bounds.max_slice_bytes,
        "the byte bound held"
    );
    assert!(
        slice.text.chars().all(|c| c == 'α'),
        "truncation landed on a character boundary: {:?}",
        slice.text
    );
    assert!(
        slice.truncated_bytes > 0,
        "refused bytes are counted, not silently dropped"
    );
    assert_eq!(
        slice.text.len() + slice.truncated_bytes,
        "ααααα".len(),
        "every byte is either retained or accounted for"
    );
}

#[test]
fn events_for_another_session_are_ignored_rather_than_applied() {
    let mut view = SessionView::new(&sid(SESSION));
    let envelope = event(
        EventMember::TextStarted,
        serde_json::json!({"sessionID": SES_OTHER}),
    );
    assert_eq!(view.apply(&envelope, bounds()), Ok(Applied::Ignored));
    assert!(view.slices.is_empty());
}

#[test]
fn a_member_outside_the_vocabulary_is_ignored_rather_than_refused() {
    // The union is larger than this client's journey. An unknown member is not
    // a defect and must not red the transcript.
    let payload = serde_json::json!({
        "id": "evt_1",
        "type": "session.some.future.member",
        "created": 1u64,
        "data": {"sessionID": SESSION}
    })
    .to_string();
    let Ok(envelope) = EventEnvelope::decode(&payload) else {
        panic!("an unknown member decodes");
    };
    let mut view = SessionView::new(&sid(SESSION));
    assert_eq!(view.apply(&envelope, bounds()), Ok(Applied::Ignored));
}

#[test]
fn a_healthy_transcript_never_reports_a_bound() {
    // The positive control for both bound witnesses. A projection that
    // truncated everything would satisfy each of them individually.
    let mut view = SessionView::new(&sid(SESSION));
    conformant(&mut view);
    assert_eq!(view.dropped_slices, 0);
    assert!(view.slices.iter().all(|s| s.truncated_bytes == 0));
}

mod progress;
mod unfinished_answer;

#[test]
fn a_relisted_reasoning_tail_refuses_an_unowned_live_delta() {
    let mut view = SessionView::new(&sid(SESSION));
    let row = crate::wire::DurableMessage::Assistant {
        items: vec![crate::wire::MessageItem::Reasoning("consider ".into())],
        finish: None,
        error: None,
        usage: None,
        cache: Box::default(),
        footer: None,
    };
    view.apply_durable(&row, bounds())
        .unwrap_or_else(|error| panic!("history: {error}"));
    // The read never carries an open block, so this delta names another one:
    // refused (the caller relists), never appended to the recovered tail.
    let frame = delta(EventMember::ReasoningDelta, "this branch");
    assert_eq!(
        view.apply(&frame, bounds()),
        Err(crate::projection::ProjectionError::IdentityMismatch {
            kind: SliceKind::Reasoning
        })
    );
    let reasoning = view
        .slices
        .iter()
        .filter(|slice| slice.kind == SliceKind::Reasoning)
        .collect::<Vec<_>>();
    assert_eq!(reasoning.len(), 1);
    assert_eq!(reasoning[0].text, "consider ");
}

#[test]
fn an_early_failure_row_settles_the_turn_it_ended() {
    let mut view = SessionView::new(&sid(SESSION));
    for message in [
        crate::wire::DurableMessage::User { text: "go".into() },
        crate::wire::DurableMessage::ExecutionFailure {
            text: "context could not be prepared".into(),
        },
    ] {
        if let Err(error) = view.apply_durable(&message, bounds()) {
            panic!("durable rows must apply: {error}");
        }
    }
    assert!(
        matches!(
            view.execution,
            ExecutionState::Settled {
                outcome: Outcome::Failed,
                ..
            }
        ),
        "an early failure left the turn running: {:?}",
        view.execution
    );
}

fn applied(rows: Vec<crate::wire::DurableMessage>) -> SessionView {
    let mut view = SessionView::new(&sid(SESSION));
    for row in rows {
        if let Err(error) = view.apply_durable(&row, bounds()) {
            panic!("durable rows must apply: {error}");
        }
    }
    view
}

#[test]
fn an_input_after_an_early_failure_starts_a_new_turn() {
    let view = applied(vec![
        crate::wire::DurableMessage::ExecutionFailure {
            text: "context could not be prepared".into(),
        },
        crate::wire::DurableMessage::User {
            text: "again".into(),
        },
    ]);
    assert_eq!(view.execution, ExecutionState::Idle, "{:?}", view.execution);
}

#[test]
fn a_failure_after_a_tool_call_step_settles_failed() {
    let view = applied(vec![
        crate::wire::DurableMessage::User { text: "go".into() },
        crate::wire::DurableMessage::Assistant {
            items: Vec::new(),
            finish: Some(crate::wire::DurableFinish::ToolCalls),
            error: None,
            usage: None,
            cache: Box::default(),
            footer: None,
        },
        crate::wire::DurableMessage::ExecutionFailure {
            text: "the tool's turn failed".into(),
        },
    ]);
    assert!(
        matches!(
            view.execution,
            ExecutionState::Settled {
                outcome: Outcome::Failed,
                ..
            }
        ),
        "{:?}",
        view.execution
    );
}
