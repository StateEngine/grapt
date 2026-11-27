//! Disposable HTTP peers exercise the original profile, rotated auth and teardown.
use super::*;
use crate::test_support::*;
use grapt_api::ProductHealth;
use grapt_client::Operation;
use std::{fs, time::Duration};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
fn initial_subscribe_401_is_visible_without_an_installed_stream() -> Result {
    let profile = Profile::new()?;
    let peer = Peer::start("old")?;
    peer.event_status.store(401, Ordering::Release);
    profile.publish(&peer, false)?;
    let bridge = opened(&profile)?;
    assert!(!bridge.subscribed());
    assert!(matches!(
        bridge.connection().state,
        supervise::Connection::Disconnected { reason, .. } if reason.contains("401")
    ));
    assert_eq!(peer.events.load(Ordering::Acquire), 1);
    Ok(())
}

#[test]
fn replacement_subscribe_401_remains_visible_after_the_old_stream_closes() -> Result {
    let profile = Profile::new()?;
    let mut old = Peer::start("old")?;
    profile.publish(&old, false)?;
    let mut bridge = opened(&profile)?;
    old.close()?;
    closed(&mut bridge);
    let peer = Peer::start("new")?;
    peer.event_status.store(401, Ordering::Release);
    profile.publish(&peer, false)?;
    assert!(reopen(&mut bridge).is_err_and(|error| error.to_string().contains("401")));
    assert!(!bridge.subscribed());
    assert!(matches!(
        bridge.connection().state,
        supervise::Connection::Disconnected { reason, .. } if reason.contains("401")
    ));
    assert_eq!(peer.events.load(Ordering::Acquire), 1);
    Ok(())
}

#[test]
fn original_profile_reloads_rotated_endpoint_and_auth_before_allowing_rpc() -> Result {
    let profile = Profile::new()?;
    let mut old = Peer::start("old")?;
    profile.publish(&old, false)?;
    let mut config = profile.config()?;
    config.expected = Some(ProductHealth::new(12345, "r-restart", "fixture", "old")?);
    let (wake, _ready) = mpsc::sync_channel(1);
    let mut bridge = Bridge::open(config, Bounds::PRODUCTION, wake)?;
    old.close()?;
    closed(&mut bridge);
    let next = Peer::start("new")?;
    profile.publish(&next, false)?;
    let request = WireRequest::new(Operation::SessionList, None, None)?;
    assert!(matches!(
        bridge.call(&request),
        Err(TransportError::Unavailable { .. })
    ));
    assert_eq!(reopen(&mut bridge)?, Some(true));
    assert_eq!(bridge.rpc.address(), next.address);
    assert!(bridge.authority_dirty.load(Ordering::Acquire));
    assert!(bridge.sessions_dirty.load(Ordering::Acquire));
    assert_eq!(bridge.call(&request)?.status, 200);
    assert_eq!(next.calls.load(Ordering::Acquire), 1);
    assert_eq!(next.events.load(Ordering::Acquire), 1);
    Ok(())
}

#[test]
fn switching_session_during_reconnect_shows_the_new_session_and_keeps_the_old_claimed() -> Result {
    let profile = Profile::new()?;
    let peer = Peer::start("old")?;
    profile.publish(&peer, false)?;
    let mut bridge = opened(&profile)?;
    peer.window_requests.hold();
    bridge.watch_session(Some("ses_window_second".parse()?));
    closed(&mut bridge);
    assert!(bridge.begin_reconnect()?.is_none());
    // The replacement has sent its second-Session subscription, but the
    // service has not answered it. Selecting a third Session must revoke this
    // pending attachment too, not just the previous installed stream.
    until(|| peer.events.load(Ordering::Acquire) >= 2);
    bridge.watch_session(Some("ses_window_third".parse()?));
    peer.window_requests.release();
    let mut result = None;
    until(|| {
        result = Some(bridge.poll_reconnect());
        !matches!(result, Some(Ok(None)))
    });
    assert_eq!(result.ok_or("replacement result missing")??, Some(true));
    let requests = peer.window_requests.seen()?;
    // The replacement shows the third Session, and the Session it left stays
    // claimed: switching away must never hand that turn to the close grace.
    assert!(
        requests.last().is_some_and(|request| {
            request.starts_with("GET /api/event?session=ses_window_third&")
                && request.contains("retain=ses_window_second")
        }),
        "the replacement did not keep the previous Session claimed: {requests:?}"
    );
    Ok(())
}

#[test]
fn detached_stop_peer_refuses_replaced_and_dropped_attachments() -> Result {
    let profile = Profile::new()?;
    let mut old = Peer::start("old")?;
    profile.publish(&old, false)?;
    let mut bridge = opened(&profile)?;
    bridge.stop_peer()?.verify()?;
    let stale = bridge.stop_peer()?;
    old.close()?;
    closed(&mut bridge);
    let next = Peer::start("new")?;
    profile.publish(&next, false)?;
    assert_eq!(reopen(&mut bridge)?, Some(true));
    assert!(matches!(
        stale.verify(),
        Err(TransportError::Unavailable { reason }) if reason.contains("retry stop")
    ));
    let detached = bridge.stop_peer()?;
    drop(bridge);
    assert!(matches!(
        detached.verify(),
        Err(TransportError::Unavailable { reason }) if reason.contains("retry stop")
    ));
    assert_eq!(next.calls.load(Ordering::Acquire), 0);
    Ok(())
}

#[test]
fn missing_malformed_and_foreign_readiness_refuse_then_recover_without_effects() -> Result {
    let profile = Profile::new()?;
    let mut old = Peer::start("old")?;
    profile.publish(&old, false)?;
    let mut bridge = opened(&profile)?;
    old.close()?;
    closed(&mut bridge);
    let next = Peer::start("new")?;
    fs::remove_file(profile.0.join("serve-ready.json"))?;
    assert!(reopen(&mut bridge).is_err());
    fs::write(profile.0.join("serve-ready.json"), b"{broken")?;
    assert!(reopen(&mut bridge).is_err());
    profile.publish(&next, true)?;
    assert!(reopen(&mut bridge).is_err_and(|e| e.to_string().contains("different product")));
    assert!(!bridge.subscribed());
    let request = WireRequest::new(
        Operation::SessionPrompt,
        profile.config()?.session.as_ref(),
        None,
    )?;
    assert!(matches!(
        bridge.call(&request),
        Err(TransportError::Unavailable { .. })
    ));
    assert_eq!(next.calls.load(Ordering::Acquire), 0);
    assert_eq!(next.events.load(Ordering::Acquire), 0);
    profile.publish(&next, false)?;
    assert_eq!(reopen(&mut bridge)?, Some(true));
    Ok(())
}

#[test]
fn quiet_subscription_is_cancelled_when_bridge_drops() -> Result {
    let profile = Profile::new()?;
    let peer = Peer::start("old")?;
    profile.publish(&peer, false)?;
    let bridge = opened(&profile)?;
    let online = bridge.fence.clone();
    drop(bridge);
    until(|| Arc::strong_count(&online) == 1);
    assert!(!online.load(Ordering::Acquire));
    Ok(())
}

#[test]
fn slow_discovery_yields_and_exit_does_not_wait_for_health() -> Result {
    let profile = Profile::new()?;
    let mut old = Peer::start("old")?;
    profile.publish(&old, false)?;
    let mut bridge = opened(&profile)?;
    old.close()?;
    closed(&mut bridge);
    let next = Peer::start("new")?;
    next.hold.store(true, Ordering::Release);
    profile.publish(&next, false)?;
    let online = bridge.fence.clone();
    let began = tokio::time::Instant::now();
    assert_eq!(bridge.begin_reconnect()?, None);
    assert!(began.elapsed() < Duration::from_millis(100));
    until(|| next.health.load(Ordering::Acquire) == 1);
    let began = tokio::time::Instant::now();
    for _ in 0..100 {
        assert_eq!(bridge.poll_reconnect()?, None);
    }
    drop(bridge);
    assert!(began.elapsed() < Duration::from_millis(100));
    next.hold.store(false, Ordering::Release);
    until(|| Arc::strong_count(&online) == 1);
    Ok(())
}

#[test]
fn credential_change_during_discovery_cannot_install_a_mixed_profile() -> Result {
    let profile = Profile::new()?;
    let mut old = Peer::start("old")?;
    profile.publish(&old, false)?;
    let mut bridge = opened(&profile)?;
    old.close()?;
    closed(&mut bridge);
    let next = Peer::start("new")?;
    next.hold.store(true, Ordering::Release);
    profile.publish(&next, false)?;
    assert_eq!(bridge.begin_reconnect()?, None);
    until(|| next.health.load(Ordering::Acquire) == 1);
    fs::write(profile.0.join("basic.credential"), "fixture:changed")?;
    next.hold.store(false, Ordering::Release);
    assert!(reopen(&mut bridge).is_err_and(|e| e.to_string().contains("changed during attachment")));
    assert!(!bridge.subscribed());
    assert_eq!(next.events.load(Ordering::Acquire), 0);
    assert_eq!(next.calls.load(Ordering::Acquire), 0);
    profile.publish(&next, false)?;
    assert_eq!(reopen(&mut bridge)?, Some(true));
    Ok(())
}

#[test]
fn launcher_health_pin_refuses_another_initial_product() -> Result {
    let profile = Profile::new()?;
    let peer = Peer::start("old")?;
    profile.publish(&peer, false)?;
    let mut config = profile.config()?;
    config.expected = Some(ProductHealth::new(12345, "r-restart", "fixture", "other")?);
    assert!(config.connect(Bounds::PRODUCTION).is_err());
    assert_eq!(peer.calls.load(Ordering::Acquire), 0);
    assert_eq!(peer.health.load(Ordering::Acquire), 0);
    Ok(())
}
#[test]
fn a_burst_the_view_has_not_drained_waits_instead_of_dropping() -> Result {
    let profile = Profile::new()?;
    let server = Flood::burst()?;
    profile.publish_flood(&server, false)?;
    let mut bridge = opened(&profile)?;
    // Nothing drains while the reader receives far more than the handoff
    // holds; the reader waits, so the stream stays whole and live.
    std::thread::sleep(Duration::from_millis(200));
    let mut ticks = Vec::new();
    until(|| {
        while let Ok(FramePoll::Frame(text)) = bridge.poll_frame() {
            ticks.push(text);
        }
        ticks.len() == 2000
    });
    let expected: Vec<String> = (0..2000).map(|tick| format!("tick-{tick}")).collect();
    assert_eq!(ticks, expected);
    assert!(bridge.subscribed());
    assert!(bridge.connection().state.streaming());
    Ok(())
}

#[test]
fn a_transport_gap_keeps_commands_and_stop_while_the_view_resyncs() -> Result {
    let profile = Profile::new()?;
    let server = Flood::start()?;
    profile.publish_flood(&server, false)?;
    let mut bridge = opened(&profile)?;
    gapped(&mut bridge);
    assert!(!bridge.subscribed());
    assert!(bridge.fence.load(Ordering::Acquire));
    let request = WireRequest::new(Operation::SessionList, None, None)?;
    assert_eq!(bridge.call(&request)?.status, 200);
    assert_eq!(server.calls.load(Ordering::Acquire), 1);
    bridge.stop_peer()?.verify()?;
    Ok(())
}

#[test]
fn gap_then_failed_replacement_refuses_commands_before_they_reach_the_peer() -> Result {
    let profile = Profile::new()?;
    let server = Flood::start()?;
    profile.publish_flood(&server, false)?;
    let mut bridge = opened(&profile)?;
    gapped(&mut bridge);
    profile.publish_flood(&server, true)?;
    assert!(reopen(&mut bridge).is_err_and(|e| e.to_string().contains("different product")));
    let prompt = WireRequest::new(
        Operation::SessionPrompt,
        profile.config()?.session.as_ref(),
        None,
    )?;
    assert!(matches!(
        bridge.call(&prompt),
        Err(TransportError::Unavailable { .. })
    ));
    assert!(bridge.stop_peer().is_err());
    assert_eq!(server.calls.load(Ordering::Acquire), 0);
    profile.publish_flood(&server, false)?;
    assert_eq!(reopen(&mut bridge)?, Some(true));
    let request = WireRequest::new(Operation::SessionList, None, None)?;
    assert_eq!(bridge.call(&request)?.status, 200);
    assert_eq!(server.calls.load(Ordering::Acquire), 1);
    Ok(())
}

#[test]
fn installation_fences_a_stop_peer_taken_before_a_gap_driven_replacement() -> Result {
    let profile = Profile::new()?;
    let server = Flood::start()?;
    profile.publish_flood(&server, false)?;
    let mut bridge = opened(&profile)?;
    let usable = bridge.stop_peer()?;
    let stale = bridge.stop_peer()?;
    gapped(&mut bridge);
    // Before installation the same attachment still serves stop.
    usable.verify()?;
    assert_eq!(reopen(&mut bridge)?, Some(true));
    assert!(matches!(
        stale.verify(),
        Err(TransportError::Unavailable { reason }) if reason.contains("retry stop")
    ));
    bridge.stop_peer()?.verify()?;
    Ok(())
}
