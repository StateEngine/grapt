//! Deterministic failures on either side of the complete-request boundary.
use super::*;
use std::io;

#[derive(Clone, Copy)]
enum FailAt {
    Head,
    Body,
    Flush,
}

struct FailingWriter {
    bytes: Vec<u8>,
    fail_at: FailAt,
}

impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if matches!(self.fail_at, FailAt::Head)
            || (matches!(self.fail_at, FailAt::Body) && !self.bytes.is_empty())
        {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "write refused"));
        }
        assert!(self.bytes.len() + bytes.len() <= 4096, "fixture bound");
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::TimedOut, "flush expired"))
    }
}

fn fail_request(fail_at: FailAt) -> (TransportError, Vec<u8>) {
    let credential = BasicCredential::new("user", "secret")
        .unwrap_or_else(|error| panic!("fixture credential: {error}"));
    let address = "127.0.0.1:1"
        .parse()
        .unwrap_or_else(|error| panic!("fixture address: {error}"));
    let connection = HttpConnection::attach(address, credential, Bounds::PRODUCTION);
    let mut writer = FailingWriter {
        bytes: Vec::new(),
        fail_at,
    };
    let error = connection
        .write_request(
            &mut writer,
            Method::Post,
            "/api/session/ses_test/prompt",
            &[],
            Some(b"{}"),
        )
        .err()
        .unwrap_or_else(|| panic!("fixture stage must fail"));
    (error, writer.bytes)
}

/// Client/App witnesses inject the class returned by the real HTTP writer,
/// after their contract peer admits the request. No socket timing is assumed.
pub(crate) fn failure_after_body() -> TransportError {
    let (error, bytes) = fail_request(FailAt::Flush);
    assert!(bytes.starts_with(b"POST /api/session/ses_test/prompt HTTP/1.1\r\n"));
    assert!(
        bytes.ends_with(b"\r\n\r\n{}"),
        "complete body was not written"
    );
    error
}

pub(crate) fn failure_before_send() -> TransportError {
    let (error, bytes) = fail_request(FailAt::Head);
    assert!(bytes.is_empty());
    error
}

#[test]
fn a_flush_failure_after_the_complete_body_is_uncertain() {
    let error = failure_after_body();
    assert!(
        matches!(error, TransportError::AfterSend { .. }),
        "{error:?}"
    );
}

#[test]
fn failed_head_or_incomplete_body_writes_are_still_unsent() {
    assert!(matches!(failure_before_send(), TransportError::Io { .. }));
    let (error, bytes) = fail_request(FailAt::Body);
    assert!(bytes.ends_with(b"\r\n\r\n"));
    assert!(!bytes.ends_with(b"{}"));
    assert!(matches!(error, TransportError::Io { .. }), "{error:?}");
}

#[test]
fn seat01_tui_attaches_an_approver_only_on_its_event_subscription(
) -> Result<(), Box<dyn std::error::Error>> {
    let connection = HttpConnection::attach(
        "127.0.0.1:1".parse()?,
        BasicCredential::new("test", "test")?,
        Bounds::PRODUCTION,
    );
    for path in ["/api/event", "/api/session"] {
        let mut request = Vec::new();
        connection.write_request(&mut request, Method::Get, path, &[], None)?;
        let request = String::from_utf8(request)?;
        assert_eq!(
            request.contains("X-Grapt-Approver: *\r\n"),
            path == "/api/event",
            "TUI approver role escaped its subscription"
        );
    }
    Ok(())
}
