use super::*;
use crate::{
    backend::{Backend, Command},
    handle_key,
    ui::Ui,
};
use crossterm::event::{KeyEvent, KeyModifiers};
use grapt_client::wire::{decode_model_variant, model_id, provider_id};
use ratatui::{backend::TestBackend, Terminal};
use std::sync::mpsc;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn row(index: usize, variant: &str) -> Result<ModelRow, Box<dyn std::error::Error>> {
    Ok(ModelRow {
        id: model_id(&format!("model{index}"))?,
        provider_id: provider_id("provider")?,
        name: format!("Model{index:03}"),
        enabled: index != 0,
        variant: decode_model_variant(format!(r#"{{"model":{{"id":"model{index}","providerID":"provider","variant":"{variant}"}}}}"#).as_bytes())?,
        context_window: Some(200_000),
        default_effort: None,
        capabilities: grapt_client::wire::ModelCapabilities {
            tools: Some(true),
            input_text: Some(true),
            output_text: Some(true),
        },
    })
}

fn backend() -> (Backend, crate::Commands) {
    let (tx, commands) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _ready) = mpsc::sync_channel(1);
    (
        Backend {
            commands: tx,
            updates,
            wake,
        },
        commands,
    )
}

fn press(ui: &mut Ui, backend: &Backend, key: KeyCode) -> Result<(), String> {
    handle_key(KeyEvent::new(key, KeyModifiers::NONE), ui, backend).map(|_| ())
}

#[test]
fn chip_keeps_effort_visible_and_renders_unavailable_without_a_default() -> TestResult {
    let mut model = row(1, "high")?;
    model.name = "Long 中👩🏽‍💻 name ".repeat(30);
    for width in [0, 10, 40, 56] {
        let chip = chip(Some(&model), width);
        assert!(cells::cells(&chip) <= width, "{chip}");
        if width >= 10 {
            assert!(chip.ends_with("high"));
        }
    }
    model.name.clear();
    assert!(chip(Some(&model), 56).contains("model1"));
    assert!(chip(None, 56).contains("unavailable"));
    model.variant = None;
    assert!(!chip(Some(&model), 56).contains("default"));
    Ok(())
}

#[test]
fn model_selection_waits_for_the_result_and_failure_keeps_the_draft() -> TestResult {
    let (backend, commands) = backend();
    let mut ui = Ui::new(false);
    ui.composer.insert_str("unsent 中👩‍💻 draft");
    press(&mut ui, &backend, KeyCode::F(3))?;
    assert!(matches!(commands.try_recv()?, Command::Models));
    assert!(!crate::keys::paste("provider", &mut ui));
    press(&mut ui, &backend, KeyCode::Enter)?;
    assert!(commands.try_recv().is_err(), "loading cannot select");
    let model = row(1, "high")?;
    ui.model_update(Some(model.clone()), Some(vec![model.clone()]), None);
    press(&mut ui, &backend, KeyCode::Enter)?;
    assert!(matches!(commands.try_recv()?, Command::Model(choice) if same(&choice,&model)));
    // An immediate second Enter, Esc or paste cannot submit under the old choice.
    press(&mut ui, &backend, KeyCode::Enter)?;
    press(&mut ui, &backend, KeyCode::Esc)?;
    crate::keys::paste("hidden", &mut ui);
    assert!(commands.try_recv().is_err());
    assert_eq!(crate::draft(&ui), "unsent 中👩‍💻 draft");
    ui.model_update(Some(model.clone()), None, Some(false));
    assert!(ui.models.as_ref().is_some_and(|picker| !picker.pending));
    assert_eq!(ui.model, Some(model.clone()));
    press(&mut ui, &backend, KeyCode::Enter)?;
    assert!(matches!(commands.try_recv()?, Command::Model(_)));
    ui.model_update(Some(model), None, Some(true));
    assert!(ui.models.is_none());
    assert_eq!(crate::draft(&ui), "unsent 中👩‍💻 draft");
    press(&mut ui, &backend, KeyCode::Enter)?;
    assert!(matches!(commands.try_recv()?, Command::Submit(_)));
    Ok(())
}

#[test]
fn disabled_choices_are_visible_and_refused_and_a_command_burst_keeps_the_draft() -> TestResult {
    let (backend, commands) = backend();
    let mut ui = Ui::new(false);
    ui.composer.insert_str("draft");
    press(&mut ui, &backend, KeyCode::F(3))?;
    commands.try_recv()?;
    ui.model_update(None, Some(vec![row(0, "high")?]), None);
    press(&mut ui, &backend, KeyCode::Enter)?;
    assert!(ui.status.contains("disabled"));
    assert!(commands.try_recv().is_err());
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| ui.draw(frame))?;
    let shown: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(shown.contains("disabled"));
    assert!(shown.contains("Model000"));
    press(&mut ui, &backend, KeyCode::Esc)?;
    crate::burst(&backend)?;
    press(&mut ui, &backend, KeyCode::F(3))?;
    assert!(!ui.status.contains("busy"));
    assert_eq!(crate::draft(&ui), "draft");
    assert!(matches!(
        crate::after_burst(&commands)?[..],
        [Command::Models]
    ));
    Ok(())
}

#[test]
fn catalog_paging_search_efforts_and_cancel_preserve_the_editor() -> TestResult {
    for light in [false, true] {
        let (backend, commands) = backend();
        let mut ui = Ui::new(light);
        ui.composer.insert_str("saved draft");
        press(&mut ui, &backend, KeyCode::F(3))?;
        commands.try_recv()?;
        let rows = (0..100)
            .map(|i| row(i, "high"))
            .collect::<Result<Vec<_>, _>>()?;
        ui.model_update(Some(row(90, "high")?), Some(rows), None);
        for (width, height, key, expected) in [
            (80, 24, None, "Model090"),
            (120, 40, Some(KeyCode::End), "Model099"),
            (80, 24, Some(KeyCode::Home), "Model000"),
        ] {
            if let Some(key) = key {
                press(&mut ui, &backend, key)?;
            }
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            terminal.draw(|frame| ui.draw(frame))?;
            let shown: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(shown.contains(expected), "{shown}");
            assert!(shown.contains("Esc close"));
        }
        crate::keys::paste("Model099", &mut ui);
        assert!(!crate::keys::paste(&"q".repeat(257), &mut ui));
        assert!(ui.status.contains("too long"));
        press(&mut ui, &backend, KeyCode::Esc)?;
        assert!(commands.try_recv().is_err());
        assert_eq!(crate::draft(&ui), "saved draft");
    }
    // Effort rows with the same model and provider remain distinct.
    let mut picker = Picker::default();
    let high = row(1, "high")?;
    let low = row(1, "low")?;
    picker.loaded(vec![low.clone(), high.clone()], Some(&high));
    assert_eq!(picker.selected(), Some(&high));
    picker.key(KeyCode::Up);
    assert_eq!(picker.selected(), Some(&low));
    Ok(())
}

#[test]
fn inherited_effort_is_visible_without_overwriting_explicit_selection() -> TestResult {
    let mut model = row(1, "low")?;
    model.default_effort = row(1, "high")?.variant;
    assert!(chip(Some(&model), 80).contains("low"));
    model.variant = None;
    assert!(chip(Some(&model), 80).contains("high"));
    assert!(
        model.variant.is_none(),
        "rendering must not select an effort"
    );
    Ok(())
}

#[test]
fn ctrl_u_clears_only_the_model_filter_and_pending_selection_ignores_it() -> TestResult {
    for light in [false, true] {
        let (backend, commands) = backend();
        let mut ui = Ui::new(light);
        ui.composer.insert_str("saved 👩‍💻 draft");
        let cursor = ui.composer.cursor();
        let first = row(1, "high")?;
        let mut chosen = row(2, "low")?;
        let query = format!("界{}", "é".repeat(120));
        chosen.name = query.clone();
        press(&mut ui, &backend, KeyCode::F(3))?;
        assert!(matches!(commands.try_recv()?, Command::Models));
        ui.model_update(Some(first.clone()), Some(vec![first, chosen.clone()]), None);
        press(&mut ui, &backend, KeyCode::Char('界'))?;
        crate::keys::paste(&"é".repeat(120), &mut ui);
        assert_eq!(
            ui.models.as_ref().map(|p| p.query.as_str()),
            Some(query.as_str())
        );
        handle_key(
            KeyEvent::new(
                KeyCode::Char('u'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
            &mut ui,
            &backend,
        )?;
        assert_eq!(
            ui.models.as_ref().map(|p| p.query.as_str()),
            Some(query.as_str())
        );
        let clear = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        handle_key(clear, &mut ui, &backend)?;
        let picker = ui.models.as_ref().ok_or("model picker closed")?;
        assert!(picker.query.is_empty());
        assert_eq!(picker.entries().len(), 2);
        assert_eq!(picker.selected(), Some(&chosen));
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let shown: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(shown.contains("Ctrl+U clear filter") && shown.contains("Model001"));
        crate::keys::paste(&"界".repeat(85), &mut ui);
        crate::keys::paste("é", &mut ui);
        assert_eq!(ui.models.as_ref().map(|p| p.query.len()), Some(255));
        assert!(ui.status.contains("too long"));
        handle_key(clear, &mut ui, &backend)?;
        let picker = ui.models.as_ref().ok_or("model picker closed")?;
        assert!(picker.query.is_empty());
        assert_eq!(picker.entries().len(), 2);
        assert!(picker.selected().is_some());
        assert!(
            commands.try_recv().is_err(),
            "filtering cannot select or submit"
        );
        press(&mut ui, &backend, KeyCode::Enter)?;
        assert!(matches!(commands.try_recv()?, Command::Model(_)));
        handle_key(clear, &mut ui, &backend)?;
        press(&mut ui, &backend, KeyCode::Enter)?;
        assert!(ui
            .models
            .as_ref()
            .is_some_and(|p| p.pending && p.query.is_empty()));
        assert!(
            commands.try_recv().is_err(),
            "pending choice cannot select twice"
        );
        assert_eq!(crate::draft(&ui), "saved 👩‍💻 draft");
        assert_eq!(ui.composer.cursor(), cursor);
    }
    Ok(())
}

#[test]
fn an_unreported_tool_surface_reads_as_unknown_not_as_a_denial() -> TestResult {
    let mut known = row(1, "default")?;
    assert_eq!(tools(&known), "tools");
    known.capabilities.tools = Some(false);
    assert_eq!(tools(&known), "no tools");
    known.capabilities.tools = None;
    assert_eq!(tools(&known), "tools —");
    Ok(())
}
