// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// at bb64b894fd67158be0f874872e6d7bfe717e7368. Mouse capture/selection follows
// Apache-2.0, Copyright SpaceXAI).
//! Transcript drags copy painted text; Shift+drag retains native selection.
//! Modified by Grapt: agent selection also retains modal wheel ownership.
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

mod scrollbar;
pub(crate) use scrollbar::{tick, ticking, Grab};

pub fn decode(bytes: &[u8]) -> Result<Option<Event>, String> {
    let values = if bytes.starts_with(b"\x1b[M") && bytes.len() == 6 {
        [
            bytes[3].checked_sub(32).map(u16::from),
            bytes[4].checked_sub(32).map(u16::from),
            bytes[5].checked_sub(32).map(u16::from),
        ]
    } else {
        let text = std::str::from_utf8(bytes).map_err(|_| "Mouse report is not UTF-8")?;
        let body = text
            .strip_prefix("\x1b[<")
            .and_then(|s| s.strip_suffix(['M', 'm']))
            .ok_or("Invalid mouse report")?;
        let mut fields = body.split(';');
        let values = [
            fields.next().and_then(|s| s.parse().ok()),
            fields.next().and_then(|s| s.parse().ok()),
            fields.next().and_then(|s| s.parse().ok()),
        ];
        if fields.next().is_some() {
            return Err("Mouse report has extra fields".into());
        }
        values
    };
    let [Some(button), Some(column), Some(row)] = values else {
        return Err("Invalid mouse report coordinates".into());
    };
    let column = column.checked_sub(1).ok_or("Mouse column is zero")?;
    let row = row.checked_sub(1).ok_or("Mouse row is zero")?;
    let release = bytes.starts_with(b"\x1b[<") && bytes.ends_with(b"m");
    let kind = match (release, button & 0xe3) {
        (true, 0) | (false, 3) => MouseEventKind::Up(MouseButton::Left),
        (false, 0) => MouseEventKind::Down(MouseButton::Left),
        (false, 2) => MouseEventKind::Down(MouseButton::Right),
        (true, 2) => MouseEventKind::Up(MouseButton::Right),
        (false, 32) => MouseEventKind::Drag(MouseButton::Left),
        (false, 35) => MouseEventKind::Moved,
        (false, 64) => MouseEventKind::ScrollUp,
        (false, 65) => MouseEventKind::ScrollDown,
        _ => return Ok(None),
    };
    let mut modifiers = KeyModifiers::NONE;
    if button & 4 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if button & 8 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if button & 16 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    Ok(Some(Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })))
}

/// Returns whether the report changed anything worth painting.
pub fn handle(event: MouseEvent, ui: &mut crate::ui::Ui) -> bool {
    // Modal views and native Shift-selection own input before either pointer region.
    if blocked(ui) || event.modifiers.contains(KeyModifiers::SHIFT) {
        cancel(ui);
        let cleared = ui.selection.clear();
        return cleared || event.kind != MouseEventKind::Moved;
    }
    let point = Position::new(event.column, event.row);
    // A transcript grab keeps ownership across switcher rows until release.
    if ui.selection.pressed() {
        match event.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                ui.selection.drag(point);
                return true;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let (dragged, text) = ui.selection.release(point);
                if let Some(text) = text {
                    if ui.preferences.values.copy_on_select {
                        ui.clipboard.request(crate::clipboard::Action::Copy(text));
                    }
                } else if !dragged
                    && !ui.navigation.loading()
                    && !ui.mentions.visible()
                    && !ui.slash.visible()
                {
                    ui.transcript.click_tool(event.column, event.row);
                }
                // Resolve against the painted disclosure before changing focus.
                ui.transcript.clear_tool_focus();
                return true;
            }
            _ => {}
        }
    }
    // The scrollbar owns its padding and ongoing grabs before text or agent rows.
    if scrollbar::handle(event, ui) {
        return true;
    }
    if event.kind == MouseEventKind::Moved {
        return ui.switcher.hover(event.column, event.row);
    }
    let rows = match event.kind {
        MouseEventKind::ScrollUp => i64::from(ui.preferences.values.wheel_rows),
        MouseEventKind::ScrollDown => -i64::from(ui.preferences.values.wheel_rows),
        MouseEventKind::Down(MouseButton::Right) => {
            cancel(ui);
            ui.selection.clear();
            if ui.mentions_enabled() {
                ui.launch.cancel();
                ui.clipboard.request(crate::clipboard::Action::Paste);
            }
            return true;
        }
        _ => {
            if event.kind == MouseEventKind::Down(MouseButton::Left) {
                ui.selection.clear();
                if ui.mentions_enabled() && ui.images.click(event.column, event.row) {
                    return true;
                }
                if ui.scrollbar_grab.is_none()
                    && ui.switcher.click(&ui.agents, event.column, event.row)
                {
                    ui.launch.cancel();
                    return true;
                }
                if ui.scrollbar_grab.is_none()
                    && ui.transcript_area.contains(point)
                    && !ui.navigation.loading()
                    && !ui.mentions.visible()
                    && !ui.slash.visible()
                {
                    ui.launch.cancel();
                    ui.selection.start(point);
                    return true;
                }
                ui.transcript.clear_tool_focus();
            }
            return true;
        }
    };
    ui.selection.clear();
    cancel(ui);
    if ui.switcher.scroll(event.column, event.row, rows) {
        return true;
    }
    ui.launch.cancel();
    ui.navigation.scroll(rows);
    true
}

pub fn cancel(ui: &mut crate::ui::Ui) {
    ui.scrollbar_grab = None;
    ui.transcript.end_scrollbar_drag();
}

fn blocked(ui: &crate::ui::Ui) -> bool {
    ui.connect.visible()
        || ui.settings.is_some()
        || ui.command_palette.is_some()
        || ui.permission.is_some()
        || ui.picker.is_some()
        || ui.agent_picker.is_some()
        || ui.models.is_some()
        || ui.usage_pane.is_some()
        || ui.command_pane.is_some()
        || ui.grants_pane.is_some()
        || ui.forms.focused
        || ui.recall.searching()
}

#[cfg(windows)]
pub fn native(
    flags: crossterm_winapi::EventFlags,
    buttons: crossterm_winapi::ButtonState,
    position: crossterm_winapi::Coord,
    modifiers: KeyModifiers,
) -> Option<Event> {
    use crossterm_winapi::EventFlags;
    let kind = match flags {
        EventFlags::MouseWheeled if buttons.scroll_up() => MouseEventKind::ScrollUp,
        EventFlags::MouseWheeled => MouseEventKind::ScrollDown,
        EventFlags::MouseMoved if buttons.left_button() => MouseEventKind::Drag(MouseButton::Left),
        EventFlags::MouseMoved => MouseEventKind::Moved,
        EventFlags::PressOrRelease | EventFlags::DoubleClick if buttons.left_button() => {
            MouseEventKind::Down(MouseButton::Left)
        }
        EventFlags::PressOrRelease | EventFlags::DoubleClick if buttons.right_button() => {
            MouseEventKind::Down(MouseButton::Right)
        }
        EventFlags::PressOrRelease if buttons.release_button() => {
            MouseEventKind::Up(MouseButton::Left)
        }
        _ => return None,
    };
    Some(Event::Mouse(MouseEvent {
        kind,
        column: position.x.max(0) as u16,
        row: position.y.max(0) as u16,
        modifiers,
    }))
}

#[cfg(test)]
mod scrollbar_tests;
#[cfg(test)]
mod tests;
