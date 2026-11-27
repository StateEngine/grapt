//! Explicit external approver lifetime for process journeys that answer asks.
use super::*;

pub(super) struct Attachment {
    stream: TcpStream,
    drain: Option<thread::JoinHandle<()>>,
}

impl Attachment {
    pub(super) fn open(address: SocketAddr, scope: &str) -> Self {
        assert!(
            scope == "*" || scope.parse::<SessionId>().is_ok(),
            "invalid fixture approval scope"
        );
        let mut stream =
            TcpStream::connect(address).unwrap_or_else(|e| panic!("approver connect: {e}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap_or_else(|e| panic!("approver timeout: {e}"));
        write!(stream, "GET /api/event HTTP/1.1\r\nHost: {address}\r\nAuthorization: {AUTHORIZATION}\r\nX-Grapt-Approver: {scope}\r\nConnection: close\r\n\r\n").unwrap_or_else(|e| panic!("approver request: {e}"));
        let mut head = Vec::new();
        for _ in 0..16_384 {
            let mut byte = [0_u8; 1];
            stream
                .read_exact(&mut byte)
                .unwrap_or_else(|e| panic!("approver handshake: {e}"));
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        assert!(head.ends_with(b"\r\n\r\n"), "over-bound approver handshake");
        assert!(
            String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"),
            "approver refused: {}",
            String::from_utf8_lossy(&head)
        );
        let mut reader = stream
            .try_clone()
            .unwrap_or_else(|e| panic!("approver reader: {e}"));
        reader
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap_or_else(|e| panic!("approver drain timeout: {e}"));
        let drain = thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            for _ in 0..36_000 {
                match reader.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(_) => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                        ) => {}
                    Err(_) => return,
                }
            }
            panic!("approver fixture exceeded its bounded drain lifetime");
        });
        Self {
            stream,
            drain: Some(drain),
        }
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        if let Some(drain) = self.drain.take() {
            if drain.join().is_err() && !thread::panicking() {
                panic!("approver drain panicked");
            }
        }
    }
}
