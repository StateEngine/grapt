//! Bounded HTTP request/response helpers shared by the real-server journeys.
use super::*;

/// How long a helper keeps retrying a transient capacity wait on a safe read.
///
/// The server's own reader wait is 500 ms, so this covers several of them. It is
/// deliberately far below the journey's overall wall-clock budget: a genuinely
/// unavailable server still fails the test, just a little later.
const TRANSIENT_READ_DEADLINE: Duration = Duration::from_secs(5);
/// The wait when a transient answer carries no usable `Retry-After`.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_millis(150);

/// One response: its status, the `Retry-After` it named, and its body.
///
/// `Retry-After` is parsed rather than discarded: a helper that claims to honour
/// it and then sleeps its own constant would retry before the server asked and
/// flood a busy reader.
struct Answer {
    status: u16,
    retry_after: Option<Duration>,
    body: Vec<u8>,
}

pub(super) fn request(address: SocketAddr, method: &str, path: &str, body: &str) -> (u16, Vec<u8>) {
    let answer = answer(address, method, path, body, Duration::from_secs(8));
    (answer.status, answer.body)
}

pub(super) fn request_with_read_timeout(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: &str,
    read_timeout: Duration,
) -> (u16, Vec<u8>) {
    let answer = answer(address, method, path, body, read_timeout);
    (answer.status, answer.body)
}

fn answer(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: &str,
    read_timeout: Duration,
) -> Answer {
    let mut stream = TcpStream::connect(address)
        .unwrap_or_else(|error| panic!("connect {path} failed: {error}"));
    stream
        .set_read_timeout(Some(read_timeout))
        .unwrap_or_else(|error| panic!("request timeout failed: {error}"));
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: {AUTHORIZATION}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .unwrap_or_else(|error| panic!("write {path} failed: {error}"));
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap_or_else(|error| {
        panic!(
            "read {path} failed: {error}; received {} bytes, bounded prefix: {}",
            bytes.len(),
            String::from_utf8_lossy(&bytes[..bytes.len().min(2048)])
        )
    });
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("{path} returned no headers"));
    let head = String::from_utf8_lossy(&bytes[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or_else(|| panic!("{path} returned no status"));
    let body = &bytes[split + 4..];
    let chunked = head
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"));
    let body = if chunked {
        dechunk(path, body)
    } else {
        body.to_vec()
    };
    Answer {
        status,
        retry_after: retry_after(&head),
        body,
    }
}

/// The `Retry-After` a response named, in seconds, when it named one.
fn retry_after(head: &str) -> Option<Duration> {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("retry-after")
                .then(|| value.trim().parse::<u64>().ok())
        })
        .flatten()
        .map(Duration::from_secs)
}

/// A chunked HTTP/1.1 body, reassembled; a large response arrives this way.
fn dechunk(path: &str, mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .unwrap_or_else(|| panic!("{path}: chunk size line unterminated"));
        let size = std::str::from_utf8(&body[..line])
            .ok()
            .and_then(|raw| usize::from_str_radix(raw.split(';').next()?.trim(), 16).ok())
            .unwrap_or_else(|| panic!("{path}: chunk size unreadable"));
        body = &body[line + 2..];
        if size == 0 {
            return out;
        }
        assert!(body.len() >= size + 2, "{path}: chunk truncated");
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

/// A JSON body, retrying a transient capacity wait on a SAFE read only.
///
/// The client retries 408/429/5xx (`ClientError::is_transient`). A reader busy for
/// its own 500 ms wait answers 503 with `Retry-After`, so a single `assert_eq!(200)`
/// turned an ordinary capacity wait on a loaded machine into a failed journey.
///
/// Only GET and HEAD are retried. A mutation that answers 503 is returned to the
/// caller exactly as it arrived: the server may have committed before it said so,
/// and silently re-sending a write would hide the first attempt's outcome behind a
/// second, unrelated one. A 4xx is never retried either.
pub(super) fn json(address: SocketAddr, method: &str, path: &str, body: &str) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + TRANSIENT_READ_DEADLINE;
    loop {
        let response = answer(address, method, path, body, Duration::from_secs(8));
        let retryable = is_transient(response.status) && is_safe(method);
        if !retryable || tokio::time::Instant::now() >= deadline {
            assert_eq!(
                response.status,
                200,
                "{path}: {}",
                String::from_utf8_lossy(&response.body)
            );
            return decode_body(path, &response.body);
        }
        // Wait as the server asked, bounded by the overall read budget so a
        // pathological header cannot hold the helper past its own deadline.
        let wait = response.retry_after.unwrap_or(DEFAULT_RETRY_AFTER);
        std::thread::sleep(wait.min(deadline - tokio::time::Instant::now()));
    }
}

/// Methods that may be re-sent without a second effect.
fn is_safe(method: &str) -> bool {
    matches!(method, "GET" | "HEAD")
}

/// The statuses the real client treats as transient, plus a 503 from a busy reader.
fn is_transient(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

fn decode_body(path: &str, bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or_else(|error| {
        let at = error.column().saturating_sub(1).min(bytes.len());
        let window = &bytes[at.saturating_sub(120)..(at + 40).min(bytes.len())];
        panic!(
            "{path} response was not JSON: {error}; {} bytes, near: {:?}",
            bytes.len(),
            String::from_utf8_lossy(window)
        )
    })
}

pub(super) fn json_with_read_timeout(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: &str,
    read_timeout: Duration,
) -> serde_json::Value {
    let (status, bytes) = request_with_read_timeout(address, method, path, body, read_timeout);
    assert_eq!(status, 200, "{path}: {}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("{path} response was not JSON: {error}"))
}

pub(super) fn percent_encode(raw: &str) -> String {
    raw.as_bytes()
        .iter()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(*byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[path = "http/retry_review.rs"]
mod retry_review;
