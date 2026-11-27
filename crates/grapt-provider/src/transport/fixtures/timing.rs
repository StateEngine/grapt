//! Separate header/body delays on the real transport, with an injected clock.
use super::*;
use std::sync::atomic::AtomicI64;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};

#[derive(Debug)]
struct Clock(AtomicI64);

impl grapt_types::ClockAuthority for Clock {
    fn now(&self) -> grapt_types::Timestamp {
        grapt_types::Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

struct TimedFixture {
    base: String,
    body: SyncSender<()>,
    finish: SyncSender<()>,
    server: tokio::task::JoinHandle<Result<(), String>>,
}

async fn phase_ready(receiver: Receiver<()>) -> Result<bool, String> {
    // Yield on the single-thread test runtime without adding Tokio features.
    // A one-slot channel and the server's deadline bound every phase wait.
    for _ in 0..1000 {
        match receiver.try_recv() {
            Ok(()) => return Ok(true),
            Err(TryRecvError::Disconnected) => return Ok(false),
            Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(1)).await,
        }
    }
    Err("timing phase gate deadline".into())
}

async fn serve_timed(clock: Arc<Clock>, empty: bool) -> Result<TimedFixture, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let base = format!(
        "http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let (body, body_ready) = sync_channel(1);
    let (finish, finish_ready) = sync_channel(1);
    let server = tokio::spawn(async move {
        // A missing client, malformed request or missing phase is a loud,
        // bounded fixture failure. No detached listener survives this witness.
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (mut socket, _) = listener.accept().await.map_err(|e| e.to_string())?;
            let mut raw = Vec::new();
            let mut buffer = [0_u8; 2048];
            loop {
                let n = socket.read(&mut buffer).await.map_err(|e| e.to_string())?;
                if n == 0 || raw.len() + n > 65536 {
                    return Err("timing fixture request absent or excessive".to_owned());
                }
                raw.extend_from_slice(&buffer[..n]);
                let text = std::str::from_utf8(&raw).map_err(|e| e.to_string())?;
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text[..split].lines().find_map(|line| {
                        line.to_lowercase().strip_prefix("content-length: ")
                            .and_then(|s| s.parse::<usize>().ok())
                    }).ok_or("request content length absent")?;
                    if raw.len() >= split + 4 + length { break; }
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
            clock.0.store(2000, Ordering::SeqCst);
            socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n").await.map_err(|e| e.to_string())?;
            socket.flush().await.map_err(|e| e.to_string())?;
            // The client must observe the head before this phase advances the
            // injected clock; delays alone would race on a busy test host.
            if !phase_ready(body_ready).await? { return Ok(()); }
            tokio::time::sleep(Duration::from_millis(10)).await;
            clock.0.store(5000, Ordering::SeqCst);
            if !empty {
                let chunk = sse(&[r#"{"type":"response.output_text.delta","delta":"ok"}"#]);
                socket.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await.map_err(|e| e.to_string())?;
                socket.write_all(&chunk).await.map_err(|e| e.to_string())?;
                socket.write_all(b"\r\n").await.map_err(|e| e.to_string())?;
                socket.flush().await.map_err(|e| e.to_string())?;
                if !phase_ready(finish_ready).await? {
                    return Err("first body was not consumed".into());
                }
                clock.0.store(9000, Ordering::SeqCst);
                let chunk = sse(&[r#"{"type":"response.completed","response":{}}"#]);
                socket.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await.map_err(|e| e.to_string())?;
                socket.write_all(&chunk).await.map_err(|e| e.to_string())?;
                socket.write_all(b"\r\n").await.map_err(|e| e.to_string())?;
            }
            socket.write_all(b"0\r\n\r\n").await.map_err(|e| e.to_string())?;
            socket.flush().await.map_err(|e| e.to_string())?;
            Ok(())
        }).await.map_err(|_| "timing fixture deadline".to_owned())?
    });
    Ok(TimedFixture {
        base,
        body,
        finish,
        server,
    })
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
async fn witness(empty: bool, cancel: bool) -> Result<(), String> {
    let clock = Arc::new(Clock(AtomicI64::new(1000)));
    let (observer, samples) = crate::DispatchObserver::bounded(clock.clone());
    let fixture = serve_timed(clock, empty).await?;
    let adapter = adapter(2).with_dispatch(observer);
    let open = adapter
        .open(input(
            "att_timing",
            &fixture.base,
            ProtocolFamily::OpenAiResponses,
            responses_request(),
        ))
        .await
        .map_err(|e| e.to_string())?;
    assert!(matches!(
        samples.try_next(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    let published = if cancel {
        drop(open);
        drop(fixture.body);
        drop(fixture.finish);
        None
    } else {
        fixture.body.try_send(()).map_err(|_| "body gate absent")?;
        let ProviderOpen::Streaming(mut stream) = open else {
            return Err("stream absent".into());
        };
        assert!(matches!(
            stream.next_fragment().await,
            Some(AttemptFragment::AttemptOpened)
        ));
        if empty {
            drop(fixture.finish);
            assert!(matches!(
                stream.next_fragment().await,
                Some(AttemptFragment::ProviderFailure { .. })
            ));
        } else {
            let mut saw_text = false;
            for _ in 0..4 {
                if matches!(stream.next_fragment().await, Some(AttemptFragment::TextDelta { text, .. }) if text == "ok")
                {
                    saw_text = true;
                    break;
                }
            }
            assert!(saw_text, "the first chunk must produce text");
            fixture
                .finish
                .try_send(())
                .map_err(|_| "finish gate absent")?;
            let mut saw_terminal = false;
            for _ in 0..4 {
                if matches!(
                    stream.next_fragment().await,
                    Some(AttemptFragment::ProviderTerminal { .. })
                ) {
                    saw_terminal = true;
                    break;
                }
            }
            assert!(saw_terminal, "the final chunk must produce a terminal");
        }
        assert!(stream.next_fragment().await.is_none());
        // Terminal publishes while the stream still exists. Drop cannot
        // publish again or overwrite the first byte with a later chunk.
        let sample = samples.try_next().map_err(|e| e.to_string())?;
        drop(stream);
        Some(sample)
    };
    fixture.server.await.map_err(|e| e.to_string())??;
    let sample = match published {
        Some(sample) => sample,
        None => samples.try_next().map_err(|e| e.to_string())?,
    };
    assert_eq!(sample.attempt.as_str(), "att_timing");
    assert_eq!(sample.model.as_ref(), Some(responses_request().model()));
    assert_eq!(sample.at_ms, 1000);
    assert_eq!(sample.response_head_at_ms, Some(2000));
    assert_eq!(sample.http_status, Some(200));
    assert_eq!(
        sample.first_stream_byte_at_ms,
        if empty || cancel { None } else { Some(5000) }
    );
    assert!(matches!(
        samples.try_next(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    assert_eq!(samples.lost_at_least(), 0);
    Ok(())
}

#[tokio::test]
async fn delayed_head_and_body_have_distinct_ordered_stamps() -> Result<(), String> {
    witness(false, false).await
}

#[tokio::test]
async fn cancellation_after_head_keeps_first_byte_unknown() -> Result<(), String> {
    witness(false, true).await
}

#[tokio::test]
async fn empty_body_is_absent_rather_than_zero() -> Result<(), String> {
    witness(true, false).await
}
