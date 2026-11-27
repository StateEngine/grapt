use super::*;
use grapt_client::{Slice, SliceKind};
use ratatui::{backend::TestBackend, Terminal};

fn row(text: String, kind: SliceKind, closed: bool) -> Slice {
    Slice {
        compaction: None,
        kind,
        tool_id: None,
        tool_phase: None,
        ordinal: 0,
        text,
        read_detail_start: None,
        tool_summary: None,
        marks: Vec::new(),
        closed,
        footer: None,
        truncated_bytes: 0,
    }
}

fn lines(count: usize) -> String {
    (0..count).map(|i| format!("ROW_{i:03}\n")).collect()
}

fn visible(transcript: &mut Transcript) -> Vec<String> {
    transcript
        .visible(76, 10, Palette::new(false))
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn overflow_scrollbar_thumb_tracks_top_middle_and_live_tail_with_existing_colors(
) -> Result<(), String> {
    let mut terminal = Terminal::new(TestBackend::new(80, 14)).map_err(|e| e.to_string())?;
    let mut transcript = Transcript::new();
    transcript.update(true, 0, vec![row(lines(120), SliceKind::User, true)]);
    let content = Rect::new(1, 2, 76, 10);
    let bar = Rect::new(79, 2, 1, 10);
    for light in [false, true] {
        let palette = Palette::new(light);
        let mut previous = bar.bottom();
        for (top, scroll) in [(false, 0), (false, 60), (true, 0)] {
            transcript.from_top = top;
            transcript.scroll = scroll;
            terminal
                .draw(|frame| {
                    transcript.paint(frame, content, palette);
                    transcript.draw_scrollbar(frame, bar, palette);
                })
                .map_err(|e| e.to_string())?;
            let thumb: Vec<_> = (bar.y..bar.bottom())
                .filter(|y| terminal.backend().buffer()[(bar.x, *y)].symbol() == "█")
                .collect();
            let first = *thumb.first().ok_or("missing thumb")?;
            assert!(
                first < previous,
                "thumb must move toward the top with history"
            );
            assert!(thumb.iter().all(|y| *y > bar.y && *y < bar.bottom() - 1));
            previous = first;
            let color = if top || scroll > 0 {
                palette.accent
            } else {
                palette.muted
            };
            assert_eq!(terminal.backend().buffer()[(bar.x, first)].fg, color);
        }
    }
    transcript.update(true, 0, vec![row("fits".into(), SliceKind::User, true)]);
    terminal
        .draw(|frame| {
            transcript.paint(frame, content, Palette::new(false));
            transcript.draw_scrollbar(frame, bar, Palette::new(false));
        })
        .map_err(|e| e.to_string())?;
    assert!((bar.y..bar.bottom()).all(|y| terminal.backend().buffer()[(bar.x, y)].symbol() == " "));
    Ok(())
}

#[test]
fn detached_view_stays_on_the_same_rows_as_new_cells_arrive_and_end_restores_following() {
    let mut transcript = Transcript::new();
    transcript.update(true, 0, vec![row(lines(120), SliceKind::User, true)]);
    visible(&mut transcript);
    transcript.scroll = 24;
    let before = visible(&mut transcript);
    transcript.update(
        false,
        1,
        vec![row("NEW_OUTPUT\n".repeat(40), SliceKind::User, true)],
    );
    assert_eq!(visible(&mut transcript), before);
    transcript.scroll = 0;
    assert!(visible(&mut transcript)
        .iter()
        .any(|line| line.contains("NEW_OUTPUT")));
    assert!(!transcript.away_from_tail());
}

#[test]
fn thumb_tracks_mixed_height_items_by_their_known_rows() -> Result<(), String> {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).map_err(|e| e.to_string())?;
    let mut transcript = Transcript::new();
    transcript.update(
        true,
        0,
        vec![
            row(lines(40), SliceKind::User, true),
            row(lines(500), SliceKind::User, true),
        ],
    );
    transcript.from_top = true;
    transcript.visible(76, 600, Palette::new(false));
    transcript.from_top = false;
    transcript.scroll = 493;
    terminal
        .draw(|frame| {
            transcript.paint(frame, Rect::new(1, 2, 76, 10), Palette::new(false));
            transcript.draw_scrollbar(frame, Rect::new(79, 2, 1, 20), Palette::new(false));
        })
        .map_err(|e| e.to_string())?;
    let bar = transcript
        .scrollbar_geometry(Rect::new(79, 2, 1, 20))
        .ok_or("missing scrollbar")?;
    assert!(
        bar.thumb.y < bar.track.y + bar.track.height / 2,
        "the second item starts near the top of this transcript"
    );
    Ok(())
}

#[test]
fn detached_view_stays_on_the_same_rows_while_the_current_stream_drains() {
    let mut transcript = Transcript::new();
    let original: String = (0..120).map(|i| format!("ROW_{i:03}\n\n")).collect();
    transcript.update(true, 0, vec![row(original.clone(), SliceKind::Text, false)]);
    while transcript.draining() {
        transcript.drain();
    }
    visible(&mut transcript);
    transcript.scroll = 24;
    let before = visible(&mut transcript);
    assert!(!transcript.from_top && transcript.scroll > 0);
    transcript.update(
        false,
        0,
        vec![row(
            format!("{original}{}", "NEW_LINE\n\n".repeat(40)),
            SliceKind::Text,
            false,
        )],
    );
    while transcript.draining() {
        transcript.drain();
    }
    assert_eq!(visible(&mut transcript), before);
    transcript.scroll = 0;
    assert!(visible(&mut transcript)
        .iter()
        .any(|line| line.contains("NEW_LINE")));
}

#[test]
fn unpainted_streaming_tool_does_not_create_false_overflow() {
    let mut transcript = Transcript::new();
    let hidden = Slice {
        compaction: None,
        tool_phase: Some(grapt_client::projection::ToolPhase::Streaming),
        ..row("not visible yet".into(), SliceKind::Tool, false)
    };
    transcript.update(
        true,
        0,
        vec![row("fits".into(), SliceKind::User, true), hidden],
    );
    visible(&mut transcript);
    assert!(transcript.scrollbar_metrics().is_none());
}

#[test]
fn scrollbar_drawing_never_measures_or_lays_out_unseen_history() -> Result<(), String> {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).map_err(|e| e.to_string())?;
    let mut transcript = Transcript::new();
    transcript.update(
        true,
        0,
        (0..1000)
            .map(|_| row(lines(30), SliceKind::User, true))
            .collect(),
    );
    visible(&mut transcript);
    let before: Vec<_> = transcript
        .cells
        .iter()
        .map(|cell| (cell.height, cell.cache.is_some()))
        .collect();
    terminal
        .draw(|frame| {
            transcript.draw_scrollbar(frame, Rect::new(79, 0, 1, 24), Palette::new(false))
        })
        .map_err(|e| e.to_string())?;
    let after: Vec<_> = transcript
        .cells
        .iter()
        .map(|cell| (cell.height, cell.cache.is_some()))
        .collect();
    assert_eq!(before, after);
    assert!(transcript.painted_cells() < 10);
    Ok(())
}
