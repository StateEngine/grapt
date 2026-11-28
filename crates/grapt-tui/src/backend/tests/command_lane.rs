//! A permission reply and a Session open through the running protocol loop.
use crate::backend::tests::boundary_fixture::{until, Fixture, Result};
use crate::backend::{Command, Outcome, Selection};
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
fn quit_is_acknowledged_behind_a_held_reply_and_a_full_queue() -> Result {
    let fixture = Fixture::ready()?;
    held_reply(&fixture)?;
    for _ in 0..20 {
        fixture.command(Command::Models)?;
    }
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
fn a_submit_held_when_quit_arrives_never_reaches_the_service() -> Result {
    let fixture = Fixture::ready()?;
    held_reply(&fixture)?;
    let inputs = || -> Result<usize> {
        let source = fixture.source.lock().map_err(|_| "source lock")?;
        Ok(source.user_inputs(&fixture.session).len())
    };
    let before = inputs()?;
    fixture.command(Command::Submit("held behind the reply".into()))?;
    fixture.barrier()?;
    fixture.gate.release();
    fixture.take_until(|update| update.permission.is_none())?;
    // Anything still held would run and reach the service within this window.
    let settled = Instant::now();
    while settled.elapsed() < Duration::from_secs(1) {
        assert_eq!(inputs()?, before, "a held submit reached the service");
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[test]
fn commands_a_quit_discards_are_settled_and_later_sends_still_work() -> Result {
    let fixture = Fixture::ready()?;
    held_reply(&fixture)?;
    let submit = fixture.command(Command::Submit("discarded by the quit".into()))?;
    let select = fixture.command(Command::Select(Selection {
        id: fixture.session.clone(),
    }))?;
    fixture.barrier()?;
    let (mut refused, mut answered) = (false, false);
    fixture.take_until(|update| {
        refused |= update.outcomes.iter().any(|settled| {
            settled.id == submit && matches!(&settled.outcome, Outcome::Submission(_, false, true))
        });
        answered |= update.selection == Some(select);
        refused && answered
    })?;
    // The terminal stayed open: the next prompt is sent as usual.
    fixture.gate.release();
    fixture.take_until(|update| update.permission.is_none())?;
    let next = fixture.command(Command::Submit("sent after the close".into()))?;
    fixture.take_until(|update| {
        update.outcomes.iter().any(|settled| {
            settled.id == next && matches!(&settled.outcome, Outcome::Submission(_, true, _))
        })
    })?;
    Ok(())
}

#[test]
fn a_pending_quit_runs_before_a_model_refresh_that_would_block() -> Result {
    let fixture = Fixture::paused()?;
    fixture.gate.hold(Operation::SessionGet)?;
    fixture.backend.updates.request_refresh();
    let asked = Instant::now();
    fixture.barrier()?;
    assert!(
        asked.elapsed() < Duration::from_secs(1),
        "{:?}",
        asked.elapsed()
    );
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
