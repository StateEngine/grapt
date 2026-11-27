//! The transport tells a reply lost after the request was sent apart from a
//! request that never left.

use super::*;

/// From a dry run: a reply lost AFTER the request was written may hide an
/// admitted request, so it is not the same failure as a request that never
/// left. A server that reads the request and closes without answering is
/// `AfterSend`; a refused connection stays `Io`. The server is bounded in
/// time and bytes and its failure is reported, so a client that never
/// connects fails this witness rather than hanging the gate.
#[test]
fn a_reply_lost_after_the_request_was_sent_is_told_apart_from_a_failed_send() {
    use std::io::Read as _;
    use std::net::TcpListener;

    let bind = || match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => panic!("a loopback listener must bind: {error}"),
    };
    let address = |listener: &TcpListener| match listener.local_addr() {
        Ok(address) => address,
        Err(error) => panic!("the listener must have an address: {error}"),
    };
    let Ok(credential) = BasicCredential::new("user", "secret") else {
        panic!("credential")
    };

    let silent = bind();
    let at = address(&silent);
    let server = std::thread::spawn(move || -> Result<(), String> {
        const DEADLINE: Duration = Duration::from_secs(10);
        const MAX_HEADER: usize = 64 * 1_024;
        let started = tokio::time::Instant::now();
        silent.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut socket = loop {
            match silent.accept() {
                Ok((socket, _)) => break socket,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() > DEADLINE {
                        return Err("the client never connected".to_owned());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e.to_string()),
            }
        };
        socket.set_nonblocking(false).map_err(|e| e.to_string())?;
        socket
            .set_read_timeout(Some(DEADLINE))
            .map_err(|e| e.to_string())?;
        // Read the whole request, then close without a byte of reply.
        let mut request = Vec::new();
        let mut chunk = [0u8; 1_024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            if request.len() > MAX_HEADER {
                return Err("the request header never ended".to_owned());
            }
            match socket.read(&mut chunk) {
                Ok(0) => return Err("the client closed before its header ended".to_owned()),
                Ok(count) => request.extend_from_slice(&chunk[..count]),
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    });
    let connection = HttpConnection::attach(at, credential.clone(), Bounds::PRODUCTION);
    let lost = connection.exchange(Method::Get, "/api/session", &[], None, 1_024, READ_TIMEOUT);
    match server.join() {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => panic!("the silent server failed: {reason}"),
        Err(_) => panic!("the silent server panicked"),
    }
    assert!(
        matches!(lost, Err(TransportError::AfterSend { .. })),
        "{lost:?}"
    );

    let closed = address(&bind());
    let refused = HttpConnection::attach(closed, credential, Bounds::PRODUCTION).exchange(
        Method::Get,
        "/api/session",
        &[],
        None,
        1_024,
        READ_TIMEOUT,
    );
    assert!(
        matches!(refused, Err(TransportError::Io { .. })),
        "{refused:?}"
    );
}
