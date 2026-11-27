use super::*;
use grapt_client::{Slice, SliceKind};
use ratatui::backend::TestBackend;

fn row(text: &str) -> Slice {
    Slice {
        compaction: None,
        kind: SliceKind::User,
        tool_id: None,
        tool_phase: None,
        ordinal: 0,
        text: text.into(),
        read_detail_start: None,
        tool_summary: None,
        marks: Vec::new(),
        closed: true,
        footer: None,
        truncated_bytes: 0,
    }
}

#[test]
fn immediate_edits_exclude_commands_and_history_navigation() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for code in [
        KeyCode::Char('中'),
        KeyCode::Backspace,
        KeyCode::Left,
        KeyCode::End,
    ] {
        assert!(crate::edits_composer(KeyEvent::new(
            code,
            KeyModifiers::NONE
        )));
    }
    assert!(crate::edits_composer(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::SHIFT
    )));
    for code in [KeyCode::Enter, KeyCode::Esc, KeyCode::F(2), KeyCode::PageUp] {
        assert!(!crate::edits_composer(KeyEvent::new(
            code,
            KeyModifiers::NONE
        )));
    }
    for (code, modifier) in [
        (KeyCode::Char('p'), KeyModifiers::CONTROL),
        (KeyCode::Home, KeyModifiers::CONTROL),
        (KeyCode::Down, KeyModifiers::ALT),
    ] {
        assert!(!crate::edits_composer(KeyEvent::new(code, modifier)));
    }
}

#[test]
fn input_paint_preserves_the_conversation_until_its_own_frame() -> Result<(), String> {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).map_err(|e| e.to_string())?;
    let mut ui = Ui::new(false);
    ui.policy = Some("Preset: default | effective: maintainer".into());
    ui.activity.observe(
        true,
        Some(3600),
        crate::activity::Phase::Thinking,
        tokio::time::Instant::now(),
    );
    ui.transcript.update(true, 0, vec![row("already painted")]);
    let mut presenter = Presenter::default();
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, false))
        .map_err(|e| e.to_string())?;
    let before = terminal.backend().buffer().clone();
    ui.transcript
        .update(true, 0, vec![row("queued conversation update")]);
    ui.composer.insert_str("typing é👩‍💻");
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    let after = terminal.backend().buffer();
    for y in 0..40 {
        for x in 0..120 {
            if !ui.composer_area.contains((x, y).into()) {
                assert_eq!(
                    before[(x, y)],
                    after[(x, y)],
                    "input changed a conversation cell"
                );
            }
        }
    }
    assert!(after.content.iter().any(|cell| cell.symbol() == "👩‍💻"));
    ui.composer = crate::editor::Editor::default();
    ui.composer.insert_str("x");
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    assert!(
        !terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.symbol() == "👩‍💻"),
        "the complete editor region clears old wide text"
    );
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, false))
        .map_err(|e| e.to_string())?;
    assert_ne!(
        terminal.backend().buffer(),
        &before,
        "the normal frame applies queued content"
    );
    Ok(())
}

#[test]
fn editor_growth_resize_and_overlay_use_a_complete_frame() -> Result<(), String> {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).map_err(|e| e.to_string())?;
    let mut ui = Ui::new(false);
    ui.transcript.update(true, 0, vec![row("conversation")]);
    let mut presenter = Presenter::default();
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, false))
        .map_err(|e| e.to_string())?;
    let single_line = ui.composer_area;
    ui.composer.insert_str("first\nsecond");
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    assert!(ui.composer_area.height > single_line.height);
    assert_eq!(presenter.composer, ui.composer_area);
    terminal.backend_mut().resize(80, 24);
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    assert_eq!(
        presenter.canvas.as_ref().map(|b| b.area),
        Some(Rect::new(0, 0, 80, 24))
    );
    ui.picker = Some(crate::sessions::Picker::new(&ui.agents));
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    assert_eq!(
        presenter.canvas.as_ref(),
        Some(terminal.backend().buffer()),
        "an overlay receives a complete saved frame"
    );
    Ok(())
}

#[test]
fn deleting_a_saved_draft_matches_complete_editor_pixels_and_cursor_styles() -> Result<(), String> {
    for (width, height) in [(80, 24), (120, 40)] {
        for light in [false, true] {
            for replacement in ["x", "", "中"] {
                let mut terminal =
                    Terminal::new(TestBackend::new(width, height)).map_err(|e| e.to_string())?;
                let mut ui = Ui::new(light);
                ui.composer.insert_str("ABCDE 👩‍💻 ZZZ");
                let mut presenter = Presenter::default();
                terminal
                    .draw(|frame| presenter.render(&mut ui, frame, false))
                    .map_err(|e| e.to_string())?;
                ui.composer = crate::editor::Editor::default();
                ui.composer.insert_str(replacement);
                terminal
                    .draw(|frame| presenter.render(&mut ui, frame, true))
                    .map_err(|e| e.to_string())?;
                let immediate = terminal.backend().buffer().clone();
                terminal
                    .draw(|frame| presenter.render(&mut ui, frame, false))
                    .map_err(|e| e.to_string())?;
                assert_eq!(
                    &immediate,
                    terminal.backend().buffer(),
                    "immediate editor differs from complete render for {replacement:?}"
                );
            }
        }
    }
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn cached_stream_frames_match_full_frames_through_metadata_and_overlay_changes(
) -> Result<(), String> {
    let mut ui = Ui::new(false);
    let mut fresh = Ui::new(false);
    let mut presenter = Presenter::default();
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).map_err(|e| e.to_string())?;
    let mut reference = Terminal::new(TestBackend::new(120, 40)).map_err(|e| e.to_string())?;
    let mut source = String::new();
    let now = tokio::time::Instant::now();
    for index in 0..200 {
        for target in [&mut ui, &mut fresh] {
            target.activity.observe(
                (50..150).contains(&index),
                (index < 75).then_some(3600),
                crate::activity::Phase::Thinking,
                now + std::time::Duration::from_secs(index),
            );
        }
        source.push_str(match index % 12 {
            0 => "## Section\n",
            1 => "\n",
            2 => "paragraph **bold** 中👩🏽‍💻\n",
            3 => "\n",
            4 => "```rust\n",
            5 => "let value = 1;\n",
            6 => "let next = 2;\n",
            7 => "```\n",
            8 => "\n",
            9 => "after\n",
            _ => "\n",
        });
        let mut slice = row(&source);
        slice.kind = SliceKind::Text;
        slice.closed = index == 199;
        ui.transcript.update(false, 0, vec![slice.clone()]);
        fresh.transcript.update(false, 0, vec![slice]);
        ui.transcript.drain();
        fresh.transcript.drain();
        if index % 17 == 0 {
            ui.status = format!("status{index}");
            fresh.status = ui.status.clone();
        }
        if index % 23 == 0 {
            ui.title = format!("title{index}");
            fresh.title = ui.title.clone();
        }
        if index % 41 == 0 {
            ui.palette = grapt_tui_transcript::palette::Palette::new(!ui.palette.light);
            fresh.palette = ui.palette;
        }
        if index % 19 == 0 {
            ui.composer.insert_str("draft");
            fresh.composer.insert_str("draft");
        }
        if index == 35 {
            ui.picker = Some(crate::sessions::Picker::new(&ui.agents));
            fresh.picker = Some(crate::sessions::Picker::new(&fresh.agents));
        }
        if index == 36 {
            ui.picker = None;
            fresh.picker = None;
        }
        if index == 100 {
            terminal.backend_mut().resize(80, 24);
            reference.backend_mut().resize(80, 24);
        }
        terminal
            .draw(|frame| presenter.render(&mut ui, frame, index == 36))
            .map_err(|e| e.to_string())?;
        reference
            .draw(|frame| fresh.draw(frame))
            .map_err(|e| e.to_string())?;
        for (cell, (actual, expected)) in terminal
            .backend()
            .buffer()
            .content
            .iter()
            .zip(&reference.backend().buffer().content)
            .enumerate()
        {
            assert_eq!(actual, expected, "stream frame {index} cell {cell}");
        }
    }
    for input in [false, true] {
        for messages in [
            vec!["first question".into()],
            vec!["replacement question".into()],
            Vec::new(),
        ] {
            ui.queued.messages = messages;
            ui.queued.total = ui.queued.messages.len();
            fresh.queued = ui.queued.clone();
            terminal
                .draw(|frame| presenter.render(&mut ui, frame, input))
                .map_err(|e| e.to_string())?;
            reference
                .draw(|frame| fresh.draw(frame))
                .map_err(|e| e.to_string())?;
            assert_eq!(
                terminal.backend().buffer(),
                reference.backend().buffer(),
                "queued-only update differs with input={input}"
            );
            assert_eq!(ui.transcript_area, fresh.transcript_area);
        }
    }
    Ok(())
}

#[test]
fn retained_canvas_clears_replacements_resets_and_scrolled_rows() -> Result<(), String> {
    let mut ui = Ui::new(false);
    let mut reference_ui = Ui::new(false);
    let mut presenter = Presenter::default();
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).map_err(|e| e.to_string())?;
    let mut reference = Terminal::new(TestBackend::new(60, 16)).map_err(|e| e.to_string())?;
    for (index, text) in [
        "wide 中👩🏽‍💻\n".repeat(30),
        "short\n".into(),
        "replacement\n".repeat(20),
        "".into(),
        "new session\n".into(),
    ]
    .iter()
    .enumerate()
    {
        for target in [&mut ui, &mut reference_ui] {
            target.transcript.update(index == 4, 0, vec![row(text)]);
            target.transcript.scroll = if index == 2 { 4 } else { 0 };
        }
        terminal
            .draw(|frame| presenter.render(&mut ui, frame, false))
            .map_err(|e| e.to_string())?;
        reference
            .draw(|frame| reference_ui.draw(frame))
            .map_err(|e| e.to_string())?;
        assert_eq!(
            terminal.backend().buffer(),
            reference.backend().buffer(),
            "replacement {index}"
        );
    }
    Ok(())
}

#[test]
fn usage_updates_stay_in_details_and_open_details_disable_input_shortcut() -> Result<(), String> {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).map_err(|e| e.to_string())?;
    let mut ui = Ui::new(false);
    let mut presenter = Presenter::default();
    terminal
        .draw(|f| presenter.render(&mut ui, f, false))
        .map_err(|e| e.to_string())?;
    let before = terminal.backend().buffer().clone();
    ui.usage = crate::usage::tests::measured().map_err(|e| e.to_string())?;
    terminal
        .draw(|f| presenter.render(&mut ui, f, false))
        .map_err(|e| e.to_string())?;
    assert_eq!(
        terminal.backend().buffer(),
        &before,
        "routine usage must stay in the details view"
    );
    ui.usage_pane = Some(crate::usage::Pane::default());
    terminal
        .draw(|f| presenter.render(&mut ui, f, true))
        .map_err(|e| e.to_string())?;
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(text.contains("Usage · live session"));
    assert!(text.contains("Cache:"));
    let before = terminal.backend().buffer().clone();
    ui.usage = grapt_client::view::status::Snapshot::default();
    terminal
        .draw(|f| presenter.render(&mut ui, f, true))
        .map_err(|e| e.to_string())?;
    assert_ne!(
        terminal.backend().buffer(),
        &before,
        "open usage stayed stale"
    );
    Ok(())
}

#[test]
fn recovery_feedback_invalidates_cached_chrome_without_losing_draft() -> Result<(), String> {
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).map_err(|e| e.to_string())?;
    let mut ui = Ui::new(false);
    ui.status = "Connected".into();
    ui.composer.insert_str("KEPT draft");
    let mut presenter = Presenter::default();
    terminal
        .draw(|f| presenter.render(&mut ui, f, false))
        .map_err(|e| e.to_string())?;
    ui.feedback = Some(grapt_client::projection::feedback::Feedback::Failed {
        message: "Inspect the Session before retrying.".into(),
    });
    assert!(presenter
        .chrome
        .as_ref()
        .is_some_and(|c| !c.matches(&ui, ratatui::layout::Rect::new(0, 0, 40, 16))));
    terminal
        .draw(|f| presenter.render(&mut ui, f, false))
        .map_err(|e| e.to_string())?;
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(text.contains("Execution failed"));
    assert!(text.contains("KEPT draft"));
    ui.feedback = None;
    terminal
        .draw(|f| presenter.render(&mut ui, f, false))
        .map_err(|e| e.to_string())?;
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(!text.contains("Execution failed"));
    assert!(text.contains("KEPT draft"));
    Ok(())
}

#[test]
fn command_result_owns_cached_paints_until_closed() -> Result<(), String> {
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).map_err(|e| e.to_string())?;
    let mut ui = Ui::new(false);
    ui.composer.insert_str("retained draft");
    let mut presenter = Presenter::default();
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, false))
        .map_err(|e| e.to_string())?;
    ui.command_pane = Some(crate::command_pane::Pane::new(vec![
        "CORE COMMAND RESULT".into()
    ]));
    for input in [true, false] {
        terminal
            .draw(|frame| presenter.render(&mut ui, frame, input))
            .map_err(|e| e.to_string())?;
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("CORE COMMAND RESULT"));
        assert!(
            presenter.chrome.is_none(),
            "an overlay must not cache a clean conversation frame"
        );
    }
    ui.command_pane = None;
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(!text.contains("CORE COMMAND RESULT"));
    assert!(text.contains("retained draft"));
    Ok(())
}

#[test]
fn agent_picker_open_navigation_typing_and_close_match_fresh_frames(
) -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::mpsc;
    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = crate::backend::Backend {
        commands,
        updates,
        wake,
    };
    for input in [false, true] {
        let mut ui = Ui::new(false);
        let mut fresh = Ui::new(false);
        for state in [&mut ui, &mut fresh] {
            state.composer.insert_str("KEPT draft");
            state
                .transcript
                .update(true, 0, vec![row("visible conversation")]);
        }
        let mut presenter = Presenter::default();
        let mut terminal = Terminal::new(TestBackend::new(100, 30))?;
        let mut reference = Terminal::new(TestBackend::new(100, 30))?;
        terminal.draw(|frame| presenter.render(&mut ui, frame, false))?;
        let agents = ["First", "Second"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                Ok(grapt_client::wire::AgentRow {
                    id: grapt_client::wire::agent_id(&format!("agt_picker_{i}"))?,
                    name: name.into(),
                    description: None,
                    mode: grapt_api::u3a::AgentInfoDtoMode::Primary,
                    hidden: false,
                    source: None,
                })
            })
            .collect::<Result<Vec<_>, grapt_client::wire::WireError>>()?;
        ui.agent_picker = Some(crate::agents::Picker::new(agents.clone()));
        fresh.agent_picker = Some(crate::agents::Picker::new(agents));
        for key in [
            None,
            Some(KeyCode::Down),
            Some(KeyCode::Char('x')),
            Some(KeyCode::Up),
            Some(KeyCode::Esc),
        ] {
            if let Some(code) = key {
                for state in [&mut ui, &mut fresh] {
                    crate::keys::handle_key(
                        KeyEvent::new(code, KeyModifiers::NONE),
                        state,
                        &backend,
                    )?;
                }
            }
            terminal.draw(|frame| presenter.render(&mut ui, frame, input))?;
            reference.draw(|frame| fresh.draw(frame))?;
            assert_eq!(
                terminal.backend().buffer(),
                reference.backend().buffer(),
                "picker differs after {key:?}, input={input}"
            );
            assert_eq!(crate::keys::draft(&ui), "KEPT draft");
            if ui.agent_picker.is_some() {
                assert!(
                    presenter.chrome.is_none(),
                    "picker overlay was cached as clean chrome"
                );
            }
        }
        assert!(ui.agent_picker.is_none());
    }
    assert!(
        received.try_recv().is_err(),
        "picker navigation or close submitted work"
    );
    Ok(())
}

#[test]
fn agent_picker_temporarily_disables_completions_and_preserves_the_editor() -> Result<(), String> {
    let mut ui = Ui::new(false);
    ui.composer.insert_str("/rew");
    ui.agent_picker = Some(crate::agents::Picker::new(Vec::new()));
    let mut presenter = Presenter::default();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).map_err(|e| e.to_string())?;
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, false))
        .map_err(|e| e.to_string())?;
    assert!(!ui.mentions_enabled());
    assert!(!ui.slash.visible());
    assert_eq!(crate::keys::draft(&ui), "/rew");
    ui.agent_picker = None;
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, false))
        .map_err(|e| e.to_string())?;
    assert!(ui.mentions_enabled());
    assert!(ui.slash.visible());
    assert_eq!(
        ui.slash.key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE
        )),
        Some("ind ".into())
    );
    assert_eq!(crate::keys::draft(&ui), "/rew");
    Ok(())
}

#[test]
fn bypass_indicator_survives_pane_close_status_and_input_paints() -> Result<(), String> {
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).map_err(|e| e.to_string())?;
    let mut ui = Ui::new(false);
    ui.policy = Some("Bypass permissions on | Preset: bypass | effective: bypass".into());
    ui.bypass_permissions = true;
    ui.command_pane = Some(crate::command_pane::Pane::new(vec!["PERMISSIONS".into()]));
    let mut presenter = Presenter::default();
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, false))
        .map_err(|e| e.to_string())?;
    ui.command_pane = None;
    for status in ["Connected", "Service reconnecting", "Connected"] {
        ui.status = status.into();
        ui.composer.insert_str("x");
        terminal
            .draw(|frame| presenter.render(&mut ui, frame, true))
            .map_err(|e| e.to_string())?;
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("Bypass permissions on"), "{status}");
    }
    ui.policy = Some("Preset: bypass | effective: inspect".into());
    ui.bypass_permissions = false;
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    let screen = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!screen.contains("effective: inspect"));
    assert!(!screen.contains("Bypass permissions on"));
    ui.policy = Some("Preset unavailable: disconnected".into());
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    let screen = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(screen.contains("Preset unavailable"));
    assert!(!screen.contains("effective: inspect"));
    Ok(())
}

#[test]
fn mail_offline_survives_pane_close_typing_and_clears_only_on_readback() -> Result<(), String> {
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).map_err(|e| e.to_string())?;
    let mut ui = Ui::new(false);
    ui.status = "Connected".into();
    let mut presenter = Presenter::default();
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, false))
        .map_err(|e| e.to_string())?;
    // This is an input paint on a previously healthy retained canvas.
    ui.mail_attention = Some(grapt_api::u3a::MailAttentionInfo::Unavailable);
    ui.mail_recovery
        .advance(ui.mail_attention, std::time::Duration::ZERO);
    ui.mail_recovery
        .advance(ui.mail_attention, std::time::Duration::from_secs(30));
    ui.composer.insert_str("a draft");
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    let screen = |terminal: &Terminal<TestBackend>| {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    assert!(screen(&terminal).contains("offline"));
    ui.mail_attention = Some(grapt_api::u3a::MailAttentionInfo::Retrying);
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    ui.mail_attention = Some(grapt_api::u3a::MailAttentionInfo::RebindRequired);
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    assert!(screen(&terminal).contains("offline"));
    assert!(!screen(&terminal).contains("retrying"));
    ui.command_pane = Some(crate::command_pane::Pane::new(vec!["STATUS".into()]));
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, false))
        .map_err(|e| e.to_string())?;
    ui.command_pane = None;
    ui.composer.insert_str(" still here");
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    assert!(screen(&terminal).contains("offline"));
    assert_eq!(crate::keys::draft(&ui), "a draft still here");
    assert_eq!(
        ui.status, "Connected",
        "mail availability is independent of the service connection"
    );
    ui.mail_attention = None;
    terminal
        .draw(|frame| presenter.render(&mut ui, frame, true))
        .map_err(|e| e.to_string())?;
    assert!(!screen(&terminal).contains("offline"));
    Ok(())
}

#[test]
fn paused_mail_minute_invalidates_retained_chrome_without_other_input() {
    let mut ui = Ui::new(false);
    ui.mail_attention = Some(grapt_api::u3a::MailAttentionInfo::Stopped);
    ui.mail_recovery
        .advance(ui.mail_attention, std::time::Duration::ZERO);
    let area = Rect::new(0, 0, 80, 24);
    let chrome = Chrome::capture(&ui, area);
    assert!(chrome.matches(&ui, area));
    ui.mail_recovery
        .advance(ui.mail_attention, std::time::Duration::from_secs(60));
    assert!(!chrome.matches(&ui, area));
}
