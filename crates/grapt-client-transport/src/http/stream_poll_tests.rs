//! An idle subscription must yield to input before the server sends a frame.

use super::*;
use std::net::TcpListener;
use std::sync::mpsc::sync_channel;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn oversized_stream_reports_loss_and_retires_before_parsing_its_suffix() -> TestResult {
    for dedicated in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let server = std::thread::spawn(move || -> TestResult {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            let mut request = Vec::new();
            for _ in 0..8_192 {
                let mut byte = [0];
                stream.read_exact(&mut byte)?;
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            assert!(request.ends_with(b"\r\n\r\n"));
            let body = format!(
                "data: {}\n\ndata: suffix must not become an event\n\n",
                // Past the positioned bound this subscription reads with.
                "x".repeat(grapt_api::u3a::MAX_POSITIONED_FRAME_BYTES)
            );
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")?;
            stream.write_all(body.as_bytes())?;
            Ok(())
        });
        let mut connection = HttpConnection::attach(
            address,
            BasicCredential::new("test", "test")?,
            Bounds::PRODUCTION,
        );
        connection.resubscribe();
        let result = (|| -> TestResult {
            for _ in 0..2_000 {
                let frame = if dedicated {
                    connection.wait_frame()?
                } else {
                    connection.poll_frame()?
                };
                if frame != FramePoll::Idle {
                    assert_eq!(frame, FramePoll::Gap);
                    assert!(!connection.subscribed());
                    assert_eq!(connection.poll_frame()?, FramePoll::Closed);
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err("oversized stream never reported its loss".into())
        })();
        server.join().map_err(|_| "stream server panicked")??;
        result?;
    }
    Ok(())
}

#[test]
fn idle_stream_yields_without_waiting_for_data_and_keeps_later_frames() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let (release, released) = sync_channel(1);
    let (idle, observed_idle) = sync_channel(1);
    let server = std::thread::spawn(move || -> TestResult {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut request = Vec::new();
        for _ in 0..8_192 {
            let mut byte = [0];
            stream.read_exact(&mut byte)?;
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        assert!(
            request.ends_with(b"\r\n\r\n"),
            "subscription request exceeded its bound"
        );
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")?;
        released.recv_timeout(Duration::from_secs(5))?;
        stream.write_all(b"data: {\"type\":\"later\"}\n\n")?;
        Ok(())
    });
    let worker = std::thread::spawn(move || -> TestResult {
        let mut connection = HttpConnection::attach(
            address,
            BasicCredential::new("test", "test")?,
            Bounds::PRODUCTION,
        );
        connection.resubscribe();
        // A blocking subscription would now wait for the server rather than
        // reporting Idle. Read timeouts cannot make this test accidentally pass.
        connection
            .stream
            .as_ref()
            .ok_or("subscription absent")?
            .set_read_timeout(Some(Duration::from_secs(30)))?;
        let first = connection.poll_frame()?;
        idle.send(matches!(first, FramePoll::Idle))?;
        for _ in 0..1_000 {
            match connection.poll_frame()? {
                FramePoll::Frame(payload) | FramePoll::Positioned { data: payload, .. } => {
                    assert_eq!(payload, "{\"type\":\"later\"}");
                    return Ok(());
                }
                FramePoll::Closed | FramePoll::Gap => return Err("later frame was lost".into()),
                FramePoll::Idle => std::thread::sleep(Duration::from_millis(1)),
            }
        }
        Err("later frame exceeded its bounded arrival window".into())
    });
    // Always release and join the server, including when the old blocking
    // implementation fails to yield. No detached socket worker survives a red.
    let yielded = observed_idle.recv_timeout(Duration::from_secs(1));
    let released = release.send(());
    let server_result = server.join().map_err(|_| "server worker panicked")?;
    let worker_result = worker.join().map_err(|_| "client worker panicked")?;
    assert!(
        matches!(yielded, Ok(true)),
        "idle stream waited for server data: {yielded:?}"
    );
    released?;
    server_result?;
    worker_result
}

#[test]
fn dedicated_reader_sleeps_then_yields_to_public_poll_and_wakes_on_close() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let (frame, send_frame) = sync_channel(1);
    let (close, close_stream) = sync_channel(1);
    let server = std::thread::spawn(move || -> TestResult {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut request = Vec::new();
        for _ in 0..8_192 {
            let mut byte = [0];
            stream.read_exact(&mut byte)?;
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        assert!(request.ends_with(b"\r\n\r\n"));
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")?;
        send_frame.recv_timeout(Duration::from_secs(5))?;
        stream.write_all(b"data: later\n\n")?;
        close_stream.recv_timeout(Duration::from_secs(5))?;
        Ok(())
    });
    let mut connection = HttpConnection::attach(
        address,
        BasicCredential::new("test", "test")?,
        Bounds::PRODUCTION,
    );
    connection.resubscribe();
    let stream = connection.stream.as_ref().ok_or("subscription absent")?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    #[cfg(not(windows))]
    let probe = stream.try_clone()?;
    let (entered, entry) = sync_channel(1);
    #[cfg(not(windows))]
    let (observed, observation) = sync_channel(1);
    #[cfg(not(windows))]
    let observer = std::thread::spawn(move || -> TestResult {
        // Clearing the prior timeout marks entry into wait_frame. The mode
        // switch must precede this marker; peek never consumes its SSE bytes.
        for _ in 0..5_000 {
            if probe.read_timeout()?.is_none() {
                entered.send(())?;
                let mut byte = [0];
                let waited = match probe.peek(&mut byte) {
                    Ok(_) => true,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                    Err(error) => return Err(error.into()),
                };
                observed.send(waited)?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Err("dedicated reader did not enter its wait".into())
    });
    let (idle, polled) = sync_channel(1);
    let reader = std::thread::spawn(move || -> TestResult {
        // Windows duplicates retain their own SO_RCVTIMEO, so the observer
        // cannot use a clone's timeout as an entry marker. Check the public
        // quiet wait there; Unix also observes the socket's blocking mode.
        #[cfg(windows)]
        entered.send(())?;
        assert_eq!(connection.wait_frame()?, FramePoll::Frame("later".into()));
        // The peer remains open and quiet here. A leaked blocking mode would
        // keep this public poll stuck until the cleanup closes that peer.
        idle.send(connection.poll_frame()?)?;
        assert_eq!(connection.wait_frame()?, FramePoll::Closed);
        Ok(())
    });
    let entry_result = entry.recv_timeout(Duration::from_secs(5));
    #[cfg(not(windows))]
    let early_probe = observation.recv_timeout(Duration::from_millis(100));
    #[cfg(windows)]
    let early_probe = polled.recv_timeout(Duration::from_millis(100));
    let released = frame.send(());
    let public_poll = polled.recv_timeout(Duration::from_secs(1));
    let closed = close.send(());
    // Always close and join before judging the negative case: neither the old
    // busy reader nor a leaked blocking public poll survives a red test.
    let server_result = server.join();
    let reader_result = reader.join();
    #[cfg(not(windows))]
    let observer_result = observer.join();
    entry_result?;
    released?;
    closed?;
    server_result.map_err(|_| "server panicked")??;
    reader_result.map_err(|_| "reader panicked")??;
    #[cfg(not(windows))]
    observer_result.map_err(|_| "probe panicked")??;
    assert!(
        matches!(early_probe, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
        "quiet dedicated read yielded before a frame: {early_probe:?}"
    );
    assert_eq!(
        public_poll?,
        FramePoll::Idle,
        "public poll must still yield"
    );
    Ok(())
}
