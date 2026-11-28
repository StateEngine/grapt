use crate::navigation::Navigation;
use grapt_client::{Slice, SliceKind};
use grapt_tui_transcript::palette::Palette;
use grapt_tui_transcript::transcript::*;

fn conversation() -> Transcript {
    let mut transcript = Transcript::new();
    transcript.update(
        true,
        0,
        vec![Slice {
            compaction: None,
            kind: SliceKind::User,
            tool_id: None,
            tool_phase: None,
            tool_summary: None,
            ordinal: 0,
            text: (0..60).map(|i| format!("BOUND_{i:03}\n")).collect(),
            read_detail_start: None,
            marks: Vec::new(),
            closed: true,
            footer: None,
            truncated_bytes: 0,
        }],
    );
    transcript
}

fn visible(transcript: &mut Transcript, width: u16, height: u16) -> Vec<String> {
    transcript
        .visible(width, height, Palette::new(false))
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn repeated_wheel_past_each_end_keeps_a_full_viewport_and_reverses_immediately() {
    for older in [true, false] {
        let mut transcript = conversation();
        let mut navigation = Navigation::default();
        transcript.from_top = !older;
        let expected = visible(&mut transcript, 40, 10);
        // Approach the opposite end one wheel report at a time. The old code
        // paints fewer and fewer rows before its empty-window fallback runs.
        for _ in 0..80 {
            navigation.scroll(if older { 3 } else { -3 });
            navigation.advance(&mut transcript);
            let rows = visible(&mut transcript, 40, 10);
            assert_eq!(rows.len(), 10, "partial viewport while reaching an end");
            navigation.boundary(&mut transcript);
        }
        let edge = visible(&mut transcript, 40, 10);
        assert_ne!(edge, expected);
        for _ in 0..30 {
            navigation.scroll(if older { 3 } else { -3 });
            assert!(navigation.advance(&mut transcript).is_none());
            assert_eq!(visible(&mut transcript, 40, 10), edge);
            assert!(navigation.boundary(&mut transcript).is_none());
        }
        navigation.scroll(if older { -3 } else { 3 });
        navigation.advance(&mut transcript);
        assert_ne!(
            visible(&mut transcript, 40, 10),
            edge,
            "hidden overscroll must not need unwinding"
        );
    }
}

#[test]
fn a_neighbor_page_request_keeps_the_loaded_edge_full() {
    let mut transcript = conversation();
    let mut navigation = Navigation::default();
    navigation.edges = (true, false);
    // More than the start of the first viewport, but less than the full page.
    navigation.scroll(59);
    assert!(navigation.advance(&mut transcript).is_none());
    assert_eq!(visible(&mut transcript, 40, 10).len(), 10);
    assert_eq!(
        navigation.boundary(&mut transcript),
        Some(grapt_client::client::HistoryMove::Older)
    );
    let edge = visible(&mut transcript, 40, 10);
    navigation.scroll(90);
    navigation.advance(&mut transcript);
    assert_eq!(visible(&mut transcript, 40, 10), edge);
}

#[test]
fn stale_drag_geometry_and_large_page_steps_share_the_viewport_bounds() {
    let mut transcript = conversation();
    // The drag started before content shrank or the viewport grew. Its cached
    // maximum is allowed as an input estimate, never as the painted bound.
    transcript.seek_to(1000, 1001);
    let tail = visible(&mut transcript, 40, 10);
    assert_eq!(tail.len(), 10);
    assert!(!transcript.from_top);
    assert_eq!(transcript.scroll, 0);
    let mut navigation = Navigation::default();
    for _ in 0..30 {
        navigation.scroll(-20);
        navigation.advance(&mut transcript);
        assert_eq!(visible(&mut transcript, 40, 10), tail);
    }
    navigation.scroll(20);
    navigation.advance(&mut transcript);
    assert_ne!(visible(&mut transcript, 40, 10), tail);
    let handoff = crate::backend::Handoff::default();
    let id = || handoff.envelope(crate::backend::Command::Models).0;
    navigation.jump(grapt_client::client::HistoryMove::Beginning);
    let beginning = id();
    navigation.sent(beginning);
    navigation.complete(
        (
            beginning,
            grapt_client::client::HistoryMove::Beginning,
            false,
        ),
        &mut transcript,
    );
    let top = visible(&mut transcript, 40, 10);
    assert!(top.iter().any(|row| row.contains("BOUND_000")));
    navigation.jump(grapt_client::client::HistoryMove::Latest);
    let latest = id();
    navigation.sent(latest);
    navigation.complete(
        (latest, grapt_client::client::HistoryMove::Latest, false),
        &mut transcript,
    );
    assert_eq!(visible(&mut transcript, 40, 10), tail);
}
