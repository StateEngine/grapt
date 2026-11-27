use super::*;
use crate::{
    backend::{
        commands::{Completion, View},
        Backend,
    },
    keys,
    ui::Ui,
};
use crossterm::event::{KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use std::sync::mpsc;
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn completion(
    text: &str,
    consume: bool,
    replacement: Option<String>,
    view: Option<View>,
) -> Completion {
    Completion {
        prepared_prompt: None,
        prepared_files: Vec::new(),
        text: text.into(),
        session: None,
        consume,
        replacement,
        view,
        notice: "test".into(),
    }
}
#[test]
fn completion_consumes_only_its_original_draft_and_refused_init_expands_only_that_draft() {
    for edited in [false, true] {
        let mut ui = Ui::new(false);
        ui.composer.insert_str("/init");
        ui.pending = Some("/init".into());
        if edited {
            ui.composer.insert_str(" new draft");
        }
        let before = keys::draft(&ui);
        ui.command_update(completion(
            "/init",
            false,
            Some("EXPANDED guidance".into()),
            None,
        ));
        assert!(ui.pending.is_none());
        assert_eq!(
            keys::draft(&ui),
            if edited {
                before.as_str()
            } else {
                "EXPANDED guidance"
            }
        );
    }
    let mut ui = Ui::new(false);
    ui.composer.insert_str("/status edited");
    ui.command_update(completion("/status", true, None, Some(View::Status)));
    assert_eq!(keys::draft(&ui), "/status edited");
    assert!(ui.usage_pane.is_some());
    ui.command_update(completion("/status edited", true, None, None));
    assert!(keys::draft(&ui).is_empty());
}
#[test]
fn pane_scrolls_resizes_reaches_tail_and_keeps_editor_keys_and_paste_out() -> TestResult {
    let (commands, rx) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands,
        updates,
        wake,
    };
    let mut ui = Ui::new(false);
    ui.composer.insert_str("KEEP 中 draft");
    let cursor = ui.composer.cursor();
    let rows = (0..100)
        .map(|i| format!("line {i} with 中 and a long readable explanation"))
        .chain(["TAIL result".into()])
        .collect();
    ui.command_pane = Some(Pane::new(rows));
    for (width, height) in [(40, 16), (100, 30), (9, 7), (1, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        terminal.draw(|frame| ui.draw(frame))?;
        keys::handle_key(
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        terminal.draw(|frame| ui.draw(frame))?;
        if width > 30 {
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("TAIL result"), "{text}");
        }
    }
    for code in [
        KeyCode::Char('x'),
        KeyCode::Enter,
        KeyCode::F(3),
        KeyCode::Up,
        KeyCode::Home,
    ] {
        keys::handle_key(KeyEvent::new(code, KeyModifiers::NONE), &mut ui, &backend)?;
    }
    assert!(!keys::paste("HIDDEN paste", &mut ui));
    assert_eq!(keys::draft(&ui), "KEEP 中 draft");
    assert_eq!(ui.composer.cursor(), cursor);
    assert!(rx.try_recv().is_err());
    keys::handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(ui.command_pane.is_none());
    assert!(keys::paste(" visible", &mut ui));
    Ok(())
}
#[test]
fn result_memory_and_narrow_wrapping_are_bounded_and_say_when_truncated() -> TestResult {
    let mut pane = Pane::new(vec!["中".repeat(MAX_BYTES)]);
    assert!(pane.rows.iter().map(String::len).sum::<usize>() <= MAX_BYTES + TRUNCATED.len());
    assert_eq!(pane.rows.last().map(String::as_str), Some(TRUNCATED));
    let mut terminal = Terminal::new(TestBackend::new(1, 7))?;
    terminal.draw(|frame| pane.draw(frame, frame.area(), Palette::new(false)))?;
    assert!(pane.wrapped.len() <= MAX_WRAPPED + 1);
    assert_eq!(pane.wrapped.last().map(String::as_str), Some(TRUNCATED));
    Ok(())
}

#[test]
fn a_selected_session_outside_the_picker_drives_interrupt_and_clears_old_results() -> TestResult {
    let (commands, rx) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands,
        updates,
        wake,
    };
    let mut ui = Ui::new(false);
    ui.composer.insert_str("KEEP pending edit");
    ui.pending = Some("/diff".into());
    ui.session_update(
        Some(grapt_client::wire::session_id("ses_first_visible")?),
        false,
    );
    ui.command_pane = Some(Pane::new(vec!["old workspace result".into()]));
    ui.models = Some(crate::models::Picker::default());
    ui.session_update(
        Some(grapt_client::wire::session_id("ses_hidden_current")?),
        true,
    );
    assert!(
        ui.agents.is_empty(),
        "the selected session is not in the bounded picker page"
    );
    assert!(ui.running());
    assert!(ui.command_pane.is_none() && ui.models.is_none());
    assert_eq!(keys::draft(&ui), "KEEP pending edit");
    assert_eq!(ui.pending.as_deref(), Some("/diff"));
    keys::handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(matches!(
        rx.try_recv()?,
        crate::backend::Command::InterruptAndResume(_)
    ));
    assert!(ui.picker.is_none());
    ui.session_update(
        Some(grapt_client::wire::session_id("ses_hidden_current")?),
        false,
    );
    assert!(!ui.running());
    Ok(())
}

#[test]
fn command_feedback_stays_visible_at_narrow_widths() -> TestResult {
    for width in [40, 80, 120] {
        let mut ui = Ui::new(false);
        ui.composer.insert_str("/new extra");
        ui.status = "Use /new to start a session with the selected agent, model and effort.".into();
        let mut terminal = Terminal::new(TestBackend::new(width, 24))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Use /new to start"), "{width}: {text}");
        assert_eq!(keys::draft(&ui), "/new extra");
    }
    Ok(())
}
