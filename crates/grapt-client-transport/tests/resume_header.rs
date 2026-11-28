//! A subscribe carries `Last-Event-ID` only under the binding its resume token
//! names: the same endpoint, credential, watched Session and position class.
use grapt_client_transport::{
    bounds::Bounds,
    connection::{Connection, ResumeToken},
    http::{BasicCredential, HttpConnection},
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use tokio::time::Instant;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const HEAD_BOUND: usize = 16 * 1024;

/// A loopback service that answers every subscribe with an open event stream
/// and reports each request head, bounded in time and size.
struct Service {
    heads: mpsc::Receiver<String>,
    connection: HttpConnection,
}

fn service() -> TestResult<Service> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let (sent, heads) = mpsc::channel();
    std::thread::spawn(move || {
        let mut open: Vec<TcpStream> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let Ok((mut peer, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            };
            let head = read_head(&mut peer).unwrap_or_default();
            let _ = peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
            open.push(peer);
            if sent.send(head).is_err() {
                return;
            }
        }
    });
    Ok(Service {
        heads,
        connection: HttpConnection::attach(
            address,
            BasicCredential::new("fixture", "DUMMY_AUTH")?,
            Bounds::PRODUCTION,
        ),
    })
}

fn read_head(peer: &mut TcpStream) -> TestResult<String> {
    peer.set_nonblocking(false)?;
    peer.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut head = Vec::new();
    while head.len() < HEAD_BOUND {
        let mut byte = [0];
        peer.read_exact(&mut byte)?;
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(String::from_utf8(head)?);
        }
    }
    Err("request head exceeded its bound".into())
}

impl Service {
    /// Subscribe once and return the request head the service saw.
    fn subscribe(&mut self) -> TestResult<String> {
        self.connection.resubscribe();
        Ok(self.heads.recv_timeout(Duration::from_secs(10))?)
    }
}

fn resume_id(head: &str) -> Option<&str> {
    head.lines()
        .find_map(|line| line.strip_prefix("Last-Event-ID: "))
}

#[test]
fn a_stream_has_a_binding_only_while_it_is_open() -> TestResult {
    let mut service = service()?;
    assert_eq!(
        service.connection.stream_binding(),
        None,
        "nothing open yet"
    );
    service.subscribe()?;
    assert!(service.connection.stream_binding().is_some());
    service.connection.retire_stream();
    assert_eq!(service.connection.stream_binding(), None, "retired");
    Ok(())
}

#[test]
fn a_subscribe_under_the_tokens_binding_carries_its_id() -> TestResult {
    let mut service = service()?;
    let session = "ses_resume_header".parse()?;
    service.connection.watch_session(Some(session));
    let first = service.subscribe()?;
    assert_eq!(resume_id(&first), None, "no token, no resume");
    let binding = service.connection.stream_binding().ok_or("bound")?;
    service.connection.resume_from(Some(ResumeToken {
        id: "e1-42".into(),
        binding,
    }));
    let resumed = service.subscribe()?;
    assert_eq!(resume_id(&resumed), Some("e1-42"));
    assert!(resumed.contains("positions=1"));
    service.connection.resume_from(None);
    let withdrawn = service.subscribe()?;
    assert_eq!(resume_id(&withdrawn), None, "a withdrawn offer is not sent");
    Ok(())
}

#[test]
fn another_session_never_carries_the_id() -> TestResult {
    let mut service = service()?;
    service.connection.watch_session(Some("ses_first".parse()?));
    service.subscribe()?;
    let token = ResumeToken {
        id: "e1-7".into(),
        binding: service.connection.stream_binding().ok_or("bound")?,
    };
    service.connection.resume_from(Some(token.clone()));
    service
        .connection
        .watch_session(Some("ses_second".parse()?));
    let other_session = service.subscribe()?;
    assert_eq!(resume_id(&other_session), None, "another Session");

    Ok(())
}

#[test]
fn an_id_that_is_not_a_safe_header_value_is_never_sent() -> TestResult {
    let mut service = service()?;
    service.subscribe()?;
    let binding = service.connection.stream_binding().ok_or("bound")?;
    for id in ["", "e1-1\r\nX-Injected: 1", "e1 1", &"e".repeat(129)] {
        service.connection.resume_from(Some(ResumeToken {
            id: id.to_owned(),
            binding: binding.clone(),
        }));
        let head = service.subscribe()?;
        assert_eq!(resume_id(&head), None, "{id:?}");
        assert!(!head.contains("X-Injected"));
    }
    Ok(())
}
