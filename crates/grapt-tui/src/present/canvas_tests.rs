use super::{Presenter, Ui};
use grapt_client::{Slice, SliceKind};
use ratatui::{backend::TestBackend, Terminal};

#[test]
fn moved_rows_match_full_paints_after_overlay_and_composer_geometry_changes() -> Result<(), String>
{
    let mut ui = Ui::new(false);
    let mut fresh = Ui::new(false);
    let text = (0..80)
        .map(|index| format!("row {index}: **界e\u{301}👩‍💻**\n\n"))
        .collect::<String>();
    for target in [&mut ui, &mut fresh] {
        target.transcript.update(
            true,
            0,
            vec![Slice {
                compaction: None,
                kind: SliceKind::Text,
                tool_id: None,
                tool_phase: None,
                ordinal: 0,
                text: text.clone(),
                read_detail_start: None,
                tool_summary: None,
                marks: Vec::new(),
                closed: true,
                footer: None,
                truncated_bytes: 0,
            }],
        );
        for _ in 0..80 {
            if !target.transcript.draining() {
                break;
            }
            target.transcript.drain();
        }
        assert!(!target.transcript.draining());
    }
    let mut presenter = Presenter::default();
    let mut terminal = Terminal::new(TestBackend::new(60, 18)).map_err(|e| e.to_string())?;
    let mut reference = Terminal::new(TestBackend::new(60, 18)).map_err(|e| e.to_string())?;
    for step in 0..12 {
        for target in [&mut ui, &mut fresh] {
            target.transcript.scroll = step * 3;
            if step == 2 {
                target.agent_picker = Some(crate::agents::Picker::new(Vec::new()));
            } else if step == 3 {
                target.agent_picker = None;
            } else if step == 5 {
                target.composer.insert_str("draft\nsecond line\nthird line");
            } else if step == 8 {
                target.palette = grapt_tui_transcript::palette::Palette::new(true);
            }
        }
        if step == 10 {
            terminal.backend_mut().resize(45, 16);
            reference.backend_mut().resize(45, 16);
        }
        terminal
            .draw(|frame| presenter.render(&mut ui, frame, false))
            .map_err(|e| e.to_string())?;
        reference
            .draw(|frame| fresh.draw(frame))
            .map_err(|e| e.to_string())?;
        assert_eq!(
            terminal.backend().buffer(),
            reference.backend().buffer(),
            "transition {step}"
        );
    }
    Ok(())
}
