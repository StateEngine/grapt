//! Only admitted live prose contributes to the presentation estimate.
use super::*;

#[test]
fn live_output_ignores_old_rows_reasoning_and_restarts_for_a_new_execution() {
    let mut view = SessionView::new(&sid(SESSION));
    let limits = small_bounds(8, 4);
    view.push_slice(SliceKind::Text, "old answer", limits);
    assert_eq!(view.streamed_text_bytes(), None);
    for frame in [
        execution(EventMember::ExecutionStarted, "exe1"),
        plain(EventMember::ReasoningStarted),
        delta(EventMember::ReasoningDelta, "excluded reasoning"),
        plain(EventMember::ReasoningEnded),
        plain(EventMember::TextStarted),
        delta(EventMember::TextDelta, "123456789"),
    ] {
        assert_eq!(view.apply(&frame, limits), Ok(Applied::Changed));
    }
    assert_eq!(view.streamed_text_bytes(), Some(9));
    assert_eq!(
        view.apply(&execution(EventMember::ExecutionStarted, "exe1"), limits),
        Ok(Applied::Changed)
    );
    assert_eq!(
        view.streamed_text_bytes(),
        Some(9),
        "duplicate start retains observed progress"
    );
    assert_eq!(
        view.apply(&plain(EventMember::TextEnded), limits),
        Ok(Applied::Changed)
    );
    assert_eq!(
        view.apply(&execution(EventMember::ExecutionSucceeded, "exe1"), limits),
        Ok(Applied::Changed)
    );
    assert_eq!(
        view.streamed_text_bytes(),
        None,
        "terminal clears the live estimate"
    );
    assert_eq!(
        view.apply(&execution(EventMember::ExecutionStarted, "exe2"), limits),
        Ok(Applied::Changed)
    );
    assert_eq!(
        view.streamed_text_bytes(),
        Some(0),
        "old answer never becomes new output"
    );
}

#[test]
fn live_output_unknown_start_and_stream_gap_never_invent_a_live_output_baseline() {
    let mut view = SessionView::new(&sid(SESSION));
    for frame in [
        plain(EventMember::TextStarted),
        delta(EventMember::TextDelta, "unattributed prose"),
    ] {
        assert_eq!(view.apply(&frame, bounds()), Ok(Applied::Changed));
    }
    assert_eq!(view.streamed_text_bytes(), None);
    assert_eq!(
        view.apply(&execution(EventMember::ExecutionStarted, "exe1"), bounds()),
        Ok(Applied::Changed)
    );
    assert_eq!(
        view.apply(&delta(EventMember::TextDelta, "1234"), bounds()),
        Ok(Applied::Changed)
    );
    assert_eq!(view.streamed_text_bytes(), Some(4));
    view.invalidate_live_quiescence();
    assert_eq!(
        view.apply(&delta(EventMember::TextDelta, "after gap"), bounds()),
        Ok(Applied::Changed)
    );
    assert_eq!(view.streamed_text_bytes(), None);
}
