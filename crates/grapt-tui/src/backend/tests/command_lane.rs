//! A permission reply and a Session open through the running protocol loop.
use crate::backend::tests::boundary_fixture::{until, Fixture, Result};
use crate::backend::{Command, Outcome, Selection};
use grapt_client::{
    client::HistoryMove,
    wire::{PermissionReplyChoice, SessionWireId},
    Bounds, Client, ContractDouble, FramePoll, Operation, Transport, WireRequest,
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
fn a_prompt_that_pages_to_the_tail_keeps_the_page_move_before_it() -> Result {
    let fixture = Fixture::ready()?;
    held_reply(&fixture)?;
    // Both wait behind the reply, then run in one pass and one publication.
    let browse = fixture.command(Command::Browse(HistoryMove::Latest))?;
    let prompt = fixture.command(Command::Submit("sent behind the page move".into()))?;
    fixture.gate.release();
    let update =
        fixture.take_until(|update| update.navigation.iter().any(|(id, ..)| *id == prompt))?;
    let ids: Vec<_> = update.navigation.iter().map(|(id, ..)| *id).collect();
    assert_eq!(ids, [browse, prompt]);
    Ok(())
}

#[test]
fn a_prompt_refused_behind_a_pending_stop_settles_its_page_request() -> Result {
    let fixture = Fixture::ready()?;
    fixture.gate.hold(Operation::SessionInterrupt)?;
    fixture.command(Command::InterruptSession(fixture.session.clone()))?;
    until(|| fixture.gate.blocked())?;
    let prompt = fixture.command(Command::Submit("refused behind the stop".into()))?;
    fixture.take_until(|update| update.navigation_dropped.contains(&prompt))?;
    // Paging still answers afterwards.
    fixture.gate.release();
    let browse = fixture.command(Command::Browse(HistoryMove::Latest))?;
    fixture.take_until(|update| update.navigation.iter().any(|(id, ..)| *id == browse))?;
    Ok(())
}

/// A command that reaches the service, built against a ready fixture, and
/// the operation its first request uses.
type LaneCommand = fn(&Fixture) -> Result<(Operation, Command)>;

#[test]
fn every_lane_command_held_at_the_service_never_holds_the_protocol_loop() -> Result {
    let commands: [LaneCommand; 7] = [
        |_| {
            Ok((
                Operation::MessageList,
                Command::Browse(HistoryMove::Beginning),
            ))
        },
        |fixture| {
            fixture.command(Command::Roster)?;
            let roster = fixture.take_until(|update| update.roster.is_some())?.roster;
            let agent = roster
                .and_then(|rows| rows.first().cloned())
                .ok_or("agent")?;
            Ok((
                Operation::SessionAgentSelect,
                Command::ChooseAgent(agent.id),
            ))
        },
        |fixture| {
            fixture.command(Command::Models)?;
            let models = fixture.take_until(|update| update.models.is_some())?.models;
            let model = models.and_then(|rows| rows.into_iter().find(|row| row.enabled));
            Ok((
                Operation::SessionModelSelect,
                Command::Model(model.ok_or("model")?),
            ))
        },
        |_| {
            Ok((
                Operation::SessionList,
                Command::SessionSearch("held".into()),
            ))
        },
        |fixture| {
            let request = crate::sessions::Request::Rename {
                id: fixture.session.clone(),
                title: "held rename".into(),
            };
            Ok((Operation::SessionRename, Command::ManageSession(request)))
        },
        |fixture| {
            let mut source = fixture.source.lock().map_err(|_| "source lock")?;
            source.raise_form(&fixture.session, "Held").ok_or("form")?;
            let list = WireRequest::new(Operation::SessionFormList, Some(&fixture.session), None)?;
            let rows = grapt_client::wire::decode_session_forms(&source.call(&list)?.body)?;
            let request = grapt_tui_forms::Request {
                target: grapt_tui_forms::Target::Session(fixture.session.clone()),
                form: rows.into_iter().next().ok_or("form row")?,
                decision: Some(grapt_tui_forms::Decision::Cancel),
            };
            Ok((Operation::SessionFormState, Command::Form(request)))
        },
        |fixture| {
            let grant = serde_json::from_value(
                serde_json::json!({"scope": "session", "resource": "/workspace/held.txt"}),
            )?;
            let request = crate::grants::Request {
                session: fixture.session.clone(),
                grant,
            };
            Ok((Operation::SandboxGrantRevoke, Command::RevokeGrant(request)))
        },
    ];
    for build in commands {
        let fixture = Fixture::ready()?;
        let (operation, command) = build(&fixture)?;
        fixture.gate.hold(operation)?;
        let id = fixture.command(command)?;
        until(|| fixture.gate.blocked())?;
        let held = Instant::now();
        for frame in fixture.turn("streamed while the command is held")? {
            fixture.frame(frame)?;
        }
        fixture.take_until(|update| update.completion.is_some())?;
        assert!(held.elapsed() < Duration::from_secs(3), "{operation:?}");
        assert!(fixture.gate.blocked(), "{operation:?} was still held");
        fixture.gate.release();
        fixture.take_until(|update| {
            update.outcomes.iter().any(|settled| settled.id == id)
                || update.navigation.iter().any(|(moved, ..)| *moved == id)
                || update.models.is_some()
        })?;
    }
    Ok(())
}

#[test]
fn a_page_move_in_flight_settles_after_a_quit_and_publications() -> Result {
    let fixture = Fixture::ready()?;
    fixture.gate.hold(Operation::MessageList)?;
    let browse = fixture.command(Command::Browse(HistoryMove::Beginning))?;
    until(|| fixture.gate.blocked())?;
    // Held behind the page move, then discarded by the quit.
    let submit = fixture.command(Command::Submit("discarded behind the page move".into()))?;
    fixture.barrier()?;
    let mut dropped = Vec::new();
    fixture.take_until(|update| {
        dropped.extend(update.navigation_dropped.iter().copied());
        dropped.contains(&submit)
    })?;
    fixture.gate.release();
    fixture.take_until(|update| {
        dropped.extend(update.navigation_dropped.iter().copied());
        update.navigation.iter().any(|(moved, ..)| *moved == browse)
    })?;
    assert!(
        !dropped.contains(&browse),
        "the page move in flight was dropped"
    );
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
