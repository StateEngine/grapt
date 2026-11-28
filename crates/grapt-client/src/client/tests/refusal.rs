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
