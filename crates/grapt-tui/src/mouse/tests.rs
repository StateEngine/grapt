use super::*;
use crate::{input_bytes::Parser, ui::Ui};
use ratatui::{backend::TestBackend, Terminal};
use std::time::Duration;

#[test]
fn split_sgr_and_legacy_wheel_reports_never_become_keys_or_draft_text() -> Result<(), String> {
    for bytes in [
        b"\x1b[<64;4;10M".as_slice(),
        b"\x1b[M`$*",
        b"\x1b[<65;4;10M",
        b"\x1b[<35;4;10M",
    ] {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        for byte in bytes {
            events.extend(parser.feed(&[*byte], Duration::ZERO));
        }
        assert!(parser.idle());
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(Event::Mouse(_))));
        let mut ui = Ui::new(false);
        ui.composer.insert_str("KEEP_DRAFT");
        for event in events {
            if let Event::Mouse(mouse) = event? {
                handle(mouse, &mut ui);
            }
        }
        assert_eq!(ui.composer.lines(), ["KEEP_DRAFT"]);
        ui.navigation.advance(&mut ui.transcript);
        if bytes.contains(&b'5') {
            assert_eq!(ui.transcript.scroll, 0);
        } else {
            assert_eq!(ui.transcript.scroll, 3);
        }

        // Agents owns input even at a page boundary with history on both sides.
        // Apply pending navigation too, so a hidden page request cannot pass.
        for scroll in [0, 7] {
            let mut ui = Ui::new(false);
            ui.composer.insert_str("KEEP_AGENT_DRAFT\nsecond line");
            ui.agent_picker = Some(crate::agents::Picker::new(Vec::new()));
            ui.navigation.edges = (true, true);
            ui.transcript.from_top = true;
            ui.transcript.scroll = scroll;
            let Some(Event::Mouse(mouse)) = decode(bytes)? else {
                return Err("wheel fixture did not decode to a mouse report".into());
            };
            handle(mouse, &mut ui);
            assert_eq!(ui.navigation.advance(&mut ui.transcript), None);
            assert!(!ui.navigation.loading());
            assert_eq!(ui.transcript.scroll, scroll);
            assert!(ui.transcript.from_top);
            assert!(ui.agent_picker.is_some());
            assert_eq!(ui.composer.lines(), ["KEEP_AGENT_DRAFT", "second line"]);
        }
    }
    Ok(())
}

#[test]
fn click_motion_release_and_invalid_reports_cannot_leak_into_the_editor() {
    for bytes in [
        b"\x1b[<0;1;1M".as_slice(),
        b"\x1b[<0;1;1m",
        b"\x1b[<64;1;1m",
        b"\x1b[<32;1;1M",
        b"\x1b[<64;0;1M",
        b"\x1b[<999999;1;1M",
    ] {
        let mut parser = Parser::new();
        let events = parser.feed(bytes, Duration::ZERO);
        assert!(parser.idle());
        assert!(events
            .iter()
            .all(|e| !matches!(e, Ok(Event::Key(_)) | Ok(Event::Paste(_)))));
    }
}

#[cfg(windows)]
#[test]
fn native_console_records_preserve_wheel_direction_without_editor_keys() {
    for (delta, kind) in [
        (120_i16, MouseEventKind::ScrollUp),
        (-120, MouseEventKind::ScrollDown),
    ] {
        let position = crossterm_winapi::Coord { x: 4, y: 10 };
        let buttons = crossterm_winapi::ButtonState::from((delta as u16 as u32) << 16);
        let flags = crossterm_winapi::EventFlags::MouseWheeled;
        assert!(
            matches!(native(flags, buttons, position, KeyModifiers::NONE), Some(Event::Mouse(e)) if e.kind == kind)
        );
        assert_eq!(
            native(
                crossterm_winapi::EventFlags::PressOrRelease,
                buttons,
                position,
                KeyModifiers::NONE,
            ),
            None
        );
    }
}

#[test]
fn long_history_keeps_submission_recovery_offline_feedback_and_pending_questions_visible(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_client::wire::{form_id, session_id, FormField, FormOwner, FormRow};
    for (width, height) in [(80, 24), (160, 40)] {
        let mut ui = Ui::new(false);
        ui.composer.insert_str("KEEP_RECOVERY_DRAFT");
        let session = session_id("ses_wheel_feedback")?;
        ui.forms.update(
            Some(session.clone()),
            None,
            vec![FormRow {
                metadata: Default::default(),
                id: form_id("frm_wheel_feedback")?,
                owner: FormOwner::Session(session),
                title: "Pending question".into(),
                fields: vec![FormField {
                    attributes: Default::default(),
                    key: "branch".into(),
                    kind: "string".into(),
                    title: None,
                    required: true,
                }],
            }],
        );
        ui.forms.focused = false;
        ui.transcript.update(
            true,
            0,
            vec![grapt_client::Slice {
                compaction: None,
                kind: grapt_client::SliceKind::User,
                tool_id: None,
                tool_phase: None,
                ordinal: 0,
                text: (0..80).map(|i| format!("feedback row {i}\n")).collect(),
                read_detail_start: None,
                tool_summary: None,
                marks: Vec::new(),
                closed: true,
                footer: None,
                truncated_bytes: 0,
            }],
        );
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        for status in [
            grapt_client::ClientError::Unconfirmed.to_string(),
            "Offline: reconnect failed".into(),
        ] {
            ui.status = status.clone();
            for scroll in [0, 6] {
                ui.transcript.scroll = scroll;
                terminal.draw(|frame| ui.draw(frame))?;
                let screen: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(screen.contains(&status), "recovery hidden: {screen}");
                assert!(screen.contains("question"), "pending question hidden");
                assert!(screen.contains("KEEP_RECOVERY_DRAFT"));
            }
        }
    }
    Ok(())
}
