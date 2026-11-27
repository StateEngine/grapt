use core::time::Duration;
use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::http1::SEAT_CREDENTIAL_HEADER;

// ---------------------------------------------------------------------------
// A raw HTTP/1 client. Bytes on a socket, not a hyper client: enabling a client
// stack to test a server would make the witness depend on the same parser it is
// probing.
// ---------------------------------------------------------------------------

pub(super) struct RawConnection {
    stream: TcpStream,
    buffer: Vec<u8>,
}

impl RawConnection {
    pub(super) async fn open(address: SocketAddr) -> Self {
        match TcpStream::connect(address).await {
            Ok(stream) => Self {
                stream,
                buffer: Vec::new(),
            },
            Err(error) => panic!("the witness client could not connect to {address}: {error}"),
        }
    }

    pub(super) async fn send(&mut self, bytes: &[u8]) {
        if let Err(error) = self.stream.write_all(bytes).await {
            panic!("the witness client could not write: {error}");
        }
    }

    /// Read until the response head is complete, then return status and head.
    ///
    /// Only the head, deliberately: a witness that also drained the body could
    /// not distinguish "headers produced" from "body finished", which is exactly
    /// the distinction the in-flight permit turns on.
    pub(super) async fn read_head(&mut self) -> RawResponse {
        loop {
            if let Some(end) = find_head_end(&self.buffer) {
                let head = String::from_utf8_lossy(&self.buffer[..end]).into_owned();
                self.buffer.drain(..end);
                return RawResponse::parse(&head);
            }
            let mut chunk = [0u8; 4096];
            // BOUNDED at the method, once, rather than at sixteen call sites.
            // Several mutations do not make a request fail — they make it never
            // be answered. `QH06` removes the pre-refusal of a known oversized
            // `Content-Length`, so the route waits for a body the witness
            // deliberately never sends; without this bound the row hangs until
            // the battery's step timeout and reports a timeout instead of a law.
            // Thirty seconds is far longer than any legitimate answer on
            // loopback and far shorter than the four hours a stall would cost.
            match tokio::time::timeout(Duration::from_secs(30), self.stream.read(&mut chunk)).await
            {
                Err(_) => panic!(
                    "no response head arrived within thirty seconds, so this request \
                     was neither answered nor refused; a bound that should have \
                     refused it before the route did not. Read so far: {:?}",
                    String::from_utf8_lossy(&self.buffer)
                ),
                Ok(Ok(0)) => panic!(
                    "the connection closed before a response head arrived; read so far: {:?}",
                    String::from_utf8_lossy(&self.buffer)
                ),
                Ok(Ok(read)) => self.buffer.extend_from_slice(&chunk[..read]),
                Ok(Err(error)) => panic!("the witness client could not read: {error}"),
            }
        }
    }

    /// Read to end of stream, returning every byte that arrived after the head.
    ///
    /// The bytes are RETURNED rather than counted, because the difference
    /// between a body that finished and a connection that was cut is visible
    /// only in them: a chunked response ends with the zero-length chunk
    /// `0\r\n\r\n`, and a connection that stopped being polled after its
    /// graceful shutdown was signalled closes without ever writing it.
    /// Each read is BOUNDED, and the bound is not defensive padding: several
    /// mutations leave the server holding a keep-alive connection open forever,
    /// and an unbounded read would turn those rows into a battery that hangs
    /// until its step timeout instead of a witness that fails. On expiry this
    /// returns what actually arrived, so the assertion that follows reports the
    /// missing bytes rather than a timeout with no content.
    pub(super) async fn drain_to_close(&mut self) -> Vec<u8> {
        let mut collected = core::mem::take(&mut self.buffer);
        let mut chunk = [0u8; 4096];
        loop {
            match tokio::time::timeout(Duration::from_secs(10), self.stream.read(&mut chunk)).await
            {
                Ok(Ok(0)) | Err(_) => return collected,
                Ok(Ok(read)) => collected.extend_from_slice(&chunk[..read]),
                Ok(Err(_)) => return collected,
            }
        }
    }
}

/// A parsed response head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawResponse {
    pub(super) status: u16,
    pub(super) head: String,
}

impl RawResponse {
    fn parse(head: &str) -> Self {
        let Some(first) = head.lines().next() else {
            panic!("an empty response head");
        };
        let mut parts = first.split_whitespace();
        let _version = parts.next();
        let Some(code) = parts.next() else {
            panic!("a response head with no status code: {first:?}");
        };
        let Ok(status) = code.parse::<u16>() else {
            panic!("a response head with a non-numeric status: {code:?}");
        };
        Self {
            status,
            head: head.to_owned(),
        }
    }

    pub(super) fn says_close(&self) -> bool {
        self.head
            .lines()
            .any(|line| line.to_ascii_lowercase().starts_with("connection: close"))
    }
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|start| start + 4)
}

/// A `GET /witness` authenticated as `subject`.
pub(super) fn authenticated_get(subject: &str) -> Vec<u8> {
    format!("GET /witness HTTP/1.1\r\nHost: witness\r\n{SEAT_CREDENTIAL_HEADER}: {subject}\r\n\r\n")
        .into_bytes()
}
