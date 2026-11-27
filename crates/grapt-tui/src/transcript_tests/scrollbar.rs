use grapt_client::{Slice, SliceKind};
use ratatui::{backend::TestBackend, Terminal};

// Synthetic rows, duplicated from the transcript crate's scrollbar unit tests.
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

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn cold_paged_thumb_drag_keeps_the_grab_under_the_pointer() -> Result<(), String> {
    use crate::{mouse, ui::Ui};
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    for page_before_release in [true, false] {
        let mut ui = Ui::new(false);
        ui.navigation.edges = (true, true);
        ui.transcript.update(
            true,
            0,
            (0..2_000)
                .map(|i| row(format!("entry {i}\n{}", lines(80)), SliceKind::User, true))
                .collect(),
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).map_err(|e| e.to_string())?;
        terminal
            .draw(|frame| ui.draw(frame))
            .map_err(|e| e.to_string())?;
        let bar = ui
            .transcript
            .scrollbar_geometry(ui.scrollbar_area)
            .ok_or("missing bar")?;
        let report = |kind, y| MouseEvent {
            kind,
            column: bar.area.x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        ui.navigation.jump(grapt_client::client::HistoryMove::Older);
        mouse::handle(
            report(MouseEventKind::Down(MouseButton::Left), bar.thumb.y),
            &mut ui,
        );
        let target = bar.track.y + (bar.track.height - bar.thumb.height) / 2;
        mouse::handle(
            report(MouseEventKind::Drag(MouseButton::Left), target),
            &mut ui,
        );
        terminal
            .draw(|frame| ui.draw(frame))
            .map_err(|e| e.to_string())?;
        let moved = ui
            .transcript
            .scrollbar_geometry(ui.scrollbar_area)
            .ok_or("missing moved bar")?;
        assert!(
            moved.thumb.y.abs_diff(target) <= 1,
            "grabbed thumb fell behind the pointer: thumb {}, pointer {target}",
            moved.thumb.y
        );
        assert_eq!(
            moved.thumb.height, bar.thumb.height,
            "cold layout changed thumb size mid-drag"
        );
        assert_eq!(
            moved.maximum, bar.maximum,
            "cold layout changed the drag range"
        );
        assert!(
            ui.transcript
                .measured()
                .filter(|measured| *measured)
                .count()
                <= usize::from(ui.transcript_area.height) + 2,
            "drag measured the hidden history prefix"
        );
        let original = ui.transcript.slices().cloned().collect::<Vec<_>>();
        let mut incoming = Some(
            (2_000..4_001)
                .map(|i| row(format!("entry {i}\n{}", lines(80)), SliceKind::User, true))
                .collect(),
        );
        if page_before_release {
            ui.transcript_update(
                true,
                0,
                incoming.take().ok_or("missing page")?,
                false,
                Some((grapt_client::client::HistoryMove::Older, true)),
            );
            ui.navigation.complete(
                grapt_client::client::HistoryMove::Older,
                true,
                &mut ui.transcript,
            );
            ui.transcript_update(
                true,
                0,
                original.clone(),
                false,
                Some((grapt_client::client::HistoryMove::Cancel, true)),
            );
            ui.navigation.complete(
                grapt_client::client::HistoryMove::Cancel,
                true,
                &mut ui.transcript,
            );
        }
        for target in bar.track.y..=bar.track.bottom() - bar.thumb.height {
            mouse::handle(
                report(MouseEventKind::Drag(MouseButton::Left), target),
                &mut ui,
            );
            terminal
                .draw(|frame| ui.draw(frame))
                .map_err(|e| e.to_string())?;
            let moved = ui
                .transcript
                .scrollbar_geometry(ui.scrollbar_area)
                .ok_or("missing grabbed bar")?;
            assert!(moved.thumb.y.abs_diff(target) <= 1);
            assert_eq!(
                (moved.thumb.height, moved.maximum),
                (bar.thumb.height, bar.maximum)
            );
            assert_eq!(
                ui.transcript.cell_count(),
                2_000,
                "page replacement moved the captured entries"
            );
            assert!(ui.navigation.advance(&mut ui.transcript).is_none());
            assert!(ui.navigation.boundary(&mut ui.transcript).is_none());
        }
        mouse::handle(
            report(MouseEventKind::Drag(MouseButton::Left), target),
            &mut ui,
        );
        terminal
            .draw(|frame| ui.draw(frame))
            .map_err(|e| e.to_string())?;
        let selected = ui.transcript.window_first().ok_or("no selected row")?;
        let source = ui.transcript.cell_slice(selected.index).text.clone();
        mouse::handle(
            report(MouseEventKind::Up(MouseButton::Left), target),
            &mut ui,
        );
        if !page_before_release {
            ui.transcript_update(
                true,
                0,
                incoming.take().ok_or("missing late page")?,
                false,
                Some((grapt_client::client::HistoryMove::Older, true)),
            );
            ui.navigation.complete(
                grapt_client::client::HistoryMove::Older,
                true,
                &mut ui.transcript,
            );
            ui.transcript_update(
                true,
                0,
                original,
                false,
                Some((grapt_client::client::HistoryMove::Cancel, true)),
            );
            ui.navigation.complete(
                grapt_client::client::HistoryMove::Cancel,
                true,
                &mut ui.transcript,
            );
        }
        assert_eq!(
            ui.transcript.cell_count(),
            2_000,
            "cancel did not restore the captured page"
        );
        ui.navigation.advance(&mut ui.transcript);
        assert!(!ui.navigation.loading());
        terminal
            .draw(|frame| ui.draw(frame))
            .map_err(|e| e.to_string())?;
        let restored = ui
            .transcript
            .window_first()
            .ok_or("lost selected row on release")?;
        assert_eq!(ui.transcript.cell_slice(restored.index).text, source);
        assert_eq!(
            restored.start, selected.start,
            "obsolete page intent replaced the drag selection"
        );
        let bar = ui
            .transcript
            .scrollbar_geometry(ui.scrollbar_area)
            .ok_or("missing released bar")?;
        mouse::handle(
            report(MouseEventKind::Down(MouseButton::Left), bar.thumb.y),
            &mut ui,
        );
        mouse::handle(
            report(MouseEventKind::Drag(MouseButton::Left), target),
            &mut ui,
        );
        terminal
            .draw(|frame| ui.draw(frame))
            .map_err(|e| e.to_string())?;
        mouse::handle(
            report(MouseEventKind::Up(MouseButton::Left), target),
            &mut ui,
        );
        let before = ui.transcript.window_first().ok_or("no reading anchor")?;
        ui.navigation.scroll(-1);
        assert!(ui.navigation.advance(&mut ui.transcript).is_none());
        terminal
            .draw(|frame| ui.draw(frame))
            .map_err(|e| e.to_string())?;
        let after = ui
            .transcript
            .window_first()
            .ok_or("no reading anchor after wheel")?;
        assert_eq!(after.index, before.index);
        assert_eq!(
            after.start,
            before.start + 1,
            "release changed the meaning of relative scroll"
        );
    }
    Ok(())
}
