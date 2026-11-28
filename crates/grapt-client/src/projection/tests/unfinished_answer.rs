//! An answer read from history without a finish: in flight, or left that way
//! by an interrupt or a lost process. A read never carries an open block's
//! text, so no live delta extends it: a delta names its own block or is refused.
use super::{bounds, delta, execution, plain, sid, SESSION};
use crate::contract::EventMember;
use crate::projection::{SessionView, SliceKind};

#[test]
fn a_live_delta_after_a_history_read_is_refused_not_appended() {
    // A history read taken mid-answer: the assistant row has text and no
    // finish yet, because the answer is still streaming.
    let mut view = SessionView::new(&sid(SESSION));
    for row in [
        crate::wire::DurableMessage::User { text: "go".into() },
        crate::wire::DurableMessage::Assistant {
            items: vec![crate::wire::MessageItem::Text(
                "long-000001 long-000002 ".into(),
            )],
            finish: None,
            error: None,
            usage: None,
            cache: Box::default(),
            footer: None,
        },
    ] {
        if let Err(error) = view.apply_durable(&row, bounds()) {
            panic!("durable rows must apply: {error}");
        }
    }
    // The delta belongs to a block the read could not carry: refused (the
    // caller relists), and the recovered text is left exactly as read.
    let applied = view.apply(&delta(EventMember::TextDelta, "long-000003 "), bounds());
    assert_eq!(
        applied,
        Err(crate::projection::ProjectionError::IdentityMismatch {
            kind: SliceKind::Text
        })
    );
    assert!(view
        .slices
        .last()
        .is_some_and(|slice| slice.text == "long-000001 long-000002 "));
}

/// A view installed from a read whose last answer has no finish: in flight,
/// or left that way by an interrupt or a lost process.
fn unfinished_answer() -> SessionView {
    let mut view = SessionView::new(&sid(SESSION));
    for row in [
        crate::wire::DurableMessage::User { text: "go".into() },
        crate::wire::DurableMessage::Assistant {
            items: vec![crate::wire::MessageItem::Text("cut short ".into())],
            finish: None,
            error: None,
            usage: None,
            cache: Box::default(),
            footer: None,
        },
    ] {
        if let Err(error) = view.apply_durable(&row, bounds()) {
            panic!("durable rows must apply: {error}");
        }
    }
    view
}

/// The next turn's answer streams into a slice of its own, and the
/// unfinished answer keeps exactly its own text.
fn next_answer_streams_apart(view: &mut SessionView, what: &str) {
    let mut placer = crate::double::Placer::default();
    for frame in [
        plain(EventMember::TextStarted),
        delta(EventMember::TextDelta, "next answer"),
    ] {
        let applied = view.apply(&placer.place(frame), bounds());
        assert!(applied.is_ok(), "{what}: {applied:?}");
    }
    let texts: Vec<&str> = view
        .slices
        .iter()
        .filter(|slice| slice.kind == SliceKind::Text)
        .map(|slice| slice.text.as_str())
        .collect();
    assert_eq!(texts, ["cut short ", "next answer"], "{what}");
}

#[test]
fn an_unfinished_answer_followed_by_a_later_row_is_closed() {
    let mut view = unfinished_answer();
    let next = crate::wire::DurableMessage::User {
        text: "again".into(),
    };
    if let Err(error) = view.apply_durable(&next, bounds()) {
        panic!("durable rows must apply: {error}");
    }
    assert!(
        view.slices.iter().all(|slice| slice.closed),
        "a later row leaves the unfinished answer open"
    );
    next_answer_streams_apart(&mut view, "after a later row");
}

#[test]
fn an_unfinished_answer_is_closed_when_a_new_execution_starts() {
    let mut view = unfinished_answer();
    let started = view.apply(&execution(EventMember::ExecutionStarted, "exe2"), bounds());
    assert!(started.is_ok(), "{started:?}");
    next_answer_streams_apart(&mut view, "after a new execution started");
}

#[test]
fn an_interrupted_unfinished_answer_leaves_the_next_execution_its_own_text() {
    let mut view = unfinished_answer();
    let settled = view.apply(
        &execution(EventMember::ExecutionInterrupted, "exe1"),
        bounds(),
    );
    assert!(settled.is_ok(), "{settled:?}");
    let started = view.apply(&execution(EventMember::ExecutionStarted, "exe2"), bounds());
    assert!(started.is_ok(), "{started:?}");
    next_answer_streams_apart(&mut view, "after an interrupt and a new execution");
}

#[test]
fn an_unfinished_answer_in_older_history_is_closed() {
    let mut view = SessionView::new(&sid(SESSION));
    let next = crate::wire::DurableMessage::User {
        text: "again".into(),
    };
    if let Err(error) = view.apply_durable(&next, bounds()) {
        panic!("durable rows must apply: {error}");
    }
    view.prepend(unfinished_answer(), bounds());
    assert!(
        view.slices.iter().all(|slice| slice.closed),
        "older history leaves an unfinished answer open"
    );
    next_answer_streams_apart(&mut view, "after older history was placed in front");
}

#[test]
fn a_live_start_after_an_answer_read_complete_opens_a_new_slice() {
    // The read landed between two text blocks: the first was complete.
    let mut view = unfinished_answer();
    next_answer_streams_apart(&mut view, "a new block after the read");
}

#[test]
fn a_promoted_input_keeps_a_streaming_answer_open() {
    let mut view = unfinished_answer();
    let promoted = crate::wire::DurableMessage::User {
        text: "steer".into(),
    };
    if let Err(error) = view.apply_promoted(&promoted, bounds()) {
        panic!("a promoted input must apply: {error}");
    }
    // The promoted input does not seal the answer, and a later delta still
    // cannot extend text the read recovered.
    assert!(view
        .slices
        .iter()
        .any(|slice| slice.kind == SliceKind::Text && !slice.closed && slice.text == "cut short "));
    let applied = view.apply(&delta(EventMember::TextDelta, "more"), bounds());
    assert_eq!(
        applied,
        Err(crate::projection::ProjectionError::IdentityMismatch {
            kind: SliceKind::Text
        })
    );
}
