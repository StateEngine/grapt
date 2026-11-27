use super::*;

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
fn resize_and_content_shrink_recompute_both_ends_without_blank_rows() {
    for older in [true, false] {
        let mut transcript = conversation();
        transcript.from_top = !older;
        transcript.scroll = 59;
        let rows = visible(&mut transcript, 40, 10);
        assert_eq!(rows.len(), 10);
        assert_eq!(transcript.from_top, older);
        assert_eq!(transcript.scroll, 0);
        for (width, height) in [(18, 20), (80, 6), (40, 12)] {
            assert_eq!(
                visible(&mut transcript, width, height).len(),
                usize::from(height)
            );
            assert_eq!(transcript.from_top, older);
            assert_eq!(transcript.scroll, 0);
        }
        transcript.from_top = !older;
        transcript.scroll = 59;
        transcript.cells[0].slice.text = "short".into();
        transcript.cells[0].shown = 5;
        transcript.cells[0].cache = None;
        transcript.cells[0].height = None;
        let rows = visible(&mut transcript, 40, 12);
        assert!(rows.iter().any(|row| row.contains("short")));
        assert_eq!(transcript.scroll, 0);
    }
}
