use crate::{clipboard::Action, mouse, ui::Ui};
use crossterm::event::{Event, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};

#[test]
fn transcript_drag_copies_unicode_and_right_click_respects_modal_ownership() -> Result<(), String> {
    let mut ui = Ui::new(false);
    ui.composer.insert_str("kept draft");
    ui.transcript_area = Rect::new(2, 3, 16, 2);
    let mut canvas = Buffer::empty(ui.transcript_area);
    canvas.set_string(2, 3, "界e\u{301} first", Style::default());
    canvas.set_string(2, 4, "second", Style::default());
    let report = |bytes: &[u8], ui: &mut Ui| -> Result<(), String> {
        let Some(Event::Mouse(event)) = mouse::decode(bytes)? else {
            return Err("Expected mouse report".into());
        };
        mouse::handle(event, ui);
        Ok(())
    };
    // Start inside the second display cell of a wide character; copy it whole.
    report(b"\x1b[<0;4;4M", &mut ui)?;
    ui.selection.capture(&canvas, ui.transcript_area);
    report(b"\x1b[<32;5;5M", &mut ui)?;
    report(b"\x1b[<0;5;5m", &mut ui)?;
    let Some(Action::Copy(text)) = ui.clipboard.take_request() else {
        return Err("Transcript drag did not request a copy".into());
    };
    assert_eq!(text, "界e\u{301} first\nsec");
    assert_eq!(ui.composer.lines(), ["kept draft"]);
    report(b"\x1b[<2;3;8M", &mut ui)?;
    assert!(matches!(ui.clipboard.take_request(), Some(Action::Paste)));
    ui.agent_picker = Some(crate::agents::Picker::new(Vec::new()));
    report(b"\x1b[<2;3;8M", &mut ui)?;
    assert!(ui.clipboard.take_request().is_none());
    ui.agent_picker = None;
    let Some(Event::Mouse(mut event)) = mouse::decode(b"\x1b[<0;3;4M")? else {
        return Err("Expected left press".into());
    };
    event.modifiers = KeyModifiers::SHIFT;
    mouse::handle(event, &mut ui);
    assert!(!ui.selection.pressed());
    Ok(())
}
