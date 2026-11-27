//! R forwards the shared incremental lane instead of falling back to blocking RPC.
use super::*;
use grapt_client::{wire::session_id, BasicCredential, Operation};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::Duration,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn fixture() -> Result<(Bridge, TcpListener)> {
    fixture_with_startup_allowance(grapt_client::http::STARTUP_ACTIVE_ALLOWANCE)
}

/// The same bridge fixture with an explicit active startup allowance, so a
/// witness observes the allowance without waiting out production's thirty
/// seconds.
fn fixture_with_startup_allowance(allowance: Duration) -> Result<(Bridge, TcpListener)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let rpc = HttpConnection::attach_with_startup_allowance(
        listener.local_addr()?,
        BasicCredential::new("fixture", "local-only")?,
        Bounds::PRODUCTION,
        allowance,
    )?;
    let (_events, frames) = mpsc::sync_channel(32);
    let (reconnect, _retry) = mpsc::sync_channel(1);
    Ok((Bridge::assembled(rpc, frames, reconnect), listener))
}

fn begin(bridge: &mut Bridge, listener: &TcpListener) -> Result<TcpStream> {
    let session = session_id("ses_000000000001")?;
    let request = WireRequest::new(Operation::MessageList, Some(&session), None)?;
    assert!(bridge.begin_tool_read(&request)?.is_none());
    let (mut peer, _) = listener.accept()?;
    peer.set_nonblocking(false)?;
    peer.set_read_timeout(Some(Duration::from_secs(1)))?;
    peer.set_write_timeout(Some(Duration::from_secs(1)))?;
    assert!(bridge.poll_tool_read()?.is_none());
    let mut bytes = [0; 2048];
    let count = peer.read(&mut bytes)?;
    assert!(bytes[..count].starts_with(b"GET /api/session/ses_000000000001/message?"));
    Ok(peer)
}

#[test]
fn a_held_tool_read_yields_and_cancel_closes_the_same_socket() -> Result {
    let (mut bridge, listener) = fixture()?;
    let began = tokio::time::Instant::now();
    let mut peer = begin(&mut bridge, &listener)?;
    for _ in 0..100 {
        assert!(bridge.poll_tool_read()?.is_none());
        assert_eq!(bridge.poll_frame()?, FramePoll::Idle);
    }
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "R used a blocking tool read"
    );
    bridge.cancel_tool_read();
    let mut bytes = [0; 16];
    assert_eq!(
        peer.read(&mut bytes)?,
        0,
        "cancel retained the request socket"
    );
    assert!(matches!(
        bridge.poll_tool_read(),
        Err(TransportError::NotConnected)
    ));
    assert!(listener
        .accept()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock));
    Ok(())
}

#[test]
fn a_partial_tool_response_completes_without_another_event_or_request() -> Result {
    let (mut bridge, listener) = fixture()?;
    let mut peer = begin(&mut bridge, &listener)?;
    peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{")?;
    assert!(bridge.poll_tool_read()?.is_none());
    peer.write_all(b"}")?;
    let mut response = None;
    for _ in 0..1000 {
        response = bridge.poll_tool_read()?;
        if response.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let response = response.ok_or("incremental tool response did not complete")?;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"{}");
    assert!(matches!(
        bridge.poll_tool_read(),
        Err(TransportError::NotConnected)
    ));
    assert!(listener
        .accept()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock));
    Ok(())
}

/// The injected ACTIVE allowance, small enough to cross in a test run.
const ACTIVE: Duration = Duration::from_millis(200);

#[test]
fn startup_human_wait_reaches_the_native_bridge_http_budget() -> Result {
    let (mut bridge, listener) = fixture_with_startup_allowance(ACTIVE)?;
    // O1: one real McpInspect reaches the peer as one written request.
    let request = WireRequest::new(Operation::McpInspect, None, None)?
        .with_query("location[directory]", "/native-startup");
    assert!(bridge.begin_operator(&request)?.is_none());
    let (mut peer, _) = listener.accept()?;
    peer.set_nonblocking(false)?;
    peer.set_read_timeout(Some(Duration::from_secs(1)))?;
    peer.set_write_timeout(Some(Duration::from_secs(1)))?;
    assert!(bridge.poll_operator()?.is_none());
    let mut bytes = [0; 2048];
    assert!(
        peer.read(&mut bytes)? > 0,
        "the request never reached the peer"
    );
    // O2: the human wait begins.
    bridge.operator_human_wait(true);
    // O3: waiting well past the ACTIVE allowance still returns Pending,
    // because human wait charges the separate, production-sized allowance.
    std::thread::sleep(ACTIVE * 3);
    assert!(
        bridge.poll_operator()?.is_none(),
        "a human wait past the active allowance was refused instead of held"
    );
    // O4: the human wait ends and the peer's reply completes successfully.
    bridge.operator_human_wait(false);
    peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")?;
    let mut response = None;
    for _ in 0..1000 {
        response = bridge.poll_operator()?;
        if response.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        response.ok_or("startup inspection did not finish")?.body,
        b"{}"
    );
    Ok(())
}
