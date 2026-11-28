//! A permission reply and a Session open through the running protocol loop.
use crate::backend::tests::boundary_fixture::{until, Fixture, Result};
use crate::backend::{Command, Selection};
use grapt_client::{
    wire::{PermissionReplyChoice, SessionWireId},
    Bounds, Client, ContractDouble, FramePoll, Operation,
};
use std::time::Duration;
use tokio::time::Instant;

/// Answer an open ask and hold the reply's answer at the service.
fn held_reply(fixture: &Fixture) -> Result {
    fixture
        .source
        .lock()
        .map_err(|_| "source lock")?
        .ask_permission(&fixture.session, "Held reply question")
        .ok_or("permission")?;
    fixture.hint(Operation::PermissionRequestList);
    let ask = fixture
        .take_until(|update| update.permission.is_some())?
        .permission
        .ok_or("ask")?;
    fixture.gate.hold(Operation::SessionPermissionReply)?;
    fixture.command(Command::Reply(ask, PermissionReplyChoice::Once))?;
    until(|| fixture.gate.blocked())
}

#[test]
fn a_held_permission_reply_never_holds_the_protocol_loop() -> Result {
    let fixture = Fixture::ready()?;
    held_reply(&fixture)?;
    let held = Instant::now();
    // While the reply's answer is held, the loop still pumps and publishes.
    for frame in fixture.turn("streamed while the reply is held")? {
        fixture.frame(frame)?;
    }
    fixture.take_until(|update| update.completion.is_some())?;
    // Well inside the HTTP allowance a blocked loop would wait out.
    assert!(
        held.elapsed() < Duration::from_secs(3),
        "{:?}",
        held.elapsed()
    );
    assert!(fixture.gate.blocked(), "the reply was still held");
    fixture.gate.release();
    fixture.take_until(|update| update.permission.is_none())?;
    assert_eq!(fixture.gate.calls(Operation::SessionPermissionReply)?, 1);
    Ok(())
}

#[test]
fn quit_is_acknowledged_while_a_reply_is_held() -> Result {
    let fixture = Fixture::ready()?;
    held_reply(&fixture)?;
    let held = Instant::now();
    fixture.barrier()?;
    assert!(
        held.elapsed() < Duration::from_secs(1),
        "{:?}",
        held.elapsed()
    );
    assert!(fixture.gate.blocked(), "the reply was still held");
    Ok(())
}

#[test]
fn a_held_session_open_keeps_the_old_session_live_until_it_installs() -> Result {
    let fixture = Fixture::ready()?;
    let other = located_session(&fixture)?;
    fixture.gate.hold(Operation::SessionGet)?;
    let id = fixture.command(Command::Select(Selection { id: other.clone() }))?;
    until(|| fixture.gate.blocked())?;
    let held = Instant::now();
    // The turn's settlement reads the old Session again; keep it out.
    for frame in fixture.turn("streamed while the open is held")? {
        if !settles(&frame) {
            fixture.frame(frame)?;
        }
    }
    let live = fixture.take_until(|update| !update.rows.is_empty())?;
    assert!(
        held.elapsed() < Duration::from_secs(3),
        "{:?}",
        held.elapsed()
    );
    assert_eq!(live.selected.as_ref(), Some(&fixture.session));
    fixture.gate.release();
    fixture.take_until(|update| {
        update.selection == Some(id) && update.selected.as_ref() == Some(&other)
    })?;
    Ok(())
}

fn settles(frame: &FramePoll) -> bool {
    let (FramePoll::Frame(text) | FramePoll::Positioned { data: text, .. }) = frame else {
        return false;
    };
    ["succeeded", "failed", "interrupted", "session.idle"]
        .iter()
        .any(|kind| text.contains(kind))
}

/// A second Session in the fixture's project, created as the fixture's own.
fn located_session(fixture: &Fixture) -> Result<SessionWireId> {
    let mut source = fixture.source.lock().map_err(|_| "source lock")?;
    let mut client = Client::new(
        std::mem::replace(&mut *source, ContractDouble::new()),
        Bounds::PRODUCTION,
    );
    let created = (|| {
        client.refresh_projects()?;
        let project = client.projects().first().ok_or("project")?.clone();
        client.select_project(&project)?;
        let agent = client.agents().first().ok_or("agent")?.id.clone();
        client.select_agent(&agent)?;
        let model = client.models().first().ok_or("model")?.clone();
        client.select_model(&model)?;
        Ok::<_, Box<dyn std::error::Error>>(client.create_session()?)
    })();
    *source = std::mem::replace(client.peer_mut(), ContractDouble::new());
    created
}
