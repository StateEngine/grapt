//! The reader's ordered replacement barrier separates old and new live facts.
use super::*;
use grapt_client::supervise::Connection as State;
use grapt_client::BasicCredential;
use std::time::Duration;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn rpc() -> Result<HttpConnection> {
    Ok(HttpConnection::attach(
        "127.0.0.1:1".parse()?,
        BasicCredential::new("fixture", "local-only")?,
        Bounds::PRODUCTION,
    ))
}
/// A live, verified attachment fed by the returned sender.
fn live() -> Result<(SyncSender<Delivery>, Receiver<()>, Bridge)> {
    let (events, frames) = mpsc::sync_channel(32);
    let (reconnect, retry) = mpsc::sync_channel(1);
    let mut bridge = fixture(frames, reconnect)?;
    bridge.state = State::Live;
    bridge.fence.store(true, Ordering::Release);
    Ok((events, retry, bridge))
}
fn fixture(frames: Receiver<Delivery>, reconnect: SyncSender<()>) -> Result<Bridge> {
    // Before any verified attachment.
    let mut bridge = Bridge::assembled(rpc()?, frames, reconnect);
    bridge.state = State::Replacing { attempt: 1 };
    bridge.fence = Arc::new(AtomicBool::new(false));
    Ok(bridge)
}
fn unavailable(reason: &str) -> TransportError {
    TransportError::Unavailable {
        reason: reason.to_owned(),
    }
}
fn replacement() -> Result<Delivery> {
    replacement_at(0)
}
fn replacement_at(generation: u64) -> Result<Delivery> {
    Ok(Delivery::Replacement(
        Ok(Box::new(Attachment {
            rpc: rpc()?,
            cancel: None,
            valid: Arc::new(AtomicBool::new(true)),
        })),
        generation,
    ))
}

#[test]
fn loss_bypasses_a_catalog_gate_before_reconnect_can_discard_it() -> Result {
    let (_events, frames) = mpsc::sync_channel(32);
    let (reconnect, _retry) = mpsc::sync_channel(1);
    let mut bridge = fixture(frames, reconnect)?;
    let (gap, gaps) = mpsc::sync_channel(1);
    bridge.gaps = gaps;
    bridge.state = State::Live;
    // A gap loses live events, not the attachment.
    bridge.fence.store(true, Ordering::Release);
    bridge.sessions_dirty.store(true, Ordering::Release);
    bridge
        .gate
        .hold(FramePoll::Frame("catalog-gated event".into()));
    gap.send(())?;
    assert!(bridge.subscribed());
    assert_eq!(bridge.poll_frame()?, FramePoll::Gap);
    assert!(!bridge.subscribed());
    assert_eq!(bridge.begin_reconnect()?, None);
    Ok(())
}

#[test]
fn queued_stop_reason_survives_closure_before_the_first_client_pump() -> Result {
    let (events, frames) = mpsc::sync_channel(32);
    let (reconnect, _retry) = mpsc::sync_channel(1);
    let mut bridge = fixture(frames, reconnect)?;
    bridge.state = State::Live;
    events.send(Delivery::Frame(FramePoll::Positioned {
        id: "stop-1".into(),
        data: serde_json::json!({"id":"evt_connected","type":"server.connected","data":{}})
            .to_string(),
    }))?;
    events.send(Delivery::Frame(FramePoll::Positioned {
        id: "stop-2".into(),
        data: serde_json::json!({
            "id":"evt_service_stop", "type":"server.stopping", "created":1,
            "data":{"reason":"service-stopped"}
        })
        .to_string(),
    }))?;
    events.send(Delivery::Frame(FramePoll::Closed))?;
    let mut client = grapt_client::Client::new(bridge, Bounds::PRODUCTION);
    client.pump_at(Duration::ZERO)?;
    assert!(client
        .service_stop_notice()
        .is_some_and(|notice| notice.contains("service-stopped")));
    Ok(())
}

#[test]
fn replacement_fences_pending_and_queued_old_frames_without_dropping_the_new_prefix() -> Result {
    let (events, frames) = mpsc::sync_channel(32);
    let (reconnect, _retry) = mpsc::sync_channel(1);
    let mut bridge = fixture(frames, reconnect)?;
    bridge
        .gate
        .hold(FramePoll::Frame("old catalog-gated event".into()));
    events.send(Delivery::Frame(FramePoll::Frame(
        "old connection event".into(),
    )))?;
    events.send(Delivery::Frame(FramePoll::Closed))?;
    events.send(replacement()?)?;
    events.send(Delivery::Frame(FramePoll::Frame(
        "new connection event".into(),
    )))?;
    events.send(Delivery::Frame(FramePoll::Closed))?;
    assert_eq!(bridge.begin_reconnect()?, Some(true));
    bridge.sessions_dirty.store(false, Ordering::Release);
    assert_eq!(
        bridge.poll_frame()?,
        FramePoll::Frame("new connection event".into())
    );
    assert_eq!(bridge.poll_frame()?, FramePoll::Closed);
    assert_eq!(bridge.poll_frame()?, FramePoll::Idle);
    Ok(())
}

#[test]
fn replacement_drains_a_full_old_handoff_to_release_its_blocked_reader() -> Result {
    let (events, frames) = mpsc::sync_channel(32);
    let (reconnect, retry) = mpsc::sync_channel(1);
    for n in 0..32 {
        events.send(Delivery::Frame(FramePoll::Frame(format!("old event {n}"))))?;
    }
    let mut bridge = fixture(frames, reconnect)?;
    let barrier = replacement()?;
    let (began, started) = mpsc::sync_channel(1);
    let (closed, closed_sent) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || -> std::result::Result<(), String> {
        retry.recv().map_err(|e| e.to_string())?;
        began.send(()).map_err(|e| e.to_string())?;
        // This cannot finish until recovery consumes an old frame: all slots
        // are occupied and no foreground consumer has drained one yet.
        events
            .send(Delivery::Frame(FramePoll::Closed))
            .map_err(|e| e.to_string())?;
        closed.send(()).map_err(|e| e.to_string())?;
        events.send(barrier).map_err(|e| e.to_string())?;
        events
            .send(Delivery::Frame(FramePoll::Frame("new prefix".into())))
            .map_err(|e| e.to_string())?;
        Ok(())
    });
    bridge.reconnect.send(())?;
    bridge.replacement_requested = true;
    started.recv_timeout(Duration::from_secs(2))?;
    assert_eq!(closed_sent.try_recv(), Err(mpsc::TryRecvError::Empty));
    // Even foreground polls cannot leak an old frame while recovery is pending.
    assert_eq!(bridge.poll_frame()?, FramePoll::Idle);
    let mut connected = false;
    for _ in 0..2000 {
        if bridge.poll_reconnect()? == Some(true) {
            connected = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        connected,
        "full handoff stranded replacement behind old Closed"
    );
    worker.join().map_err(|_| "reader fixture panicked")??;
    bridge.sessions_dirty.store(false, Ordering::Release);
    assert_eq!(bridge.poll_frame()?, FramePoll::Frame("new prefix".into()));
    assert_eq!(bridge.poll_frame()?, FramePoll::Idle);
    Ok(())
}

fn listed() -> Result<WireRequest> {
    Ok(WireRequest::new(
        grapt_client::Operation::SessionList,
        None,
        None,
    )?)
}
fn reconnecting(error: &TransportError) -> bool {
    matches!(error, TransportError::Unavailable { reason } if reason.contains("reconnecting"))
}

#[test]
fn recovery_installs_a_queued_replacement_before_reading_as_subscribed() -> Result {
    let (events, frames) = mpsc::sync_channel(32);
    let (reconnect, _retry) = mpsc::sync_channel(1);
    let mut bridge = fixture(frames, reconnect)?;
    // The reader has connected and queued its replacement; the bridge has
    // not drained it yet.
    bridge.fence.store(true, Ordering::Release);
    bridge.replacement_requested = true;
    events.send(replacement()?)?;
    assert!(
        !bridge.subscribed(),
        "a pending replacement must not read as subscribed"
    );
    let mut client = grapt_client::Client::new(bridge, Bounds::PRODUCTION);
    client.pump_at(Duration::ZERO)?;
    assert!(
        !client.peer_mut().replacement_requested,
        "recovery stopped polling and stranded the replacement"
    );
    assert!(client.peer_mut().subscribed());
    assert!(client.peer_mut().fence.load(Ordering::Acquire));
    assert_eq!(client.peer_mut().connection().state, State::Live);
    // Reads are no longer refused as "service reconnecting".
    let request = listed()?;
    let read = client.peer_mut().begin_read(&request);
    assert!(!matches!(&read, Err(error) if reconnecting(error)));
    Ok(())
}

#[test]
fn a_failed_replacement_fences_the_old_attachment() -> Result {
    let (events, _retry, mut bridge) = live()?;
    events.send(Delivery::Replacement(
        Err(unavailable("different product")),
        0,
    ))?;
    assert!(bridge.begin_reconnect().is_err());
    assert!(!bridge.fence.load(Ordering::Acquire));
    assert!(matches!(
        bridge.connection().state,
        State::Disconnected { .. }
    ));
    assert!(bridge.call(&listed()?).is_err_and(|e| reconnecting(&e)));
    Ok(())
}

#[test]
fn a_pending_reconnect_holds_multi_step_work_but_not_one_shot_calls() -> Result {
    let (_events, _retry, mut bridge) = live()?;
    bridge.replacement_requested = true;
    let request = listed()?;
    assert!(bridge.begin_read(&request).is_err_and(|e| reconnecting(&e)));
    assert!(bridge
        .begin_catalog(&request)
        .is_err_and(|e| reconnecting(&e)));
    assert!(bridge
        .begin_tool_read(&request)
        .is_err_and(|e| reconnecting(&e)));
    assert!(bridge
        .begin_operator(&request)
        .is_err_and(|e| reconnecting(&e)));
    // The unreachable fixture peer fails with I/O, not with a reconnect refusal.
    assert!(bridge.call(&request).is_err_and(|e| !reconnecting(&e)));
    assert!(bridge.stop_peer().is_ok());
    Ok(())
}

#[test]
fn a_service_too_old_keeps_its_typed_refusal_and_is_not_retried_as_an_outage() -> Result {
    let (events, retry, mut bridge) = live()?;
    events.send(Delivery::Replacement(
        Err(TransportError::ServerTooOld {
            missing: "stream positions",
        }),
        0,
    ))?;
    assert!(matches!(
        bridge.begin_reconnect(),
        Err(TransportError::ServerTooOld { .. })
    ));
    assert!(matches!(bridge.connection().state, State::Refused { .. }));
    // The one replacement that found the service too old was requested;
    // every later pump keeps the refusal and asks for no other.
    retry.try_recv()?;
    for _ in 0..3 {
        assert!(matches!(
            bridge.begin_reconnect(),
            Err(TransportError::ServerTooOld { .. })
        ));
        assert!(matches!(
            bridge.poll_reconnect(),
            Err(TransportError::ServerTooOld { .. })
        ));
        bridge.resubscribe();
    }
    assert!(
        retry.try_recv().is_err(),
        "a too-old service was asked again"
    );
    Ok(())
}

#[test]
fn an_unpositioned_first_frame_refuses_the_service_without_a_replacement() -> Result {
    let (events, retry, mut bridge) = live()?;
    events.send(Delivery::Frame(FramePoll::Frame("unpositioned".into())))?;
    bridge.refuse_stream("stream positions");
    assert!(!bridge.subscribed());
    for _ in 0..3 {
        assert!(matches!(
            bridge.begin_reconnect(),
            Err(TransportError::ServerTooOld { .. })
        ));
        bridge.resubscribe();
    }
    assert!(
        retry.try_recv().is_err(),
        "refusing a too-old stream started a replacement"
    );
    Ok(())
}

#[test]
fn a_replacement_from_another_product_is_refused_not_retried_as_an_outage() -> Result {
    let (events, _retry, mut bridge) = live()?;
    events.send(Delivery::Replacement(
        Err(unavailable(
            "ready service differs from the launcher-selected product",
        )),
        0,
    ))?;
    assert!(bridge.begin_reconnect().is_err());
    assert!(matches!(bridge.connection().state, State::Refused { .. }));
    assert!(bridge.call(&listed()?).is_err_and(|e| reconnecting(&e)));
    assert!(bridge.stop_peer().is_err());
    Ok(())
}

#[test]
fn a_close_held_behind_a_catalog_refresh_still_starts_recovery() -> Result {
    let (events, frames) = mpsc::sync_channel(32);
    let (reconnect, _retry) = mpsc::sync_channel(1);
    let mut bridge = fixture(frames, reconnect)?;
    bridge.state = State::Live;
    // The reader saw its stream close and revoked the fence; its Closed frame
    // now waits behind a catalog refresh that the revoked fence refuses.
    bridge.fence.store(false, Ordering::Release);
    bridge.sessions_dirty.store(true, Ordering::Release);
    events.send(Delivery::Frame(FramePoll::Closed))?;
    assert!(!bridge.subscribed());
    assert_eq!(bridge.poll_frame()?, FramePoll::Idle);
    assert_eq!(bridge.connection().state, State::Replacing { attempt: 1 });
    assert_eq!(bridge.begin_reconnect()?, None);
    Ok(())
}

#[test]
fn a_refused_service_keeps_its_refusal_through_retries_until_one_verifies() -> Result {
    let (events, retry, mut bridge) = live()?;
    let refused = |bridge: &Bridge| {
        matches!(&bridge.connection().state,
            State::Refused { reason } if reason.contains("launcher-selected"))
    };
    events.send(Delivery::Replacement(
        Err(unavailable(
            "ready service differs from the launcher-selected product",
        )),
        0,
    ))?;
    assert!(bridge.begin_reconnect().is_err());
    assert!(refused(&bridge));
    // Recovery probes again; an outage on the way keeps the refusal.
    retry.try_recv()?;
    events.send(Delivery::Replacement(
        Err(unavailable("connection refused")),
        0,
    ))?;
    assert!(bridge.begin_reconnect().is_err());
    assert!(refused(&bridge));
    assert!(bridge.call(&listed()?).is_err_and(|e| reconnecting(&e)));
    retry.try_recv()?;
    events.send(replacement()?)?;
    assert_eq!(bridge.begin_reconnect()?, Some(true));
    assert_eq!(bridge.connection().state, State::Live);
    Ok(())
}

#[test]
fn a_close_from_the_old_stream_while_replacing_cannot_undo_the_replacement() -> Result {
    let (events, _retry, mut bridge) = live()?;
    events.send(Delivery::Frame(FramePoll::Closed))?;
    assert_eq!(bridge.poll_frame()?, FramePoll::Closed);
    assert_eq!(bridge.connection().state, State::Replacing { attempt: 1 });
    assert_eq!(bridge.begin_reconnect()?, None);
    // The old generation closes again before its replacement arrives.
    events.send(Delivery::Frame(FramePoll::Closed))?;
    assert_eq!(bridge.poll_frame()?, FramePoll::Idle);
    events.send(replacement()?)?;
    assert_eq!(bridge.poll_reconnect()?, Some(true));
    assert_eq!(bridge.poll_frame()?, FramePoll::Idle);
    assert_eq!(bridge.connection().state, State::Live);
    Ok(())
}

#[test]
fn a_stuck_catalog_behind_a_full_handoff_still_reaches_recovery() -> Result {
    let (events, _retry, mut bridge) = live()?;
    // A catalog refresh that keeps failing holds every frame.
    bridge.sessions_dirty.store(true, Ordering::Release);
    // A reader whose stream has settled does not exit: production's reader
    // waits for the backend to ask again (`bridge.rs`, `retry.recv()`), so its
    // handoff stays open. This one parks the same way and reports where it
    // parked, which is the fact this test waits for; it never races a
    // descheduled thread against a wall clock.
    let (parked, parks) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        // More than the handoff holds, then the service goes away: this
        // reader parks on a full queue and its Closed cannot be seen.
        let frame = |tick| Delivery::Frame(FramePoll::Frame(format!("tick-{tick}")));
        if (0..64).any(|tick| events.send(frame(tick)).is_err()) {
            return;
        }
        if events.send(Delivery::Frame(FramePoll::Closed)).is_err() {
            return;
        }
        // Every frame and then Closed are through, so the reader has nothing to
        // read until a replacement is asked for: signal the park and hold.
        let _ = parked.send(());
        let _ = released.recv();
    });
    let gapped = (0..2000).any(|_| {
        std::thread::sleep(Duration::from_millis(1));
        bridge.catalog_refreshed(false);
        matches!(bridge.poll_frame(), Ok(FramePoll::Gap))
    });
    assert!(
        gapped,
        "a frame held behind a failing catalog hid the stream"
    );
    assert!(matches!(bridge.connection().state, State::Resyncing { .. }));
    assert!(!bridge.subscribed());
    // Recovery drains the old generation, which releases the parked reader.
    assert_eq!(bridge.begin_reconnect()?, None);
    // The park is the recovered event: it proves the reader delivered its whole
    // prefix and its Closed, so a handoff that keeps its reader parked has been
    // drained. The guard bounds a stall, not the reader's scheduling: a healthy
    // pass parks within a poll or two.
    let guard = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let _ = bridge.poll_reconnect();
        if parks.recv_timeout(Duration::from_millis(1)).is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < guard,
            "the parked reader was never released: recovery stalled"
        );
    }
    release.send(())?;
    reader.join().map_err(|_| "reader fixture panicked")?;
    Ok(())
}

#[test]
fn a_new_generation_starts_its_own_catalog_hold() -> Result {
    let (events, _retry, mut bridge) = live()?;
    let stale = |bridge: &mut Bridge| {
        bridge.gate = CatalogGate::with_backstop(Duration::from_millis(10));
        let _ = bridge
            .gate
            .admit(Some(FramePoll::Frame("old".into())), true);
        std::thread::sleep(Duration::from_millis(20));
    };
    let (gap, gaps) = mpsc::sync_channel(1);
    bridge.gaps = gaps;
    stale(&mut bridge);
    gap.send(())?;
    assert_eq!(bridge.poll_frame()?, FramePoll::Gap);
    assert_eq!(bridge.gate.holding_since(), None);
    stale(&mut bridge);
    assert_eq!(bridge.begin_reconnect()?, None);
    assert_eq!(bridge.gate.holding_since(), None);
    stale(&mut bridge);
    events.send(replacement()?)?;
    assert_eq!(bridge.poll_reconnect()?, Some(true));
    assert_eq!(bridge.gate.holding_since(), None);
    // The install marks the catalog dirty; the first new frame waits for it
    // on a fresh clock instead of resyncing against the old one.
    events.send(Delivery::Frame(FramePoll::Frame("new".into())))?;
    assert_eq!(bridge.poll_frame()?, FramePoll::Idle);
    Ok(())
}

#[test]
fn retired_contract_broken_stream_is_not_subscribed_before_reader_acknowledges() -> Result {
    let (events, retry, mut bridge) = live()?;
    events.send(Delivery::Frame(FramePoll::Frame(
        "retired stream suffix".into(),
    )))?;
    assert!(
        bridge.subscribed(),
        "control must begin with a live attachment"
    );
    bridge.retire_stream();
    assert!(
        !bridge.subscribed(),
        "retirement waited for the reader and left the stream live"
    );
    assert_eq!(bridge.begin_reconnect()?, None);
    retry.try_recv()?;
    assert_eq!(
        bridge.poll_frame()?,
        FramePoll::Idle,
        "retired suffix escaped replacement draining"
    );
    Ok(())
}

#[test]
fn retired_stream_cannot_deliver_queued_frames_when_replacement_request_fails() -> Result {
    let (events, retry, mut bridge) = live()?;
    drop(retry);
    events.send(Delivery::Frame(FramePoll::Frame("retired suffix".into())))?;
    drop(events);
    bridge.retire_stream();
    assert!(!bridge.subscribed());
    assert!(
        !matches!(
            bridge.poll_frame(),
            Ok(FramePoll::Frame(_) | FramePoll::Positioned { .. })
        ),
        "failed replacement request exposed the retired suffix"
    );
    Ok(())
}

#[test]
fn retirement_joins_a_full_request_and_releases_a_full_reader_handoff() -> Result {
    let (events, retry, mut bridge) = live()?;
    for n in 0..32 {
        events.send(Delivery::Frame(FramePoll::Frame(format!("retired {n}"))))?;
    }
    let barrier = replacement_at(1)?;
    let (began, started) = mpsc::sync_channel(1);
    let (closed, closed_sent) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || -> std::result::Result<(), String> {
        began.send(()).map_err(|e| e.to_string())?;
        events
            .send(Delivery::Frame(FramePoll::Closed))
            .map_err(|e| e.to_string())?;
        closed.send(()).map_err(|e| e.to_string())?;
        retry.recv().map_err(|e| e.to_string())?;
        events.send(barrier).map_err(|e| e.to_string())?;
        events
            .send(Delivery::Frame(FramePoll::Frame("fresh prefix".into())))
            .map_err(|e| e.to_string())?;
        Ok(())
    });
    started.recv_timeout(Duration::from_secs(2))?;
    assert_eq!(closed_sent.try_recv(), Err(mpsc::TryRecvError::Empty));
    bridge.reconnect.send(())?;
    bridge.retire_stream();
    if !bridge.subscribed() {
        assert_eq!(bridge.poll_frame()?, FramePoll::Idle);
    }
    let connected = bridge.subscribed()
        || (0..2000).any(|_| {
            if matches!(bridge.poll_reconnect(), Ok(Some(true))) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
            false
        });
    assert!(
        connected,
        "full request did not release the retired reader handoff"
    );
    closed_sent.recv_timeout(Duration::from_secs(2))?;
    worker.join().map_err(|_| "reader fixture panicked")??;
    bridge.sessions_dirty.store(false, Ordering::Release);
    assert_eq!(
        bridge.poll_frame()?,
        FramePoll::Frame("fresh prefix".into())
    );
    Ok(())
}

#[test]
fn retired_catalog_text_waits_through_stale_and_failed_barriers() -> Result {
    for failed in [false, true] {
        let (events, _retry, mut bridge) = live()?;
        bridge
            .gate
            .hold(FramePoll::Frame("retired catalog text".into()));
        events.send(Delivery::Frame(FramePoll::Frame(
            "retired queue text".into(),
        )))?;
        events.send(Delivery::Stale)?;
        events.send(replacement_at(0)?)?;
        bridge.retire_stream();
        assert!(!bridge.subscribed());
        assert_eq!(bridge.poll_frame()?, FramePoll::Idle);
        assert_eq!(bridge.poll_reconnect()?, None, "stale barrier installed");
        assert_eq!(bridge.poll_frame()?, FramePoll::Idle);
        events.send(Delivery::Frame(FramePoll::Frame(
            "stale build suffix".into(),
        )))?;
        if failed {
            events.send(Delivery::Replacement(
                Err(unavailable("replacement refused")),
                1,
            ))?;
            assert!(bridge.poll_reconnect().is_err());
            events.send(Delivery::Frame(FramePoll::Frame(
                "failed build suffix".into(),
            )))?;
            assert!(!bridge.subscribed());
            assert_eq!(
                bridge.poll_frame()?,
                FramePoll::Idle,
                "failed barrier ended retirement"
            );
        } else {
            events.send(replacement_at(1)?)?;
            events.send(Delivery::Frame(FramePoll::Frame("verified prefix".into())))?;
            assert_eq!(bridge.poll_reconnect()?, Some(true));
            bridge.sessions_dirty.store(false, Ordering::Release);
            assert_eq!(
                bridge.poll_frame()?,
                FramePoll::Frame("verified prefix".into())
            );
        }
    }
    Ok(())
}
