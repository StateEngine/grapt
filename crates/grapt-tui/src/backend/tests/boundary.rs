//! The UI/protocol hand-off through its queue and transcript consumers.
use crate::backend::tests::boundary_fixture::{until, Fixture, Result};
use crate::backend::{heartbeat, Command, Failure, Handoff, Link, LAG_AFTER};
use grapt_client::{client::ConnectionStatus, supervise::Connection, FramePoll};
use std::{sync::mpsc, time::Duration};

#[test]
fn review_boundary_keeps_occupied_views_and_orders_failure_after_them() -> Result {
    let fixture = Fixture::start()?;
    let first = fixture.take()?;
    let second = fixture.turn("another view")?;
    for frame in second {
        fixture.frame(frame)?;
    }
    let slot = Handoff::default();
    slot.put(first)?;
    let next = fixture.take()?;
    assert!(slot.put(next).is_err());
    slot.fail(Failure {
        message: "fixture ended".into(),
        unattached: false,
    });
    assert!(matches!(slot.take(), Some(Ok(_))));
    assert!(matches!(slot.take(), Some(Err(failure)) if failure.message == "fixture ended"));
    assert!(slot.take().is_none());
    Ok(())
}

fn transcript(stalled: bool) -> Result<String> {
    let fixture = Fixture::start()?;
    let mut transcript = grapt_tui_transcript::transcript::Transcript::new();
    let reply: String = (0..500).map(|index| format!("word{index}中 ")).collect();
    let frames = fixture.turn(&reply)?;
    assert!(frames.len() >= 500);
    let mut built = 0;
    let mut settled = false;
    for frame in frames {
        fixture.frame(frame)?;
        if !stalled {
            if let Some(update) = fixture.backend.take_update() {
                let update = update.map_err(|failure| failure.message)?;
                settled = update.completion.is_some() && update.observed_running == Some(false);
                install(&mut transcript, update);
                built += 1;
            }
        }
    }
    let began = tokio::time::Instant::now();
    while !settled {
        let update = fixture
            .take()
            .map_err(|error| format!("stalled={stalled}: {error}"))?;
        settled = update.completion.is_some() && update.observed_running == Some(false);
        install(&mut transcript, update);
        built += 1;
        if settled {
            break;
        }
        if began.elapsed() > Duration::from_secs(10) {
            return Err("stream did not settle".into());
        }
    }
    for _ in 0..50 {
        transcript.drain();
    }
    let text = transcript
        .visible(
            240,
            1_000,
            grapt_tui_transcript::palette::Palette::new(false),
        )
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("word0中") && text.contains("word499中"),
        "{text}"
    );
    assert!(
        text.contains("Ran command"),
        "mixed tool row missing: {text}"
    );
    assert!(built > 0);
    Ok(text)
}
fn install(
    transcript: &mut grapt_tui_transcript::transcript::Transcript,
    update: crate::backend::Update,
) {
    transcript.update(update.reset, update.start, update.rows);
    for (index, row) in update.patches {
        transcript.update(false, index, vec![row]);
    }
}

#[test]
fn review_boundary_stalled_and_draining_ui_finish_with_identical_transcripts() -> Result {
    assert_eq!(transcript(false)?, transcript(true)?);
    Ok(())
}

#[test]
fn review_boundary_terminal_is_published_after_the_pending_view_without_another_frame() -> Result {
    let fixture = Fixture::start()?;
    let frames = fixture.turn("finished while UI held a view")?;
    assert!(!fixture.backend.updates.vacant());
    for frame in frames {
        fixture.frame(frame)?;
    }
    let old = fixture.take()?;
    assert!(!old
        .rows
        .iter()
        .any(|row| row.text.contains("finished while UI")));
    let next = fixture.take_until(|update| {
        update.completion.is_some() && update.observed_running == Some(false)
    })?;
    assert_eq!(next.observed_running, Some(false));
    assert_eq!(
        next.completion.as_ref().map(|(_, outcome)| outcome),
        Some(&grapt_client::Outcome::Succeeded)
    );
    assert!(next
        .rows
        .iter()
        .any(|row| row.text.contains("finished while UI held a view")));
    Ok(())
}

#[test]
fn review_boundary_idle_is_published_after_the_pending_view_without_another_frame() -> Result {
    let fixture = Fixture::start()?;
    fixture.frame(FramePoll::Frame(
        serde_json::json!({
            "id":"evt_idle_pending", "type":"session.idle", "created":999999,
            "data":{"sessionID":grapt_client::wire::Spelled::spelled(&fixture.session)}
        })
        .to_string(),
    ))?;
    let _ = fixture.take()?;
    assert_eq!(
        fixture
            .take_until(|update| update.observed_running == Some(false))?
            .observed_running,
        Some(false)
    );
    Ok(())
}

#[test]
fn review_boundary_accepts_and_accounts_for_commands_without_a_draining_backend() -> Result {
    let (commands, received) = crate::command_channel();
    let (wake, _ready) = mpsc::sync_channel(1);
    let backend = crate::backend::Backend {
        commands,
        updates: Handoff::default(),
        wake,
    };
    for index in 0..100 {
        crate::send(&backend, Command::SessionSearch(format!("query-{index}")))?;
    }
    assert_eq!(backend.updates.intake(), 100);
    for index in 0..100 {
        let Command::SessionSearch(query) = received.try_recv()? else {
            return Err("command kind".into());
        };
        assert_eq!(query, format!("query-{index}"));
        backend.updates.received();
    }
    assert_eq!(backend.updates.intake(), 0);
    assert!(received.try_recv().is_err());
    drop(received);
    assert!(crate::send(&backend, Command::Models).is_err());
    assert_eq!(backend.updates.intake(), 0);
    Ok(())
}

#[test]
fn review_boundary_a_single_coalesced_wake_drains_commands_while_the_ui_holds_a_view() -> Result {
    let fixture = Fixture::paused()?;
    fixture.barrier()?;
    until(|| fixture.backend.updates.pass_age().is_none())?;
    for _ in 0..100 {
        fixture.backend.updates.handed();
        let command = fixture.backend.updates.envelope(Command::Models);
        fixture.backend.commands.send(command)?;
    }
    let result = fixture.barrier();
    let remaining = fixture.backend.updates.intake();
    assert_eq!(
        remaining, 0,
        "accepted commands stranded behind the occupied view; barrier={result:?}"
    );
    result
}

#[test]
fn review_boundary_refreshes_coalesce_and_heartbeat_uses_only_link_and_loop_liveness() {
    let handoff = Handoff::default();
    for _ in 0..50 {
        handoff.request_refresh();
    }
    assert!(handoff.refresh_requested());
    assert!(!handoff.refresh_requested());
    for (status, state, want) in [
        (ConnectionStatus::Live, Connection::Live, Link::Connected),
        (
            ConnectionStatus::Reconnecting,
            Connection::Replacing { attempt: 3 },
            Link::Reconnecting(Some(3)),
        ),
        (
            ConnectionStatus::Reconnecting,
            Connection::Live,
            Link::Reconnecting(None),
        ),
        (ConnectionStatus::Offline, Connection::Live, Link::Offline),
    ] {
        assert_eq!(Link::of(status, &state), want);
    }
    assert_eq!(heartbeat(Link::Connected, None, 100), "connected");
    assert_eq!(
        heartbeat(
            Link::Connected,
            Some(LAG_AFTER - Duration::from_millis(1)),
            100
        ),
        "connected"
    );
    assert_eq!(heartbeat(Link::Connected, Some(LAG_AFTER), 8), "lagged");
    assert_eq!(
        heartbeat(Link::Connected, Some(LAG_AFTER), 9),
        "lagged · 9 commands waiting"
    );
    assert_eq!(
        heartbeat(Link::Reconnecting(Some(2)), Some(LAG_AFTER), 9),
        "reconnecting 2"
    );
    assert_eq!(heartbeat(Link::Reconnecting(None), None, 0), "reconnecting");
    assert_eq!(heartbeat(Link::Offline, None, 0), "offline");
    handoff.working();
    std::thread::sleep(LAG_AFTER + Duration::from_millis(5));
    assert!(handoff.pass_age().is_some_and(|age| age >= LAG_AFTER));
    handoff.waiting();
    assert_eq!(handoff.pass_age(), None);
}

#[test]
fn review_boundary_footer_hides_connected_and_leads_with_the_connection_notice() {
    let mut ui = crate::ui::Ui::new(false);
    ui.heartbeat = "connected".into();
    assert!(!crate::chrome::footer(&ui, 200)[0].contains("connected"));
    ui.heartbeat = "lagged".into();
    ui.gap_notice.advance(true);
    let row = &crate::chrome::footer(&ui, 200)[0];
    assert!(row.starts_with("lagged · Live view refreshing"), "{row}");
}

#[test]
#[ignore = "requires an explicit disposable protocol-profile output path"]
fn review_boundary_decodes_the_whole_stream_without_the_ui_taking_a_view() -> Result {
    let path = std::env::var_os("GRAPT_TUI_R_PROFILE").ok_or("profile output path is required")?;
    let mut capture = grapt_tui_transcript::profile::Capture::from_env()?;
    let fixture = Fixture::start()?;
    capture.record(capture.begin(), 0)?;
    let frames = fixture.turn(&(0..500).map(|i| format!("piece{i} ")).collect::<String>())?;
    let expected = u64::try_from(frames.len())?;
    for frame in frames {
        fixture.frame(frame)?;
    }
    let mut decoded = 0;
    let began = tokio::time::Instant::now();
    while decoded < expected {
        capture.record(capture.begin(), 0)?;
        let rows = std::fs::read_to_string(&path)?;
        decoded = rows
            .lines()
            .skip(1)
            .try_fold(0_u64, |total, row| -> Result<u64> {
                let row: serde_json::Value = serde_json::from_str(row)?;
                Ok(total + row["events_decoded"].as_u64().ok_or("decoded counter")?)
            })?;
        assert!(
            !fixture.backend.updates.vacant(),
            "the UI has taken no view"
        );
        if began.elapsed() > Duration::from_secs(10) {
            return Err(format!("only {decoded}/{expected} frames decoded").into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    fixture.barrier()?;
    eprintln!("decoded {decoded} frames with the first view still pending; command acknowledged");
    assert_eq!(decoded, expected);
    Ok(())
}

#[test]
fn review_boundary_starting_a_long_pass_wakes_a_ui_which_saw_the_loop_waiting() -> Result {
    let fixture = Fixture::start()?;
    fixture.barrier()?;
    until(|| fixture.backend.updates.pass_age().is_none())?;
    for _ in fixture.ui_ready.try_iter() {}
    // The UI chose its untimed receive from this same liveness observation.
    assert_eq!(fixture.backend.updates.pass_age(), None);
    let held = fixture.source.lock().map_err(|_| "source lock")?;
    crate::send(&fixture.backend, Command::SessionSearch("held read".into()))?;
    until(|| fixture.backend.updates.pass_age().is_some())?;
    let notified = fixture
        .ui_ready
        .recv_timeout(LAG_AFTER + Duration::from_millis(50));
    let label = heartbeat(Link::Connected, fixture.backend.updates.pass_age(), 0);
    drop(held);
    assert!(
        notified.is_ok(),
        "UI was not woken for the long pass; an explicit reread gives {label}"
    );
    Ok(())
}
