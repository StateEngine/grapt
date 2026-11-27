//! Follow the workspace-preview copy through the real key and command owners.
use super::*;
use crate::backend::CommandId;
use crate::{
    backend::{Backend, Command},
    keys,
    ui::Ui,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::mpsc;

fn enter(
    ui: &mut Ui,
    backend: &Backend,
    received: &crate::Commands,
    client: &mut Client<Peer>,
) -> TestResult {
    assert!(!keys::handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        ui,
        backend
    )?);
    let Command::Submit(text) = received.try_recv()? else {
        return Err("Enter did not dispatch the expected command".into());
    };
    let mut submissions = crate::backend::first_prompt::Submissions::default();
    submissions.submit(client, text, false, CommandId::TEST);
    // Match the protocol worker's pump before publishing the next snapshot.
    // In particular, /commands invalidates its cached catalog for reload.
    client.pump()?;
    submissions.advance(client);
    assert!(
        !submissions.preparing(),
        "fixture submission did not settle"
    );
    if let Some(done) = submissions.command.take().map(|(_, done)| done) {
        ui.command_update(done);
    }
    if let Some((text, accepted, human)) = submissions.outcome.take().map(|(_, outcome)| outcome) {
        assert!(accepted && human, "edited task was not accepted");
        ui.pending = None;
        ui.submission_accepted(&text);
    }
    Ok(())
}

#[test]
fn workspace_preview_guidance_returns_to_an_editable_task_before_enter_sends() -> TestResult {
    let mut client = prepared(true)?;
    client.enable_commands();
    client.pump()?;
    let session = client.selected().cloned();
    let agent = client.agent().cloned();
    let model = client.model().cloned();
    let count = client.peer_mut().inner.session_count();
    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _ready) = mpsc::sync_channel(1);
    let backend = Backend {
        commands,
        updates,
        wake,
    };
    let mut ui = Ui::new(false);
    ui.session_update(session.clone(), false);
    ui.model_update(model.clone(), None, None);

    assert!(keys::paste("/commands", &mut ui));
    enter(&mut ui, &backend, &received, &mut client)?;
    assert!(ui.command_pane.is_some());
    assert!(client.peer_mut().prompts.is_empty());
    keys::handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(ui.command_pane.is_none());
    assert!(received.try_recv().is_err());

    assert!(keys::paste("/focus first second", &mut ui));
    enter(&mut ui, &backend, &received, &mut client)?;
    assert!(ui.command_pane.is_some());
    assert_eq!(keys::draft(&ui), "Read first then second");
    assert!(client.peer_mut().prompts.is_empty());
    assert_eq!(client.peer_mut().inner.session_count(), count);

    keys::handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(ui.command_pane.is_none());
    assert_eq!(keys::draft(&ui), "Read first then second");
    keys::handle_key(
        KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert_eq!(keys::draft(&ui), "Read first then second!");
    assert!(received.try_recv().is_err());
    assert!(client.peer_mut().prompts.is_empty());
    enter(&mut ui, &backend, &received, &mut client)?;
    assert_eq!(client.peer_mut().prompts, ["Read first then second!"]);
    assert_eq!(client.selected(), session.as_ref());
    assert_eq!(client.agent(), agent.as_ref());
    assert_eq!(client.model(), model.as_ref());
    assert!(keys::draft(&ui).is_empty());
    assert!(ui.pending.is_none());
    assert!(received.try_recv().is_err());
    Ok(())
}
