use super::*;
use crate::backend::{Backend, Command};
use std::sync::mpsc;

#[test]
fn palette_and_help_use_real_keys_without_submitting_or_replacing_a_draft() -> Result<(), String> {
    let (sender, commands) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands: sender,
        updates,
        wake,
    };
    let mut ui = Ui::new(false);
    ui.composer.insert_str("keep this draft");
    crate::keys::handle_key(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut ui,
        &backend,
    )?;
    assert!(ui.command_palette.is_some());
    assert!(ui.picker.is_none());
    assert!(matches!(commands.try_recv(), Ok(Command::Commands)));
    assert!(!crate::keys::paste("rename", &mut ui));
    crate::keys::handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(
        ui.command_palette.is_some(),
        "a disabled command cannot replace the draft"
    );
    assert_eq!(crate::keys::draft(&ui), "keep this draft");
    crate::keys::handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    crate::keys::handle_key(
        KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(ui.command_pane.is_some());
    assert!(
        commands.try_recv().is_err(),
        "UI help does not submit model work"
    );
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
        .map_err(|e| e.to_string())?;
    terminal.draw(|f| ui.draw(f)).map_err(|e| e.to_string())?;
    assert_eq!(crate::keys::draft(&ui), "keep this draft");
    Ok(())
}

#[test]
fn aliases_are_searchable_and_colliding_custom_commands_keep_an_explicit_name() {
    let ui = Ui::new(false);
    let offered = entries(&ui, "/models");
    assert!(offered
        .iter()
        .any(|r| matches!(r.target, Target::Action(Action::Models))));
    let row = grapt_client::wire::CommandTemplate {
        claude: None,
        name: "models".into(),
        description: "custom model report".into(),
        template: "report".into(),
        unsupported: false,
        origin: grapt_client::wire::CommandOrigin::Project,
        source: "/work/.grapt/commands/models.md".into(),
        unavailable: None,
        mcp: None,
    };
    assert_eq!(actions::command_name(&row), "project:models");
    assert!(actions::canonical("/project:models report").is_none());
    assert!(actions::canonical("//models").is_none());
    assert!(actions::canonical("/modelz").is_none());
    assert_eq!(
        actions::canonical(" /models provider/model effort ").as_deref(),
        Some("/model provider/model effort")
    );
}

#[test]
fn action_routing_passes_a_command_burst_and_transfers_question_focus() -> Result<(), String> {
    let (sender, commands) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands: sender,
        updates,
        wake,
    };
    crate::burst(&backend)?;
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    for command in ["/model", "/models", "/agents"] {
        let mut ui = Ui::new(false);
        ui.composer.insert_str(command);
        assert!(!crate::keys::handle_key(enter, &mut ui, &backend)?);
        assert!(!ui.status.contains("busy"), "{command}: {}", ui.status);
    }
    let rest = crate::after_burst(&commands)?;
    assert!(matches!(
        rest[..],
        [Command::Models, Command::Models, Command::Roster]
    ));
    for overlay in 0..3 {
        let mut ui = Ui::new(false);
        ui.composer.insert_str("keep draft");
        let session =
            grapt_client::wire::session_id("ses_action_routing").map_err(|e| e.to_string())?;
        ui.forms.update(
            Some(session.clone()),
            None,
            vec![grapt_client::wire::FormRow {
                metadata: Default::default(),
                id: grapt_client::wire::form_id("frm_action_routing").map_err(|e| e.to_string())?,
                owner: grapt_client::wire::FormOwner::Session(session),
                title: "Answer this question".into(),
                fields: Vec::new(),
            }],
        );
        ui.forms.focused = false;
        match overlay {
            0 => ui.command_pane = Some(crate::command_pane::Pane::new(vec!["help".into()])),
            1 => ui.picker = Some(crate::sessions::Picker::new(&[])),
            _ => ui.models = Some(crate::models::Picker::default()),
        }
        crate::keys::handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut ui,
            &backend,
        )?;
        commands.try_recv().map_err(|e| e.to_string())?;
        crate::keys::paste("Answer pending questions", &mut ui);
        assert!(!crate::keys::handle_key(enter, &mut ui, &backend)?);
        assert!(ui.forms.focused);
        assert!(ui.command_pane.is_none() && ui.picker.is_none() && ui.models.is_none());
        assert_eq!(crate::keys::draft(&ui), "keep draft");
        crate::keys::handle_key(
            KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert!(
            !ui.forms.focused,
            "the questions shortcut still toggles focus off"
        );
    }
    Ok(())
}
