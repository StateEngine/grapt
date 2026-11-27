use super::*;
use crate::{
    backend::{Backend, Command},
    handle_key,
    ui::Ui,
};
use crossterm::event::{KeyEvent, KeyModifiers};
use grapt_client::{wire::session_id, SessionSummary};
use ratatui::{backend::TestBackend, Terminal};
use std::sync::mpsc;

fn agents() -> Result<Vec<Agent>, Box<dyn std::error::Error>> {
    (0..100)
        .map(|index| {
            Ok(Agent {
                display_name: None,
                context: Default::default(),
                row: SessionSummary {
                    id: session_id(&format!("ses_picker{index}"))?,
                    title: format!("Session{index:03}"),
                    parent: None,
                    detail: Default::default(),
                },
                current: index == 90,
                status: "idle".into(),
            })
        })
        .collect()
}

fn backend() -> (Backend, crate::Commands) {
    let (tx, commands) = crate::command_channel();
    let rx = crate::backend::Handoff::default();
    let (wake, _ready) = mpsc::sync_channel(1);
    (
        Backend {
            commands: tx,
            updates: rx,
            wake,
        },
        commands,
    )
}

#[test]
fn session_selection_stays_visible_after_paging_and_resize(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut ui = Ui::new(false);
    ui.agents = agents()?;
    ui.picker = Some(Picker::new(&ui.agents));
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    for (key, expected) in [
        (None, "Session090"),
        (Some(KeyCode::End), "Session099"),
        (Some(KeyCode::Home), "Session000"),
    ] {
        if let (Some(picker), Some(key)) = (&mut ui.picker, key) {
            picker.key(key, &ui.agents, &ui.session_pins);
        }
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
    if let Some(picker) = &mut ui.picker {
        picker.key(KeyCode::PageDown, &ui.agents, &ui.session_pins);
    }
    let selected = ui
        .picker
        .as_ref()
        .and_then(|picker| picker.selected(&ui.agents, &ui.session_pins))
        .cloned();
    terminal.backend_mut().resize(120, 40);
    terminal.draw(|frame| ui.draw(frame))?;
    assert_eq!(
        selected.as_ref(),
        ui.picker
            .as_ref()
            .and_then(|picker| picker.selected(&ui.agents, &ui.session_pins))
    );
    let expected = ui
        .agents
        .iter()
        .find(|agent| Some(&agent.row.id) == selected.as_ref())
        .map(|agent| &agent.row.title);
    let Some(expected) = expected else {
        panic!("paging lost selection")
    };
    let shown: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(shown.contains(expected));
    Ok(())
}

#[test]
fn escape_dismisses_without_interrupting_or_losing_the_draft(
) -> Result<(), Box<dyn std::error::Error>> {
    let (backend, commands) = backend();
    let mut ui = Ui::new(false);
    ui.agents = agents()?;
    ui.composer.insert_str("saved 👩‍💻 draft");
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(esc, &mut ui, &backend)?;
    assert!(ui.picker.is_some());
    handle_key(esc, &mut ui, &backend)?;
    assert!(ui.picker.is_none());
    assert_eq!(crate::draft(&ui), "saved 👩‍💻 draft");
    assert!(commands.try_recv().is_err());
    ui.agents[90].status = "running".into();
    handle_key(esc, &mut ui, &backend)?;
    assert!(matches!(
        commands.try_recv()?,
        Command::InterruptAndResume(_)
    ));
    handle_key(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut ui,
        &backend,
    )?;
    assert!(matches!(commands.try_recv()?, Command::Commands));
    handle_key(esc, &mut ui, &backend)?;
    assert!(
        commands.try_recv().is_err(),
        "closing an overlay must not interrupt its run"
    );
    Ok(())
}

#[test]
fn refreshed_order_keeps_identity_and_removed_selection_cannot_switch_sessions(
) -> Result<(), Box<dyn std::error::Error>> {
    let (backend, commands) = backend();
    let mut ui = Ui::new(false);
    ui.agents = agents()?;
    let original = ui.agents[90].row.id.clone();
    ui.picker = Some(Picker::new(&ui.agents));
    ui.agents.reverse();
    handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(matches!(commands.try_recv()?, Command::Select(request) if request.id == original));
    assert!(ui.picker.is_none());
    ui.picker = Some(Picker::new(&ui.agents));
    ui.agents.retain(|agent| agent.row.id != original);
    handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(commands.try_recv().is_err());
    assert!(ui.picker.is_some());
    assert!(ui.status.contains("no longer available"));
    Ok(())
}

#[test]
fn typing_and_pasting_filter_sessions_without_editing_the_message(
) -> Result<(), Box<dyn std::error::Error>> {
    let (backend, commands) = backend();
    let mut ui = Ui::new(false);
    ui.agents = agents()?;
    ui.composer.insert_str("keep this draft");
    handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    handle_key(
        KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(!crate::keys::paste("ESSION042", &mut ui));
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| ui.draw(frame))?;
    let shown: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(shown.contains("Session042") && shown.contains("Search: SESSION042"));
    assert!(!shown.contains("Session043"));
    assert_eq!(crate::draft(&ui), "keep this draft");
    handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(
        matches!(commands.try_recv()?, Command::Select(request) if request.id == ui.agents[42].row.id)
    );
    Ok(())
}

#[test]
fn ctrl_u_clears_only_the_session_filter_and_preserves_a_valid_selection(
) -> Result<(), Box<dyn std::error::Error>> {
    for light in [false, true] {
        let (backend, commands) = backend();
        let mut ui = Ui::new(light);
        ui.agents = agents()?.into_iter().take(3).collect();
        let query = format!("界{}", "é".repeat(120));
        ui.agents[1].row.title = query.clone();
        ui.composer.insert_str("saved 👩‍💻 draft");
        let cursor = ui.composer.cursor();
        let clear = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        ui.picker = Some(Picker::new(&ui.agents));
        handle_key(
            KeyEvent::new(KeyCode::Char('界'), KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        crate::keys::paste(&"é".repeat(120), &mut ui);
        let selected = ui.agents[1].row.id.clone();
        assert_eq!(
            ui.picker.as_ref().map(|p| p.query.as_str()),
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
            ui.picker.as_ref().map(|p| p.query.as_str()),
            Some(query.as_str())
        );
        handle_key(clear, &mut ui, &backend)?;
        let picker = ui.picker.as_ref().ok_or("session picker closed")?;
        assert!(picker.query.is_empty());
        assert_eq!(picker.entries(&ui.agents, &ui.session_pins).len(), 3);
        assert_eq!(
            picker.selected(&ui.agents, &ui.session_pins),
            Some(&selected)
        );
        crate::keys::paste(&"界".repeat(85), &mut ui);
        crate::keys::paste("é", &mut ui);
        assert_eq!(ui.picker.as_ref().map(|p| p.query.len()), Some(255));
        assert!(ui.status.contains("too long"));
        handle_key(clear, &mut ui, &backend)?;
        let picker = ui.picker.as_ref().ok_or("session picker closed")?;
        assert!(picker.query.is_empty());
        assert_eq!(picker.entries(&ui.agents, &ui.session_pins).len(), 3);
        assert!(picker.selected(&ui.agents, &ui.session_pins).is_some());
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let shown: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(shown.contains("Session000") && shown.contains("Session002"));
        assert_eq!(crate::draft(&ui), "saved 👩‍💻 draft");
        assert_eq!(ui.composer.cursor(), cursor);
        assert!(
            commands.try_recv().is_err(),
            "filtering cannot select or submit"
        );
        let selected = ui
            .picker
            .as_ref()
            .and_then(|p| p.selected(&ui.agents, &ui.session_pins))
            .cloned();
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert!(
            matches!(commands.try_recv()?, Command::Select(request) if selected.as_ref() == Some(&request.id))
        );
        assert!(commands.try_recv().is_err());
    }
    Ok(())
}

mod management;
