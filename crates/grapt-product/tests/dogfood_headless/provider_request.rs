//! Bounded HTTP request parsing for the scripted provider.
use std::io::{Cursor, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use super::super::client_allowance;
use super::{PROVIDER_BODY_BYTES, PROVIDER_HEADER_BYTES, PROVIDER_REQUEST_BYTES};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ProviderRequestReadError {
    #[error("provider request header exceeded the 65,536-byte bound")]
    HeaderTooLarge,
    #[error("provider request omitted Content-Length ({request_line}; Transfer-Encoding={transfer_encoding:?})")]
    MissingContentLength {
        request_line: String,
        transfer_encoding: Option<String>,
    },
    #[error("provider request carried invalid Content-Length {value:?}")]
    InvalidContentLength { value: String },
    #[error("provider request body declared {declared} bytes, exceeding the 524,288-byte bound")]
    BodyTooLarge { declared: usize },
    #[error(
        "provider request aggregate declared {declared} bytes, exceeding the 524,288-byte bound"
    )]
    AggregateTooLarge { declared: usize },
    #[error("provider request aggregate length overflowed")]
    AggregateLengthOverflow,
    #[error("provider request ended before its bounded body")]
    EndedBeforeBody,
    #[error("provider request was disconnected before its bounded body: {0}")]
    Disconnected(String),
    #[error("provider request read failed: {0}")]
    Read(String),
}

impl ProviderRequestReadError {
    /// The client went away mid-request, as a product killed by its own cut
    /// does: the request ended early, never malformed or oversized.
    pub(super) fn is_truncation(&self) -> bool {
        matches!(self, Self::EndedBeforeBody | Self::Disconnected(_))
    }
}

pub(crate) fn read_provider_request(
    stream: &mut TcpStream,
) -> Result<(String, String), ProviderRequestReadError> {
    read_provider_request_within(stream, client_allowance::CLI_REQUEST_ALLOWANCE)
}

fn read_provider_request_within(
    stream: &mut TcpStream,
    allowance: Duration,
) -> Result<(String, String), ProviderRequestReadError> {
    read_bounded_provider_request(&mut RequestDeadline {
        stream,
        deadline: client_allowance::now() + allowance,
    })
}

/// Socket timeouts are polls inside one bounded request, not malformed input.
/// The absolute deadline also refuses a peer that keeps trickling bytes.
struct RequestDeadline<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
}

impl Read for RequestDeadline<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let remaining = self
                .deadline
                .saturating_duration_since(client_allowance::now());
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    "provider request deadline passed",
                ));
            }
            self.stream
                .set_read_timeout(Some(remaining.min(Duration::from_secs(5))))?;
            match self.stream.read(bytes) {
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                result => return result,
            }
        }
    }
}

fn read_bounded_provider_request<R: Read>(
    stream: &mut R,
) -> Result<(String, String), ProviderRequestReadError> {
    let mut bytes = Vec::with_capacity(4_096);
    let mut chunk = [0u8; 4_096];
    let mut body_end = None;
    loop {
        if let Some(end) = body_end {
            if bytes.len() >= end {
                let split = bytes
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .ok_or(ProviderRequestReadError::EndedBeforeBody)?;
                let body_start = split
                    .checked_add(4)
                    .ok_or(ProviderRequestReadError::AggregateLengthOverflow)?;
                return Ok((
                    String::from_utf8_lossy(&bytes[..split]).into_owned(),
                    String::from_utf8_lossy(&bytes[body_start..end]).into_owned(),
                ));
            }
        }

        let allowance = body_end
            .map(|end| end.saturating_sub(bytes.len()))
            .unwrap_or(chunk.len())
            .min(chunk.len());
        if allowance == 0 {
            return Err(ProviderRequestReadError::EndedBeforeBody);
        }
        let count = stream
            .read(&mut chunk[..allowance])
            .map_err(|error| match error.kind() {
                ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::BrokenPipe => {
                    ProviderRequestReadError::Disconnected(error.to_string())
                }
                _ => ProviderRequestReadError::Read(error.to_string()),
            })?;
        if count == 0 {
            return Err(ProviderRequestReadError::EndedBeforeBody);
        }
        let next = bytes
            .len()
            .checked_add(count)
            .ok_or(ProviderRequestReadError::AggregateLengthOverflow)?;
        if body_end.is_none() && next > PROVIDER_HEADER_BYTES {
            return Err(ProviderRequestReadError::HeaderTooLarge);
        }
        bytes.extend_from_slice(&chunk[..count]);
        if body_end.is_some() {
            continue;
        }
        let Some(split) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            if bytes.len() == PROVIDER_HEADER_BYTES {
                return Err(ProviderRequestReadError::HeaderTooLarge);
            }
            continue;
        };
        let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
        let raw_length = head.lines().find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().to_owned())
            })
        });
        let raw_length =
            raw_length.ok_or_else(|| ProviderRequestReadError::MissingContentLength {
                request_line: head
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(256)
                    .collect(),
                transfer_encoding: head.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("transfer-encoding")
                        .then(|| value.trim().chars().take(64).collect())
                }),
            })?;
        let length = raw_length.parse::<usize>().map_err(|_| {
            ProviderRequestReadError::InvalidContentLength {
                value: raw_length.clone(),
            }
        })?;
        if length > PROVIDER_BODY_BYTES {
            return Err(ProviderRequestReadError::BodyTooLarge { declared: length });
        }
        let body_start = split
            .checked_add(4)
            .ok_or(ProviderRequestReadError::AggregateLengthOverflow)?;
        let declared = body_start
            .checked_add(length)
            .ok_or(ProviderRequestReadError::AggregateLengthOverflow)?;
        if declared > PROVIDER_REQUEST_BYTES {
            return Err(ProviderRequestReadError::AggregateTooLarge { declared });
        }
        body_end = Some(declared);
    }
}

fn provider_request_refusal(bytes: Vec<u8>) -> ProviderRequestReadError {
    match read_bounded_provider_request(&mut Cursor::new(bytes)) {
        Ok((head, body)) => panic!(
            "hostile provider request unexpectedly succeeded with head {head:?} and body {body:?}"
        ),
        Err(error) => error,
    }
}

#[test]
fn provider_request_reader_bounds_header_body_and_aggregate_without_coercing_length() {
    let template = b"POST /responses HTTP/1.1\r\nContent-Length: 0000000\r\n\r\n";
    let exact_body = PROVIDER_REQUEST_BYTES - template.len();
    let mut exact =
        format!("POST /responses HTTP/1.1\r\nContent-Length: {exact_body}\r\n\r\n").into_bytes();
    assert_eq!(exact.len() + exact_body, PROVIDER_REQUEST_BYTES);
    exact.resize(PROVIDER_REQUEST_BYTES, b'x');
    let (head, body) = read_bounded_provider_request(&mut Cursor::new(exact))
        .unwrap_or_else(|error| panic!("exact-bound provider request was refused: {error}"));
    assert!(head.starts_with("POST /responses HTTP/1.1"));
    assert_eq!(body.len(), exact_body);
    assert!(body.bytes().all(|byte| byte == b'x'));

    assert_eq!(
        provider_request_refusal(vec![b'h'; PROVIDER_HEADER_BYTES + 1]),
        ProviderRequestReadError::HeaderTooLarge,
        "provider request header exceeded the 65,536-byte bound"
    );
    assert_eq!(
        provider_request_refusal(
            format!(
                "POST /responses HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                PROVIDER_BODY_BYTES + 1
            )
            .into_bytes()
        ),
        ProviderRequestReadError::BodyTooLarge {
            declared: PROVIDER_BODY_BYTES + 1
        }
    );
    let aggregate = format!(
        "POST /responses HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        exact_body + 1
    )
    .into_bytes();
    assert_eq!(
        provider_request_refusal(aggregate),
        ProviderRequestReadError::AggregateTooLarge {
            declared: PROVIDER_REQUEST_BYTES + 1
        }
    );
    assert_eq!(
        provider_request_refusal(b"POST /responses HTTP/1.1\r\nHost: loopback\r\n\r\n".to_vec()),
        ProviderRequestReadError::MissingContentLength {
            request_line: "POST /responses HTTP/1.1".to_owned(),
            transfer_encoding: None,
        }
    );
    assert_eq!(
        provider_request_refusal(
            b"POST /responses HTTP/1.1\r\nContent-Length: absent\r\n\r\n".to_vec()
        ),
        ProviderRequestReadError::InvalidContentLength {
            value: "absent".to_owned()
        }
    );
}

/// A reader that yields `prefix`, then fails as a reset connection does.
struct ResetAfter(Cursor<Vec<u8>>);

impl Read for ResetAfter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.0.read(buf)? {
            0 => Err(std::io::Error::from(ErrorKind::ConnectionReset)),
            count => Ok(count),
        }
    }
}

#[test]
fn only_a_request_cut_short_by_its_client_is_a_truncation() {
    let head = b"POST /responses HTTP/1.1\r\nContent-Length: 100\r\n\r\npartial".to_vec();
    let ended = provider_request_refusal(head.clone());
    assert_eq!(ended, ProviderRequestReadError::EndedBeforeBody);
    assert!(ended.is_truncation());
    let reset = match read_bounded_provider_request(&mut ResetAfter(Cursor::new(head))) {
        Ok(request) => panic!("a reset mid-body read as a request: {request:?}"),
        Err(error) => error,
    };
    assert!(
        matches!(reset, ProviderRequestReadError::Disconnected(_)),
        "{reset:?}"
    );
    assert!(reset.is_truncation());
    for malformed in [
        b"POST /responses HTTP/1.1\r\nHost: loopback\r\n\r\n".to_vec(),
        b"POST /responses HTTP/1.1\r\nContent-Length: absent\r\n\r\n".to_vec(),
        vec![b'h'; PROVIDER_HEADER_BYTES + 1],
    ] {
        let refused = provider_request_refusal(malformed);
        assert!(!refused.is_truncation(), "{refused:?}");
    }
}

fn socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| panic!("request fixture bind: {error}"));
    listener
        .set_nonblocking(true)
        .unwrap_or_else(|error| panic!("request fixture nonblocking: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("request fixture address: {error}"));
    let client = TcpStream::connect_timeout(&address, Duration::from_secs(5))
        .unwrap_or_else(|error| panic!("request fixture connect: {error}"));
    let (server, _) = listener
        .accept()
        .unwrap_or_else(|error| panic!("request fixture accept: {error}"));
    server
        .set_nonblocking(false)
        .unwrap_or_else(|error| panic!("request fixture blocking mode: {error}"));
    (client, server)
}

#[test]
fn a_request_delayed_past_a_socket_poll_keeps_its_bounded_body() {
    let (mut client, mut server) = socket_pair();
    client
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap_or_else(|error| panic!("request fixture write timeout: {error}"));
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(6));
        client.write_all(b"POST /responses HTTP/1.1\r\nContent-Length: 4\r\n\r\nbody")
    });
    let request = read_provider_request(&mut server);
    writer
        .join()
        .unwrap_or_else(|_| panic!("delayed request writer panicked"))
        .unwrap_or_else(|error| panic!("delayed request write: {error}"));
    let (head, body) = request
        .unwrap_or_else(|error| panic!("a temporary socket poll refused the request: {error}"));
    assert!(head.starts_with("POST /responses HTTP/1.1"));
    assert_eq!(body, "body");
}

#[test]
fn a_silent_request_exhausts_its_deadline_without_becoming_a_truncation() {
    let (_client, mut server) = socket_pair();
    let error = read_provider_request_within(&mut server, Duration::from_millis(25))
        .err()
        .unwrap_or_else(|| panic!("a silent request produced a body"));
    assert!(
        matches!(&error, ProviderRequestReadError::Read(reason) if reason.contains("deadline passed")),
        "{error}"
    );
    assert!(!error.is_truncation(), "{error}");
}

/// Split a real HTTP header/body at each boundary, including inside the
/// Content-Length name and its value. A short socket read cannot erase it.
#[test]
fn content_length_survives_every_request_read_boundary() {
    struct SplitRead<'a> {
        remaining: &'a [u8],
        first: usize,
    }
    impl Read for SplitRead<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let cap = if self.first == 0 {
                out.len()
            } else {
                self.first
            };
            let n = self.remaining.len().min(out.len()).min(cap);
            out[..n].copy_from_slice(&self.remaining[..n]);
            self.remaining = &self.remaining[n..];
            self.first = self.first.saturating_sub(n);
            Ok(n)
        }
    }
    let wire =
        b"POST /chat/completions HTTP/1.1\r\nhost: loopback\r\ncontent-length: 7\r\n\r\n{\"x\":1}";
    for split in 1..wire.len() {
        let (head, body) = read_bounded_provider_request(&mut SplitRead {
            remaining: wire,
            first: split,
        })
        .unwrap_or_else(|error| panic!("request split at {split} failed: {error}"));
        assert!(head.starts_with("POST /chat/completions HTTP/1.1"));
        assert_eq!(body, r#"{"x":1}"#, "split at {split}");
    }
}
