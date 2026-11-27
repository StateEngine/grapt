//! A stalled tool-history response never spends the blocking five-second window.
use super::*;
use crate::contract::Operation;
use std::net::TcpListener;

#[test]
fn stalled_tool_history_yields_and_cancellation_closes_its_only_request(
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let credential = BasicCredential::new("fixture", "DUMMY_AUTH")?;
    let mut connection =
        HttpConnection::attach(listener.local_addr()?, credential, Bounds::PRODUCTION);
    let session = crate::wire::session_id("ses_000000000001")?;
    let request = WireRequest::new(Operation::MessageList, Some(&session), None)?;
    let began = tokio::time::Instant::now();
    assert!(connection.begin_tool_read(&request)?.is_none());
    let (mut peer, _) = listener.accept()?;
    peer.set_read_timeout(Some(Duration::from_secs(1)))?;
    assert!(connection.poll_tool_read()?.is_none());
    let mut bytes = [0; 2048];
    let length = peer.read(&mut bytes)?;
    assert!(bytes[..length].starts_with(b"GET /api/session/ses_000000000001/message?"));
    for _ in 0..100 {
        assert!(connection.poll_tool_read()?.is_none());
    }
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "tool reads blocked queued input"
    );
    assert!(connection.begin_read(&request)?.is_none());
    let (_history_peer, _) = listener.accept()?;
    assert!(connection.poll_read()?.is_none());
    std::thread::sleep(Duration::from_secs(6));
    assert!(connection.poll_read()?.is_none());
    assert!(connection.poll_tool_read()?.is_none());
    connection.cancel_read();
    connection.cancel_tool_read();
    assert_eq!(
        peer.read(&mut bytes)?,
        0,
        "cancel must close rather than retain the read"
    );
    assert!(matches!(
        connection.poll_tool_read(),
        Err(TransportError::NotConnected)
    ));
    assert!(
        listener
            .accept()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock),
        "poll/cancel must not replay any request"
    );
    Ok(())
}

/// The startup allowance is per instance and validated: zero is refused, the
/// shipped value is thirty seconds, and a form past an injected allowance is
/// refused at THAT deadline rather than production's.
#[test]
fn a_startup_allowance_is_per_instance_never_zero_and_the_one_spent(
) -> Result<(), Box<dyn std::error::Error>> {
    const ACTIVE: Duration = Duration::from_millis(200);
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let credential = BasicCredential::new("fixture", "DUMMY_AUTH")?;
    assert!(
        HttpConnection::attach_with_startup_allowance(
            listener.local_addr()?,
            credential.clone(),
            Bounds::PRODUCTION,
            Duration::ZERO,
        )
        .is_err(),
        "a zero startup allowance was accepted"
    );
    assert_eq!(
        STARTUP_ACTIVE_ALLOWANCE,
        Duration::from_secs(30),
        "production moved"
    );
    let mut connection = HttpConnection::attach_with_startup_allowance(
        listener.local_addr()?,
        credential,
        Bounds::PRODUCTION,
        ACTIVE,
    )?;
    let request = WireRequest::new(Operation::McpInspect, None, None)?
        .with_query("location[directory]", "/native-startup");
    assert!(connection.begin_operator(&request)?.is_none());
    let (mut peer, _) = listener.accept()?;
    peer.set_read_timeout(Some(Duration::from_secs(1)))?;
    assert!(connection.poll_operator()?.is_none());
    let mut bytes = [0; 2048];
    assert!(
        peer.read(&mut bytes)? > 0,
        "the request never reached the peer"
    );
    // Bounded by iterations, never by a clock reading.
    let refusal = (0..200)
        .find_map(|_| match connection.poll_operator() {
            Err(error) => Some(error),
            Ok(_) => {
                std::thread::sleep(Duration::from_millis(10));
                None
            }
        })
        .ok_or("the injected active allowance never expired; the override was ignored")?;
    assert!(
        matches!(&refusal, TransportError::Unavailable { reason }
            if reason == "response acquisition allowance elapsed"),
        "a spent active allowance was reported as something else: {refusal:?}"
    );
    Ok(())
}
