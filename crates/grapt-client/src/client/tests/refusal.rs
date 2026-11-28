//! A service refused as too old is asked nothing more, and an answered ask
//! leaves the open list.
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn too_old(result: Result<impl std::fmt::Debug, ClientError>) -> bool {
    matches!(
        result,
        Err(ClientError::Transport(TransportError::ServerTooOld { .. }))
    )
}

#[test]
fn a_refused_service_is_asked_nothing_more() -> TestResult {
    let mut client = client();
    let session = client.create_session()?;
    let sessions = client.connection.session_count();
    client.settle_refused("session snapshots");
    assert!(too_old(client.create_session()), "a create was sent");
    assert_eq!(client.connection.session_count(), sessions);
    assert!(too_old(client.pump()), "the pump advanced an owner");
    assert!(too_old(client.reconcile(&session)));
    assert_eq!(client.recovery_wait(), None, "nothing left to wake for");
    Ok(())
}

#[test]
fn an_answered_send_only_ask_leaves_the_open_list() -> TestResult {
    let mut client = client();
    let session = client.create_session()?;
    let ask = seed_ask(&mut client.connection, &session, "write a file");
    let other = seed_ask(&mut client.connection, &session, "run a command");
    client.refresh_permissions()?;
    client.send_permission_reply(&session, &ask, crate::wire::PermissionReplyChoice::Once)?;
    let open: Vec<_> = client
        .permissions()
        .iter()
        .map(|open| open.request_id.clone())
        .collect();
    assert_eq!(open, [other], "only the answered ask left");
    Ok(())
}

#[test]
fn a_refusal_settles_the_catalog_lane_and_fences_periodic_reads() -> TestResult {
    let mut client = client();
    client.create_session()?;
    client.open_commands();
    client.settle_refused("session snapshots");
    assert!(client
        .commands_status()
        .is_some_and(|status| status.contains("Upgrade")));
    assert!(too_old(
        client.prepare_read(super::offloop::PeriodicRead::Permissions)
    ));
    assert_eq!(client.recovery_wait(), None);
    Ok(())
}

#[test]
fn a_list_read_before_a_reply_is_acknowledged_never_brings_the_ask_back() -> TestResult {
    let mut client = client();
    let session = client.create_session()?;
    let ask = seed_ask(&mut client.connection, &session, "write a file");
    let prepared = client
        .prepare_read(super::offloop::PeriodicRead::Permissions)?
        .ok_or("no list read")?;
    // The host holds the answer while the reply is acknowledged.
    let held = client.connection.call(&prepared.request);
    client.send_permission_reply(&session, &ask, crate::wire::PermissionReplyChoice::Once)?;
    assert!(
        !client.accept_read(prepared, held)?,
        "the held list was applied"
    );
    assert!(client
        .permissions()
        .iter()
        .all(|open| open.request_id != ask));
    Ok(())
}

#[test]
fn refresh_actions_after_a_refusal_keep_it() -> TestResult {
    let mut client = client();
    client.create_session()?;
    client.settle_refused("session snapshots");
    client.open_commands();
    client.refresh_commands();
    assert!(client
        .commands_status()
        .is_some_and(|status| status.contains("Upgrade")));
    assert!(too_old(client.refresh_connection_catalog()));
    assert!(!client.catalog_pending());
    assert_eq!(client.recovery_wait(), None);
    Ok(())
}
