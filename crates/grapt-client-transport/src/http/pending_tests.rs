//! Recovery acquisition must preserve bounded, truthful HTTP failures.
use super::*;
use std::net::TcpListener;

fn acquire(response: &'static [u8], bound: usize) -> Result<WireResponse, TransportError> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| TransportError::from_io("fixture", e))?;
    let address = listener
        .local_addr()
        .map_err(|e| TransportError::from_io("fixture", e))?;
    let peer = std::thread::spawn(move || -> Result<(), String> {
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        for _ in 0..100 {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_nonblocking(false).map_err(|e| e.to_string())?;
                    socket
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .map_err(|e| e.to_string())?;
                    socket
                        .set_write_timeout(Some(Duration::from_secs(1)))
                        .map_err(|e| e.to_string())?;
                    let mut request = Vec::new();
                    for _ in 0..64 {
                        let mut bytes = [0; 512];
                        let n = socket.read(&mut bytes).map_err(|e| e.to_string())?;
                        if n == 0 {
                            return Err("fixture request closed".into());
                        }
                        request.extend_from_slice(&bytes[..n]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") {
                            // Split the status/head/body over separate writes; the
                            // incremental parser must settle exactly the declared body.
                            let split = response.len().min(7);
                            socket
                                .write_all(&response[..split])
                                .map_err(|e| e.to_string())?;
                            std::thread::sleep(Duration::from_millis(10));
                            socket
                                .write_all(&response[split..])
                                .map_err(|e| e.to_string())?;
                            return Ok(());
                        }
                    }
                    return Err("fixture request bound".into());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("fixture did not receive a connection".into())
    });
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))
        .map_err(|e| TransportError::from_io("fixture connect", e))?;
    let mut request = PendingHttp::new(stream, b"GET / HTTP/1.1\r\n\r\n".to_vec(), bound, false)?;
    let mut outcome = None;
    for _ in 0..200 {
        match request.poll() {
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Ok(Some(response)) => {
                outcome = Some(Ok(response));
                break;
            }
            Err(error) => {
                outcome = Some(Err(error));
                break;
            }
        }
    }
    match peer.join() {
        Ok(Ok(())) => (),
        Ok(Err(error)) => panic!("response fixture failed: {error}"),
        Err(_) => panic!("response fixture panicked"),
    }
    outcome.unwrap_or_else(|| panic!("response never settled within its fixture bound"))
}

#[test]
fn split_head_and_body_settle_without_blocking() -> Result<(), TransportError> {
    let response = acquire(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok", 16)?;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"ok");
    Ok(())
}

#[test]
fn declared_and_undeclared_oversize_are_refused() {
    assert!(matches!(
        acquire(b"HTTP/1.1 200 OK\r\nContent-Length: 999999\r\n\r\n", 4),
        Err(TransportError::ResponseTooLarge { bound: 4 })
    ));
    assert!(matches!(
        acquire(b"HTTP/1.1 200 OK\r\n\r\n12345", 4),
        Err(TransportError::ResponseTooLarge { bound: 4 })
    ));
}

#[test]
fn truncated_body_is_retryable_but_malformed_status_is_fatal() {
    assert!(acquire(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nx", 8)
        .is_err_and(|error| error.is_transient()));
    assert!(acquire(b"not-an-http-status\r\n\r\n", 8).is_err_and(|error| !error.is_transient()));
}

#[test]
fn pending_request_deadline_and_debug_are_bounded_and_redacted() -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let stream =
        TcpStream::connect_timeout(&address, Duration::from_secs(1)).map_err(|e| e.to_string())?;
    let mut request = PendingHttp::new(
        stream,
        b"Authorization: Basic FIXTURE_SECRET\r\n\r\n".to_vec(),
        8,
        false,
    )
    .map_err(|e| e.to_string())?;
    assert!(!format!("{request:?}").contains("FIXTURE_SECRET"));
    request.deadline = tokio::time::Instant::now();
    assert!(request.poll().is_err_and(|error| error.is_transient()));
    Ok(())
}
