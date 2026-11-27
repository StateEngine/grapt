//! Handshake, negotiation and bounded startup.

use std::time::Duration;

use grapt_mcp::session::Unusable;
use grapt_mcp::PROTOCOL_VERSION;
use serde_json::Value;

use super::super::*;
use super::fixtures::*;

#[tokio::test]
async fn a_bounded_startup_handshakes_discovers_and_becomes_ready() {
    let (stream, server) = scripted(vec![handshake(), listing()]);
    let mut channel = channel(stream);
    let mut session = session();
    let started = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(5),
        Duration::from_secs(5),
        "0.1.0",
    )
    .await;
    let identity = match started {
        Ok(started) => started.identity,
        Err(reason) => panic!("a well-behaved server did not start: {reason:?}"),
    };
    assert_eq!(identity.name(), "fixture-server");
    assert_eq!(identity.version(), Some("1.2"));
    assert!(session.state().is_ready());
    assert_eq!(session.catalog().len(), 1);
    server.abort();
}

/// The handshake is two steps, and the second one is ours.
///
/// A server is entitled to refuse everything until the client confirms it
/// is initialized. This fixture takes that entitlement, so a client that
/// answers the `initialize` result and goes straight to discovery does not
/// get a lenient pass — it gets the refusal a user would get.
#[tokio::test]
async fn discovery_waits_for_the_initialized_notification_that_completes_the_handshake() {
    let (stream, received, server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let mut channel = channel(stream);
    let mut session = session();
    let started = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(5),
        Duration::from_secs(5),
        "0.1.0",
    )
    .await;
    match started {
        Ok(_) => {}
        Err(reason) => panic!(
            "a server that requires the initialized notification refused startup: {reason:?}"
        ),
    }
    assert!(session.state().is_ready());

    let log = match received.lock() {
        Ok(log) => log.clone(),
        Err(_) => panic!("the recorder was poisoned"),
    };
    let methods: Vec<&str> = log
        .iter()
        .map(|frame| frame.get("method").and_then(Value::as_str).unwrap_or(""))
        .collect();
    assert_eq!(
        methods,
        vec!["initialize", "notifications/initialized", "tools/list"],
        "the notification must be sent after the result and before discovery"
    );
    let notification = &log[1];
    assert!(
        notification.get("id").is_none(),
        "a notification carries no id; one with an id is a request the server owes a reply to"
    );
    assert_eq!(
        notification.get("jsonrpc").and_then(Value::as_str),
        Some("2.0")
    );
    assert!(
        notification.get("params").is_none(),
        "this notification takes no parameters, and inventing an empty one claims a shape the \
         specification does not define"
    );
    server.abort();
}

/// Negotiation is a set, not an equality.
///
/// `2024-11-05` is a revision this client understands and is not the one it
/// offers. A handshake that demanded its own offer back would refuse this
/// server outright, which is the difference between a picker that connects
/// to a real configured server and one that cannot.
#[tokio::test]
async fn a_server_answering_an_older_understood_revision_starts_and_records_that_revision() {
    let (stream, _received, server) = conformant(handshake_answering("2024-11-05"));
    let mut channel = channel(stream);
    let mut session = session();
    let identity = match start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(5),
        Duration::from_secs(5),
        "0.1.0",
    )
    .await
    {
        Ok(started) => started.identity,
        Err(reason) => panic!("an understood older revision was refused: {reason:?}"),
    };
    assert_eq!(
        identity.negotiated_protocol(),
        "2024-11-05",
        "the recorded revision is the server's answer, not this client's offer"
    );
    assert_ne!(identity.negotiated_protocol(), PROTOCOL_VERSION);
    assert!(session.state().is_ready());
    server.abort();
}

/// Accepting a set is not accepting anything. A revision outside the
/// declared list is still refused, because the list is what bounds the
/// message shapes this client claims to understand.
#[tokio::test]
async fn a_server_answering_a_revision_outside_the_declared_set_is_still_refused() {
    let (stream, _received, server) = conformant(handshake_answering("1999-01-01"));
    let mut channel = channel(stream);
    let mut session = session();
    let reason = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(5),
        Duration::from_secs(5),
        "0.1.0",
    )
    .await;
    match reason {
        Err(StartFailure {
            reason: Unusable::Failed(detail),
            ..
        }) => {
            assert!(
                detail.contains("1999-01-01"),
                "the refusal must name what the server actually answered: {detail}"
            );
        }
        other => panic!("an unknown revision must fail, not time out or connect: {other:?}"),
    }
    assert!(!session.state().is_ready());
    server.abort();
}
