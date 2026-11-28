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
    let mut client = quiet_client()?;
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

fn quiet_client() -> std::result::Result<Client<Bridge>, Box<dyn std::error::Error>> {
    let rpc = grapt_client::HttpConnection::attach(
        std::net::SocketAddr::from(([127, 0, 0, 1], 9)),
        grapt_client::BasicCredential::new("fixture", "local-only")?,
        grapt_client::Bounds::PRODUCTION,
    );
    Ok(Client::new(
        Bridge::test_rpc(rpc),
        grapt_client::Bounds::PRODUCTION,
    ))
}

#[test]
fn an_older_open_settling_after_a_quit_never_replaces_the_newer_selection() -> Result {
    let client = quiet_client()?;
    let updates = Handoff::default();
    let older = updates.envelope(Command::Commands).0;
    let session = grapt_client::wire::session_id("ses_000000000009")?;
    let newer = updates.envelope(Command::Select(super::super::Selection { id: session }));
    let newer_id = newer.0;
    let (mut ledger, mut selection) = (Default::default(), None);
    let (mut dropped, mut roster) = (Vec::new(), None);
    let holders = super::super::closing::Holders {
        ledger: &mut ledger,
        selection: &mut selection,
        navigation_dropped: &mut dropped,
        roster: &mut roster,
    };
    super::super::closing::refuse(vec![newer], &client, holders);
    // The open that was in flight settles before the UI takes the view.
    let answered = Answered {
        selection: Some(older),
        ..Answered::default()
    };
    answered.apply(&mut Vec::new(), &mut selection, &mut String::new());
    assert_eq!(selection, Some(newer_id));
    Ok(())
}

#[test]
fn a_discarded_history_move_and_agent_choice_are_settled() -> Result {
    let client = quiet_client()?;
    let updates = Handoff::default();
    let movement = grapt_client::client::HistoryMove::Older;
    let browse = updates.envelope(Command::Browse(movement));
    let browse_id = browse.0;
    let agent = grapt_client::wire::agent_id("agt_fixture")?;
    let choose = updates.envelope(Command::ChooseAgent(agent));
    let (mut ledger, mut selection) = (Default::default(), None);
    let (mut dropped, mut roster) = (Vec::new(), None);
    let holders = super::super::closing::Holders {
        ledger: &mut ledger,
        selection: &mut selection,
        navigation_dropped: &mut dropped,
        roster: &mut roster,
    };
    super::super::closing::refuse(vec![browse, choose], &client, holders);
    assert_eq!(
        dropped,
        vec![browse_id],
        "the history move would stay pending"
    );
    assert!(roster.is_some(), "the agent picker would stay pending");
    Ok(())
}
