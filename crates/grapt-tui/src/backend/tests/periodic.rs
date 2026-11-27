//! Held periodic answers through the running protocol loop and UI hand-off.
use crate::backend::tests::boundary_fixture::{until, Fixture, Result};
use grapt_client::{wire::Spelled, FramePoll, Operation, Transport, WireRequest};
use std::time::Duration;
use tokio::time::Instant;
fn install(
    transcript: &mut grapt_tui_transcript::transcript::Transcript,
    update: crate::backend::Update,
) {
    transcript.update(update.reset, update.start, update.rows);
    for (index, row) in update.patches {
        transcript.update(false, index, vec![row]);
    }
    for _ in 0..50 {
        transcript.drain();
    }
}
fn text(transcript: &mut grapt_tui_transcript::transcript::Transcript) -> String {
    transcript
        .visible(
            240,
            1000,
            grapt_tui_transcript::palette::Palette::new(false),
        )
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}
/// Hold one periodic read for `hold` while a stream runs. Under the HTTP
/// socket allowance the held answer applies on one call; past it, the read
/// times out, its retry answers, and the result still applies once.
fn held_stream(operation: Operation, hold: Duration, calls: usize) -> Result {
    let fixture = Fixture::ready()?;
    let mut transcript = grapt_tui_transcript::transcript::Transcript::new();
    install(&mut transcript, fixture.take()?);
    {
        let mut source = fixture.source.lock().map_err(|_| "source lock")?;
        if operation == Operation::PermissionRequestList {
            source
                .ask_permission(&fixture.session, "Held read question")
                .ok_or("permission")?;
        } else {
            let request = WireRequest::new(Operation::SessionRename, Some(&fixture.session), None)?
                .with_body(grapt_client::wire::encode_rename("Held catalog title")?);
            assert_eq!(source.call(&request)?.status, 204);
        }
    }
    let before = fixture.gate.calls(operation)?;
    fixture.gate.hold(operation)?;
    if calls > 1 {
        fixture.gate.permit_timed_out_answer()?;
    }
    fixture.hint(operation);
    until(|| fixture.gate.blocked())?;
    let held = Instant::now();
    let reply: String = (0..500).map(|i| format!("periodic{i}中 ")).collect();
    // Keep settlement after the held-read assertions: terminal reconciliation
    // legitimately performs its own authority read on another lane.
    let mut terminal = Vec::new();
    for frame in fixture.turn(&reply)? {
        let settles = match &frame {
            FramePoll::Frame(text) => serde_json::from_str::<serde_json::Value>(text)?["type"]
                .as_str()
                .is_some_and(|kind| {
                    matches!(
                        kind,
                        "session.execution.succeeded"
                            | "session.execution.failed"
                            | "session.execution.interrupted"
                            | "session.idle"
                    )
                }),
            _ => false,
        };
        if settles {
            terminal.push(frame);
        } else {
            fixture.frame(frame)?;
        }
    }
    loop {
        install(&mut transcript, fixture.take()?);
        if text(&mut transcript).contains("periodic499中") {
            break;
        }
        if held.elapsed() >= hold {
            return Err("transcript did not advance during the read hold".into());
        }
    }
    assert!(
        fixture.gate.blocked(),
        "transcript must advance before the read is released"
    );
    assert!(held.elapsed() < hold);
    while held.elapsed() < hold {
        std::thread::sleep(Duration::from_millis(2).min(hold.saturating_sub(held.elapsed())));
    }
    let elapsed = held.elapsed();
    fixture.gate.release();
    let settled = fixture.take_until(|update| {
        if operation == Operation::PermissionRequestList {
            update
                .permission
                .as_ref()
                .is_some_and(|ask| ask.title == "Held read question")
        } else {
            update.agents.iter().any(|agent| {
                agent.row.id == fixture.session && agent.row.title == "Held catalog title"
            })
        }
    })?;
    assert_eq!(
        fixture.gate.calls(operation)?,
        before + calls,
        "the held answer must apply on exactly the expected reads: {}",
        settled.status
    );
    let shown = settled
        .agents
        .iter()
        .filter(|agent| agent.row.id == fixture.session)
        .count();
    assert_eq!(shown, 1, "the answer applies exactly once");
    eprintln!(
        "held {operation:?} for {}ms; transcript advanced before release; {calls} periodic HTTP calls before terminal reconciliation",
        elapsed.as_millis()
    );
    for frame in terminal {
        fixture.frame(frame)?;
    }
    fixture.take_until(|update| {
        update.observed_running == Some(false) && update.completion.is_some()
    })?;
    Ok(())
}
const UNDER_ALLOWANCE: Duration = Duration::from_secs(4);
const PAST_ALLOWANCE: Duration = Duration::from_millis(6_500);
#[test]
fn review_periodic_permissions_held_four_seconds_do_not_hold_the_transcript() -> Result {
    held_stream(Operation::PermissionRequestList, UNDER_ALLOWANCE, 1)
}
#[test]
fn review_periodic_catalog_held_four_seconds_does_not_hold_the_transcript() -> Result {
    held_stream(Operation::SessionList, UNDER_ALLOWANCE, 1)
}
#[test]
fn review_periodic_permissions_held_past_the_allowance_apply_once() -> Result {
    held_stream(Operation::PermissionRequestList, PAST_ALLOWANCE, 2)
}
#[test]
fn review_periodic_catalog_held_past_the_allowance_applies_once() -> Result {
    held_stream(Operation::SessionList, PAST_ALLOWANCE, 2)
}
#[test]
fn review_periodic_permission_hints_coalesce_while_one_answer_is_held() -> Result {
    let fixture = Fixture::ready()?;
    let operation = Operation::PermissionRequestList;
    let before = fixture.gate.calls(operation)?;
    fixture.gate.hold(operation)?;
    fixture.hint(operation);
    until(|| fixture.gate.blocked())?;
    for _ in 0..20 {
        fixture.hint(operation);
    }
    assert_eq!(fixture.gate.calls(operation)?, before + 1);
    fixture.gate.release();
    until(|| {
        fixture
            .gate
            .calls(operation)
            .is_ok_and(|calls| calls == before + 2)
    })?;
    until(|| fixture.backend.updates.pass_age().is_none())?;
    assert_eq!(fixture.gate.calls(operation)?, before + 2);
    Ok(())
}
fn form_hint(fixture: &Fixture, sequence: u64) -> Result {
    let form = {
        let mut source = fixture.source.lock().map_err(|_| "source lock")?;
        source
            .raise_form(&fixture.session, "Read while busy")
            .ok_or("form")?;
        let request = WireRequest::new(Operation::SessionFormList, Some(&fixture.session), None)?;
        let body: serde_json::Value = serde_json::from_slice(&source.call(&request)?.body)?;
        body["data"]
            .as_array()
            .and_then(|rows| rows.last())
            .ok_or("form row")?
            .clone()
    };
    let frame = serde_json::json!({"id":format!("evt_periodic_{sequence}"),"type":"form.created","created":100,"durable":{"version":1,"seq":sequence,"aggregateID":fixture.session.spelled()},"data":{"sessionID":fixture.session.spelled(),"form":form}}).to_string();
    assert!(
        grapt_client::wire::session_form_changed(&frame, &fixture.session),
        "fixture hint must validate"
    );
    fixture.frame(FramePoll::Frame(frame))
}
#[test]
fn review_periodic_a_form_hint_while_its_read_is_busy_does_not_spin_the_loop() -> Result {
    let fixture = Fixture::ready()?;
    fixture.gate.hold(Operation::SessionFormList)?;
    form_hint(&fixture, 100)?;
    until(|| fixture.gate.blocked())?;
    for _ in fixture.ui_ready.try_iter() {}
    form_hint(&fixture, 101)?;
    let began = Instant::now();
    let mut wakes = 0;
    while began.elapsed() < Duration::from_millis(100) {
        wakes += fixture.ui_ready.try_iter().count();
        std::thread::yield_now();
    }
    fixture.gate.release();
    assert!(wakes <= 16, "an in-flight form hint must not turn the wait into a zero-delay loop; {wakes} pass-start wakes in 100ms");
    Ok(())
}
