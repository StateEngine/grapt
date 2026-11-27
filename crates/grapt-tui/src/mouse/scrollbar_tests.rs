use super::*;
use crate::ui::Ui;
use crossterm::event::MouseButton;
use ratatui::{backend::TestBackend, Terminal};

fn conversation() -> Ui {
    let mut ui = Ui::new(false);
    ui.composer.insert_str("KEEP_DRAFT");
    ui.transcript.update(
        true,
        0,
        vec![grapt_client::Slice {
            compaction: None,
            kind: grapt_client::SliceKind::User,
            tool_id: None,
            tool_phase: None,
            tool_summary: None,
            ordinal: 0,
            text: (0..200).map(|i| format!("HISTORY_{i:03}\n")).collect(),
            read_detail_start: None,
            marks: Vec::new(),
            closed: true,
            footer: None,
            truncated_bytes: 0,
        }],
    );
    ui
}

fn painted(ui: &mut Ui) -> Result<(u16, u16), String> {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).map_err(|e| e.to_string())?;
    terminal
        .draw(|frame| ui.draw(frame))
        .map_err(|e| e.to_string())?;
    (0..24)
        .find_map(|y| {
            (0..80).find_map(|x| {
                (terminal.backend().buffer()[(x, y)].symbol() == "█").then_some((x, y))
            })
        })
        .ok_or_else(|| "Overflowing transcript has no thumb".into())
}

fn event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn dragging_the_thumb_moves_history_without_changing_the_draft() -> Result<(), String> {
    let mut ui = conversation();
    let (x, y) = painted(&mut ui)?;
    handle(
        event(MouseEventKind::Down(MouseButton::Left), x, y),
        &mut ui,
    );
    assert_eq!(ui.transcript.scroll, 0, "grabbing must not jump");
    handle(
        event(
            MouseEventKind::Drag(MouseButton::Left),
            x.saturating_sub(5),
            2,
        ),
        &mut ui,
    );
    ui.navigation.advance(&mut ui.transcript);
    assert!(
        ui.transcript.from_top || ui.transcript.scroll > 0,
        "thumb drag did not move history"
    );
    assert_eq!(ui.composer.lines(), ["KEEP_DRAFT"]);
    handle(event(MouseEventKind::Up(MouseButton::Left), 0, 0), &mut ui);
    let position = (ui.transcript.from_top, ui.transcript.scroll);
    handle(
        event(MouseEventKind::Drag(MouseButton::Left), x, 22),
        &mut ui,
    );
    ui.navigation.advance(&mut ui.transcript);
    assert_eq!(
        (ui.transcript.from_top, ui.transcript.scroll),
        position,
        "release outside the track must stop dragging"
    );
    Ok(())
}

#[test]
fn clicking_the_track_pages_and_modal_views_keep_mouse_ownership() -> Result<(), String> {
    let mut ui = conversation();
    let (x, y) = painted(&mut ui)?;
    handle(
        event(
            MouseEventKind::Down(MouseButton::Left),
            x,
            y.saturating_sub(3),
        ),
        &mut ui,
    );
    ui.navigation.advance(&mut ui.transcript);
    assert!(ui.transcript.scroll > 0, "track click did not page history");
    let position = (ui.transcript.from_top, ui.transcript.scroll);
    ui.agent_picker = Some(crate::agents::Picker::new(Vec::new()));
    handle(
        event(MouseEventKind::Down(MouseButton::Left), x, y),
        &mut ui,
    );
    handle(
        event(MouseEventKind::Drag(MouseButton::Left), x, 2),
        &mut ui,
    );
    ui.navigation.advance(&mut ui.transcript);
    assert_eq!((ui.transcript.from_top, ui.transcript.scroll), position);
    assert_eq!(ui.composer.lines(), ["KEEP_DRAFT"]);
    Ok(())
}

#[test]
fn split_sgr_press_drag_and_release_remain_mouse_events() -> Result<(), String> {
    use crate::input_bytes::Parser;
    for bytes in [
        b"\x1b[<0;80;20M".as_slice(),
        b"\x1b[<32;80;3M",
        b"\x1b[<0;80;3m",
    ] {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        for byte in bytes {
            events.extend(parser.feed(&[*byte], std::time::Duration::ZERO));
        }
        assert!(parser.idle());
        assert_eq!(events.len(), 1, "mouse report was discarded");
        assert!(matches!(&events[0], Ok(Event::Mouse(_))));
    }
    Ok(())
}

#[test]
fn selection_and_resize_cancel_an_existing_grab() -> Result<(), String> {
    for resize in [false, true] {
        let mut ui = conversation();
        let (x, y) = painted(&mut ui)?;
        handle(
            event(MouseEventKind::Down(MouseButton::Left), x, y),
            &mut ui,
        );
        if resize {
            let mut terminal =
                Terminal::new(TestBackend::new(70, 20)).map_err(|error| error.to_string())?;
            terminal
                .draw(|frame| ui.draw(frame))
                .map_err(|error| error.to_string())?;
        } else {
            let mut selection = event(MouseEventKind::Drag(MouseButton::Left), x, 2);
            selection.modifiers = KeyModifiers::SHIFT;
            handle(selection, &mut ui);
        }
        handle(
            event(MouseEventKind::Drag(MouseButton::Left), x, 2),
            &mut ui,
        );
        ui.navigation.advance(&mut ui.transcript);
        assert_eq!(ui.transcript.scroll, 0);
        assert!(!ui.transcript.from_top);
        assert_eq!(ui.composer.lines(), ["KEEP_DRAFT"]);
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn native_left_button_records_preserve_drag_release_and_selection_modifier() {
    use crossterm_winapi::{ButtonState, Coord, EventFlags};
    for (flags, buttons, kind) in [
        (
            EventFlags::PressOrRelease,
            1,
            MouseEventKind::Down(MouseButton::Left),
        ),
        (
            EventFlags::MouseMoved,
            1,
            MouseEventKind::Drag(MouseButton::Left),
        ),
        (
            EventFlags::PressOrRelease,
            0,
            MouseEventKind::Up(MouseButton::Left),
        ),
        (EventFlags::MouseMoved, 0, MouseEventKind::Moved),
    ] {
        assert!(matches!(
            native(flags, ButtonState::from(buttons), Coord { x: 79, y: 4 }, KeyModifiers::SHIFT),
            Some(Event::Mouse(event)) if event.kind == kind && event.column == 79
                && event.row == 4 && event.modifiers == KeyModifiers::SHIFT
        ));
    }
}

#[test]
fn scrollbar_end_steps_do_not_page_or_select_transcript_text() -> Result<(), String> {
    let mut ui = conversation();
    painted(&mut ui)?;
    let bar = ui
        .transcript
        .scrollbar_geometry(ui.scrollbar_area)
        .ok_or("scrollbar geometry missing")?;
    handle(
        event(
            MouseEventKind::Down(MouseButton::Left),
            bar.area.x,
            bar.area.y,
        ),
        &mut ui,
    );
    ui.navigation.advance(&mut ui.transcript);
    assert_eq!(ui.transcript.scroll, 1, "top end click must step one row");
    assert!(
        !ui.selection.pressed(),
        "scrollbar press started text selection"
    );
    assert!(!tick(&mut ui, std::time::Duration::ZERO));
    assert!(!tick(&mut ui, std::time::Duration::from_millis(399)));
    assert!(tick(&mut ui, std::time::Duration::from_millis(400)));
    ui.navigation.advance(&mut ui.transcript);
    assert_eq!(ui.transcript.scroll, 2, "holding must repeat by one row");
    painted(&mut ui)?;
    let moved = ui
        .transcript
        .scrollbar_geometry(ui.scrollbar_area)
        .ok_or("missing scrollbar after repeat")?;
    assert!(moved.thumb.y <= bar.thumb.y);
    handle(event(MouseEventKind::Up(MouseButton::Left), 0, 0), &mut ui);
    assert!(!tick(&mut ui, std::time::Duration::from_secs(1)));
    handle(
        event(
            MouseEventKind::Down(MouseButton::Left),
            moved.area.x,
            moved.area.bottom() - 1,
        ),
        &mut ui,
    );
    ui.navigation.advance(&mut ui.transcript);
    assert_eq!(ui.transcript.scroll, 1, "bottom end must step down");
    ui.agent_picker = Some(crate::agents::Picker::new(Vec::new()));
    assert!(!tick(&mut ui, std::time::Duration::from_secs(2)));
    assert!(!ticking(&ui), "modal must cancel held end");
    Ok(())
}
