//! The lane's bound and its attachment fence.
use super::*;
use std::sync::mpsc;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn saturated_passes_never_grow_the_held_commands_past_their_bound() -> Result {
    let updates = Handoff::default();
    let (sender, commands) = mpsc::channel();
    for _ in 0..200 {
        updates.handed();
        sender.send(updates.envelope(Command::Commands))?;
    }
    let mut held = Held::default();
    for _ in 0..8 {
        held.fill(&commands, &updates);
        assert!(held.next(true, &updates).is_empty());
        assert_eq!(held.0.len(), HELD);
    }
    // Nothing held has run, so the intake depth still names every command.
    assert_eq!(updates.intake(), 200);
    Ok(())
}

#[test]
fn one_pass_receives_at_most_its_bound_even_when_selects_coalesce() -> Result {
    let updates = Handoff::default();
    let (sender, commands) = mpsc::channel();
    let session = grapt_client::wire::session_id("ses_000000000009")?;
    for _ in 0..200 {
        updates.handed();
        let select = Command::Select(super::super::Selection {
            id: session.clone(),
        });
        sender.send(updates.envelope(select))?;
    }
    let mut held = Held::default();
    assert!(held.fill(&commands, &updates));
    assert_eq!(held.0.len(), 1);
    assert_eq!(commands.try_iter().count(), 200 - HELD);
    Ok(())
}

#[test]
fn a_not_found_open_from_a_revoked_attachment_forgets_nothing() -> Result {
    let rpc = grapt_client::HttpConnection::attach(
        std::net::SocketAddr::from(([127, 0, 0, 1], 9)),
        grapt_client::BasicCredential::new("fixture", "local-only")?,
        grapt_client::Bounds::PRODUCTION,
    );
    let mut client = Client::new(Bridge::test_rpc(rpc), grapt_client::Bounds::PRODUCTION);
    let (wake, _ready) = mpsc::sync_channel(1);
    let mut reads = Reads::start(wake)?;
    let session = grapt_client::wire::session_id("ses_000000000009")?;
    let id = Handoff::default().envelope(Command::Commands).0;
    let mut lane = Lane {
        in_flight: Some((
            InFlight::Open(id, Box::new(client.prepare_open(&session)?)),
            Arc::new(AtomicBool::new(false)),
        )),
        ..Lane::default()
    };
    let not_found = WireResponse {
        status: 404,
        body: Vec::new(),
    };
    let answered = lane.answer(&mut client, &mut reads, Ok(not_found));
    assert!(answered.unavailable.is_none());
    assert_eq!(answered.selection, Some(id));
    assert!(!lane.busy());
    Ok(())
}
