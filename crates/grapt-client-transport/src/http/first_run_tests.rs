//! Real socket witnesses for local product identity and response settlement.

use super::*;
use std::net::{TcpListener, TcpStream};

#[derive(Debug)]
struct Peer {
    address: SocketAddr,
    thread: std::thread::JoinHandle<Result<(), String>>,
}

impl Peer {
    fn health(body: String, declared: usize) -> Result<Self, String> {
        Self::response(200, body, declared, Duration::ZERO)
    }

    fn response(
        status: u16,
        body: String,
        declared: usize,
        delay: Duration,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let thread = std::thread::spawn(move || {
            let mut accepted = None;
            for _ in 0..100 {
                match listener.accept() {
                    Ok((stream, _)) => {
                        accepted = Some(stream);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
            let mut stream =
                Self::socket(accepted.ok_or("health client did not connect within one second")?)?;
            stream.deadline += delay;
            let mut request = Vec::new();
            for _ in 0..64 {
                let mut chunk = [0; 1024];
                let count = stream.read(&mut chunk).map_err(|error| error.to_string())?;
                if count == 0 {
                    return Err("health client closed before its request".into());
                }
                request.extend_from_slice(&chunk[..count]);
                // Delayed interrupt replies also consume the empty-object body.
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n")
                    && (status != 204 || request.ends_with(b"\r\n\r\n{}"))
                {
                    std::thread::sleep(delay);
                    let response = format!("HTTP/1.1 {status} Response\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n{body}");
                    stream
                        .write_all(response.as_bytes())
                        .map_err(|error| error.to_string())?;
                    return Ok(());
                }
            }
            Err("health request head exceeded its fixture bound".into())
        });
        Ok(Self { address, thread })
    }

    fn socket(stream: TcpStream) -> Result<window::SocketWindow, String> {
        // Windows accepts inherit the listener's nonblocking mode.
        stream
            .set_nonblocking(false)
            .map_err(|error| error.to_string())?;
        Ok(window::SocketWindow {
            stream,
            deadline: tokio::time::Instant::now() + Duration::from_secs(2),
        })
    }

    fn settle(self) -> Result<(), String> {
        self.thread.join().map_err(|_| "health fixture panicked")?
    }
}

#[test]
fn delayed_message_history_outlasts_the_control_request_window(
) -> Result<(), Box<dyn std::error::Error>> {
    let body = r#"{"data":[],"cursor":{}}"#;
    let peer = Peer::response(200, body.into(), body.len(), Duration::from_secs(6))?;
    let credential = BasicCredential::new("fixture", "DUMMY_AUTH")?;
    let mut connection = HttpConnection::attach(peer.address, credential, Bounds::PRODUCTION);
    let session = crate::wire::session_id("ses_000000000001")?;
    let request = WireRequest::new(Operation::MessageList, Some(&session), None)?;
    let response = connection.call(&request);
    peer.settle()?;
    let response = response?;
    assert_eq!(response.status, 200);
    assert_eq!(crate::wire::decode_messages(&response.body)?.rows, 0);
    Ok(())
}

#[test]
fn an_interrupt_waits_for_cleanup_beyond_the_ordinary_response_allowance() -> Result<(), String> {
    let peer = Peer::response(204, String::new(), 0, Duration::from_secs(6))?;
    let credential =
        BasicCredential::new("fixture", "DUMMY_AUTHENTICATION").map_err(|e| e.to_string())?;
    let mut connection = HttpConnection::attach(peer.address, credential, Bounds::PRODUCTION);
    let session = crate::wire::session_id("ses_000000000001").map_err(|e| e.to_string())?;
    let request = WireRequest::new(Operation::SessionInterrupt, Some(&session), None)
        .map_err(|e| e.to_string())?
        .with_body(crate::wire::encode_interrupt().map_err(|e| e.to_string())?);
    let result = connection.call(&request);
    peer.settle()?;
    assert_eq!(result.map_err(|e| e.to_string())?.status, 204);
    Ok(())
}

#[cfg(windows)]
#[test]
fn frc_accepted_health_socket_waits_for_its_read_allowance() -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let _client = TcpStream::connect_timeout(
        &listener.local_addr().map_err(|error| error.to_string())?,
        Duration::from_secs(2),
    )
    .map_err(|error| error.to_string())?;
    let (socket, _) = listener.accept().map_err(|error| error.to_string())?;
    // No client bytes can arrive: this thread owns both ends of the connection.
    assert_eq!(
        socket
            .peek(&mut [0])
            .err()
            .and_then(|error| error.raw_os_error()),
        Some(10035)
    );
    let mut socket = Peer::socket(socket)?.stream;
    socket
        .set_read_timeout(Some(Duration::from_millis(25)))
        .map_err(|error| error.to_string())?;
    let error = socket
        .read(&mut [0])
        .err()
        .ok_or("empty health socket returned data")?;
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
    Ok(())
}

// Attribution must succeed before opening a replacement event subscription.
// This peer serves one health response then closes its listener, so the
// matching product remains retryable while a different product is fatal.
fn finish_reconnect(connection: &mut HttpConnection) -> Result<Option<bool>, TransportError> {
    let mut result = connection.begin_reconnect();
    for _ in 0..200 {
        if !matches!(result, Ok(None)) {
            return result;
        }
        std::thread::sleep(Duration::from_millis(10));
        result = connection.poll_reconnect();
    }
    panic!("product attribution did not settle within its fixture bound")
}

#[test]
fn a_command_peer_checks_the_original_product_pin() -> Result<(), Box<dyn std::error::Error>> {
    let expected = grapt_api::ProductHealth::new(12345, "fixture", "local", "original")?;
    let replacement = grapt_api::ProductHealth::new(12346, "fixture", "local", "replacement")?;
    let body = replacement.encoded()?;
    let peer = Peer::health(body.clone(), body.len())?;
    let credential = BasicCredential::new("fixture", "DUMMY_AUTHENTICATION")?;
    let mut connection = HttpConnection::attach(peer.address, credential, Bounds::PRODUCTION);
    connection.pin_product(expected);
    let result = connection.command_peer()?.verify_pinned_product();
    peer.settle()?;
    assert!(result.is_err_and(|error| error.to_string().contains("different product")));
    Ok(())
}

#[test]
fn frc_replacement_product_is_refused_when_the_advisory_stream_closes() -> Result<(), String> {
    let expected = grapt_api::ProductHealth::new(12345, "frc-build", "local", "terminal")
        .map_err(|error| error.to_string())?;
    let replacement = grapt_api::ProductHealth::new(12346, "frc-build", "local", "terminal")
        .map_err(|error| error.to_string())?;
    let body = replacement.encoded().map_err(|error| error.to_string())?;
    let peer = Peer::health(body.clone(), body.len())?;
    let credential = BasicCredential::new("fixture", "FRC_DUMMY_AUTHENTICATION")
        .map_err(|error| error.to_string())?;
    let mut connection = HttpConnection::attach(peer.address, credential, Bounds::PRODUCTION);
    connection.pin_product(expected);
    let result = finish_reconnect(&mut connection);
    // Settle the bounded peer even if attribution was bypassed. Classify the
    // observed product result before a missing mutant connection can mask it.
    let fixture = peer.settle();
    let error = result
        .err()
        .ok_or("replacement product was treated as an ordinary stream close")?;
    fixture?;
    assert!(error.to_string().contains("different product"));
    Ok(())
}

#[test]
fn frc_live_product_remains_recoverable_after_an_advisory_stream_close() -> Result<(), String> {
    let expected = grapt_api::ProductHealth::new(12345, "frc-build", "local", "terminal")
        .map_err(|error| error.to_string())?;
    let body = expected.encoded().map_err(|error| error.to_string())?;
    let peer = Peer::health(body.clone(), body.len())?;
    let credential = BasicCredential::new("fixture", "FRC_DUMMY_AUTHENTICATION")
        .map_err(|error| error.to_string())?;
    let mut connection = HttpConnection::attach(peer.address, credential, Bounds::PRODUCTION);
    connection.pin_product(expected);
    let result = finish_reconnect(&mut connection);
    peer.settle()?;
    assert!(
        matches!(result, Err(ref error) if error.is_transient()),
        "live product advisory close was treated as process loss"
    );
    Ok(())
}

#[test]
fn frc_truncated_health_response_cannot_attest_a_product() -> Result<(), String> {
    let expected = grapt_api::ProductHealth::new(12345, "frc-build", "local", "terminal")
        .map_err(|error| error.to_string())?;
    let body = expected.encoded().map_err(|error| error.to_string())?;
    let peer = Peer::health(body.clone(), body.len() + 1)?;
    let credential = BasicCredential::new("fixture", "FRC_DUMMY_AUTHENTICATION")
        .map_err(|error| error.to_string())?;
    let connection = HttpConnection::attach(peer.address, credential, Bounds::PRODUCTION);
    let result = connection.verify_product(&expected);
    peer.settle()?;
    assert!(
        result.is_err(),
        "truncated health response was accepted as full attribution"
    );
    Ok(())
}

#[test]
fn frc_effect_does_not_leave_while_product_identity_is_pending() -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let expected = grapt_api::ProductHealth::new(12345, "frc-build", "local", "terminal")
        .map_err(|e| e.to_string())?;
    let credential =
        BasicCredential::new("fixture", "FRC_DUMMY_AUTHENTICATION").map_err(|e| e.to_string())?;
    let mut connection = HttpConnection::attach(address, credential, Bounds::PRODUCTION);
    connection.pin_product(expected);
    connection.closed().map_err(|e| e.to_string())?;
    let session = crate::wire::session_id("ses_000000000001").map_err(|e| e.to_string())?;
    let request = WireRequest::new(
        crate::contract::Operation::SessionCompact,
        Some(&session),
        None,
    )
    .map_err(|e| e.to_string())?;
    assert!(matches!(
        connection.call(&request),
        Err(TransportError::Unavailable { .. })
    ));
    assert!(
        matches!(listener.accept(), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "an effect connected before replacement-product identity was verified"
    );
    assert!(matches!(
        connection.begin_read(&request),
        Err(TransportError::Unavailable { .. })
    ));
    Ok(())
}
