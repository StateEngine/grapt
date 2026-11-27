use super::*;
use grapt_client::{wire::session_id, BasicCredential, HttpConnection};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::Duration,
};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn accept(listener: &TcpListener) -> Result<TcpStream> {
    let started = tokio::time::Instant::now();
    loop {
        match listener.accept() {
            Ok((socket, _)) => {
                socket.set_nonblocking(false)?;
                socket.set_read_timeout(Some(Duration::from_secs(2)))?;
                socket.set_write_timeout(Some(Duration::from_secs(2)))?;
                return Ok(socket);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if started.elapsed() >= Duration::from_secs(2) {
                    return Err("backend request did not arrive".into());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn read_request(socket: &mut TcpStream) -> Result<Vec<u8>> {
    let mut request = Vec::new();
    loop {
        let mut bytes = [0; 2048];
        let count = socket.read(&mut bytes)?;
        if count == 0 || request.len() + count > 8192 {
            return Err("incomplete or oversized fixture request".into());
        }
        request.extend_from_slice(&bytes[..count]);
        if let Some(at) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let header = std::str::from_utf8(&request[..at])?;
            let length = header
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .map(str::parse::<usize>)
                .transpose()?
                .unwrap_or(0);
            if request.len() >= at + 4 + length {
                return Ok(request);
            }
        }
    }
}

fn respond(socket: &mut TcpStream, status: u16, body: &[u8]) -> Result {
    write!(
        socket,
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    socket.write_all(body)?;
    Ok(())
}

#[test]
fn a_held_interrupt_allows_backend_commands_and_reports_its_refusal() -> Result {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let rpc = HttpConnection::attach(
        listener.local_addr()?,
        BasicCredential::new("fixture", "local-only")?,
        Bounds::PRODUCTION,
    );
    let mut client = Client::new(Bridge::test_rpc(rpc), Bounds::PRODUCTION);
    let session = session_id("ses_000000000001")?;
    let (admitted, admission) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || -> Result {
        let mut stop = accept(&listener)?;
        let request = read_request(&mut stop)?;
        assert!(request.starts_with(b"POST /api/session/ses_000000000001/interrupt "));
        let body = request
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .ok_or("stop header")?
            + 4;
        let dto: serde_json::Value = serde_json::from_slice(&request[body..])?;
        assert_ne!(
            dto.get("stopProcesses"),
            Some(&serde_json::Value::Bool(true))
        );
        admitted.send(())?;
        let mut list = accept(&listener)?;
        let request = read_request(&mut list)?;
        assert!(
            request.starts_with(b"GET /api/session ") || request.starts_with(b"GET /api/session?")
        );
        respond(
            &mut list,
            200,
            &grapt_client::wire::encode_sessions(Vec::new())?,
        )?;
        released.recv_timeout(Duration::from_secs(2))?;
        respond(
            &mut stop,
            400,
            br#"{"_tag":"InvalidRequestError","message":"Session stop refused by fixture"}"#,
        )
    });
    let (wake, ready) = mpsc::sync_channel(1);
    let (reply, result) = mpsc::sync_channel(1);
    let mut stops = Stops::default();
    let notice = stops.dispatch(
        &mut client,
        Command::InterruptSession(session.clone()),
        Some(reply),
        &wake,
    );
    assert!(notice.contains("Stopping"));
    admission.recv_timeout(Duration::from_secs(2))?;
    assert!(stops.pending_for(Some(&session)));
    assert!(stops.advance().is_none());
    assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
    client.refresh_sessions()?;
    release.send(())?;
    ready.recv_timeout(Duration::from_secs(2))?;
    let notice = stops.advance().ok_or("stop completion was lost")?;
    assert!(notice.contains("Session stop refused by fixture"));
    assert!(result.recv_timeout(Duration::from_secs(2))?.is_err());
    assert!(!stops.pending_for(Some(&session)));
    server.join().map_err(|_| "stop fixture panicked")??;
    Ok(())
}

#[test]
fn quit_after_a_resume_stop_requires_a_separate_non_resuming_confirmation() -> Result {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let rpc = HttpConnection::attach(
        listener.local_addr()?,
        BasicCredential::new("fixture", "local-only")?,
        Bounds::PRODUCTION,
    );
    let mut client = Client::new(Bridge::test_rpc(rpc), Bounds::PRODUCTION);
    let session = session_id("ses_000000000001")?;
    let (admitted, admission) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || -> Result {
        for resume in [true, false] {
            let mut socket = accept(&listener)?;
            let request = read_request(&mut socket)?;
            assert!(request.starts_with(b"POST /api/session/ses_000000000001/interrupt "));
            let body = request
                .windows(4)
                .position(|bytes| bytes == b"\r\n\r\n")
                .ok_or("stop header")?
                + 4;
            let dto: serde_json::Value = serde_json::from_slice(&request[body..])?;
            assert_eq!(
                dto.get("resumePending")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                resume
            );
            admitted.send(())?;
            released.recv_timeout(Duration::from_secs(2))?;
            if resume {
                respond(&mut socket, 204, b"")?;
            } else {
                respond(
                    &mut socket,
                    400,
                    br#"{"_tag":"InvalidRequestError","message":"Final stop refused"}"#,
                )?;
            }
        }
        Ok(())
    });
    let (wake, ready) = mpsc::sync_channel(1);
    let mut stops = Stops::default();
    stops.start(
        client.peer_mut().stop_peer()?,
        Request {
            session: session.clone(),
            resume: true,
            observed: None,
        },
        None,
        wake.clone(),
    )?;
    admission.recv_timeout(Duration::from_secs(2))?;
    let (reply, result) = mpsc::sync_channel(1);
    assert!(stops
        .dispatch(
            &mut client,
            Command::InterruptSession(session.clone()),
            Some(reply),
            &wake
        )
        .contains("Stopping"));
    assert!(stops
        .dispatch(
            &mut client,
            Command::InterruptSession(session.clone()),
            None,
            &wake
        )
        .contains("Exit stop"));
    assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
    release.send(())?;
    ready.recv_timeout(Duration::from_secs(2))?;
    stops.advance();
    admission.recv_timeout(Duration::from_secs(2))?;
    assert!(stops.pending_for(Some(&session)));
    assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
    release.send(())?;
    ready.recv_timeout(Duration::from_secs(2))?;
    stops.advance();
    assert!(
        matches!(result.recv_timeout(Duration::from_secs(2))?, Err(error) if error.contains("Final stop refused"))
    );
    server.join().map_err(|_| "stop fixture panicked")??;
    Ok(())
}

/// An interrupt pressed as a stream ends must not stop the next execution.
/// The stop names the execution the operator saw; once that one has ended
/// the service refuses, and the TUI says nothing was stopped.
#[test]
fn a_stop_aimed_at_an_execution_that_already_ended_never_reaches_the_next_one() -> Result {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let rpc = HttpConnection::attach(
        listener.local_addr()?,
        BasicCredential::new("fixture", "local-only")?,
        Bounds::PRODUCTION,
    );
    let mut client = Client::new(Bridge::test_rpc(rpc), Bounds::PRODUCTION);
    let session = session_id("ses_000000000001")?;
    let ended = ExecutionWireId::parse("exe_ended".to_owned()).ok_or("execution id refused")?;
    let server = std::thread::spawn(move || -> Result {
        let mut stop = accept(&listener)?;
        let request = read_request(&mut stop)?;
        assert!(request.starts_with(b"POST /api/session/ses_000000000001/interrupt "));
        let body = request
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .ok_or("stop header")?
            + 4;
        let dto: serde_json::Value = serde_json::from_slice(&request[body..])?;
        assert_eq!(
            dto["executionID"], "exe_ended",
            "the stop must name its execution"
        );
        assert_eq!(dto.get("resumePending"), None);
        respond(
            &mut stop,
            400,
            br#"{"_tag":"InvalidRequestError","message":"the active execution identity changed"}"#,
        )
    });
    let (wake, ready) = mpsc::sync_channel(1);
    let mut stops = Stops::default();
    let peer = client.peer_mut().stop_peer()?;
    stops.start(
        peer,
        Request {
            session: session.clone(),
            resume: false,
            observed: Some(ended),
        },
        None,
        wake,
    )?;
    ready.recv_timeout(Duration::from_secs(2))?;
    let notice = stops.advance().ok_or("stop completion was lost")?;
    assert!(
        notice.contains("had already finished; nothing was stopped"),
        "{notice}"
    );
    assert!(!stops.pending_for(Some(&session)));
    server.join().map_err(|_| "stop fixture panicked")??;
    Ok(())
}
