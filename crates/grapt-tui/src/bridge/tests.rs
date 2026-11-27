use super::*;
use grapt_client::{
    client::SessionActivity, wire::Spelled, BasicCredential, Client, ContractDouble, Operation,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
type Server = std::thread::JoinHandle<std::result::Result<(), String>>;

impl Bridge {
    pub(crate) fn test_event_feed(
        rpc: HttpConnection,
        wake: mpsc::SyncSender<()>,
    ) -> (
        Self,
        impl Fn(FramePoll) -> std::result::Result<(), String> + Send,
    ) {
        let (events, frames) = mpsc::sync_channel(HANDOFF_FRAMES);
        let (reconnect, _retry) = mpsc::sync_channel(1);
        let feed = move |frame| {
            events
                .try_send(Delivery::Frame(frame))
                .map_err(|e| e.to_string())?;
            let _ = wake.try_send(());
            Ok(())
        };
        (Self::assembled(rpc, frames, reconnect), feed)
    }

    pub(crate) fn test_rpc(rpc: HttpConnection) -> Self {
        let (_events, frames) = mpsc::sync_channel(32);
        let (reconnect, _retry) = mpsc::sync_channel(1);
        Self::assembled(rpc, frames, reconnect)
    }
}

// Serve the real session-list DTOs through Bridge's RPC transport, without
// introducing a second event client or hydrating the child's transcript.
fn catalog(
    bodies: Vec<(u16, Vec<u8>)>,
) -> std::result::Result<(HttpConnection, Server), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let rpc = HttpConnection::attach(
        listener.local_addr()?,
        BasicCredential::new("fixture", "local-only")?,
        Bounds::PRODUCTION,
    );
    listener.set_nonblocking(true)?;
    let server = std::thread::spawn(move || {
        for (status, body) in bodies {
            let started = tokio::time::Instant::now();
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if started.elapsed() > Duration::from_secs(5) {
                            return Err("session-list request absent".into());
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => return Err(error.to_string()),
                }
            };
            socket
                .set_nonblocking(false)
                .map_err(|error| error.to_string())?;
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|error| error.to_string())?;
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .map_err(|error| error.to_string())?;
            let mut header = Vec::new();
            while !header.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut bytes = [0; 512];
                let count = socket.read(&mut bytes).map_err(|error| error.to_string())?;
                if count == 0 || header.len() + count > 8192 {
                    return Err("invalid session-list request".into());
                }
                header.extend_from_slice(&bytes[..count]);
            }
            if ![
                b"GET /api/session ".as_slice(),
                b"GET /api/session?".as_slice(),
                b"POST /api/session ".as_slice(),
            ]
            .iter()
            .any(|prefix| header.starts_with(prefix))
            {
                return Err("unexpected RPC during child observation".into());
            }
            if status == 0 {
                // Close after the complete request crossed the wire so the
                // client observes TransportError::AfterSend immediately
                // without sleeping past the production 5s read timeout.
                drop(socket);
                continue;
            }
            write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                .and_then(|()| socket.write_all(&body))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    });
    Ok((rpc, server))
}

#[test]
fn a_new_execution_waits_for_its_catalog_entry_without_an_unrelated_wake() -> Result {
    new_child(false)
}

#[test]
fn a_failed_catalog_read_retains_the_start_until_its_bounded_retry_succeeds() -> Result {
    new_child(true)
}

fn new_child(fail_catalog: bool) -> Result {
    let mut inner = ContractDouble::new();
    let parent = inner
        .create_session("Parent")
        .ok_or("parent session bound")?;
    let request = WireRequest::new(Operation::SessionList, None, None)?;
    let first = inner.call(&request)?.body;
    let child = inner
        .create_child_session("Held child", &parent)
        .ok_or("child session bound")?;
    let second = inner.call(&request)?.body;
    let mut replies = vec![(200, first)];
    if fail_catalog {
        replies.push((503, b"{}".to_vec()));
    }
    replies.push((200, second));
    let (rpc, server) = catalog(replies)?;
    let (events, frames) = mpsc::sync_channel(32);
    let (gap, gaps) = mpsc::sync_channel(1);
    let (reconnect, _retry) = mpsc::sync_channel(1);
    let mut bridge = Bridge::assembled(rpc, frames, reconnect);
    bridge.gaps = gaps;
    let mut client = Client::new(bridge, Bounds::PRODUCTION);
    client.refresh_sessions()?;
    assert_eq!(client.sessions().len(), 1);
    let frame = |kind: &str, seq| {
        serde_json::json!({
            "id":format!("evt_child_{seq}"), "type":kind, "created":seq,
            "durable":{"aggregateID":child.spelled(),"seq":seq,"version":1},
            "data":{"sessionID":child.spelled(),"executionID":"exe_held_child"}
        })
        .to_string()
    };
    // The server need not emit session.created: real spawn starts execution.
    let start = frame("session.execution.started", 1);
    assert!(catalog_changed(&start));
    client
        .peer_mut()
        .sessions_dirty
        .store(true, Ordering::Release);
    events.send(Delivery::Frame(FramePoll::Frame(start)))?;
    client.pump_at(Duration::ZERO)?;
    assert_eq!(client.diagnostics().frames_decoded, 0);
    assert_eq!(client.observed_activity(&child), None);
    // Use Backend's actual refresh/retry owner. No other stream event or
    // input arrives: the catalog must explicitly continue the retained start.
    let (wake, ready) = mpsc::sync_channel(1);
    let mut refresh = crate::backend::CatalogRefresh::default();
    let mut now = tokio::time::Instant::now();
    if fail_catalog {
        assert!(refresh.refresh_inline(&mut client, now, &wake).is_err());
        assert!(client.peer_mut().sessions_dirty.load(Ordering::Acquire));
        assert_eq!(refresh.wait(now), Some(Duration::from_secs(1)));
        assert!(!refresh.refresh_inline(&mut client, now, &wake)?);
        client.pump_at(Duration::ZERO)?;
        assert_eq!(client.diagnostics().frames_decoded, 0);
        assert_eq!(client.observed_activity(&child), None);
        assert!(ready.try_recv().is_err());
        now += Duration::from_secs(1);
    }
    assert!(refresh.refresh_inline(&mut client, now, &wake)?);
    assert_eq!(refresh.wait(now), None);
    assert_eq!(ready.try_recv(), Ok(()), "quiet pending work lost its wake");
    client.pump_at(Duration::ZERO)?;
    assert_eq!(
        client.observed_activity(&child),
        Some(SessionActivity::Running)
    );
    assert!(client
        .sessions()
        .iter()
        .any(|row| row.id == child && row.parent.as_ref() == Some(&parent)));
    assert!(client.view_of(&child).is_none());
    events.send(Delivery::Frame(FramePoll::Frame(frame(
        "session.execution.succeeded",
        2,
    ))))?;
    client.pump_at(Duration::ZERO)?;
    assert_eq!(
        client.observed_activity(&child),
        Some(SessionActivity::Succeeded)
    );
    assert!(client.view_of(&child).is_none());
    if fail_catalog {
        gap.send(())?;
    } else {
        events.send(Delivery::Frame(FramePoll::Closed))?;
    }
    client.pump_at(Duration::ZERO)?;
    assert_eq!(client.observed_activity(&child), None);
    assert_eq!(client.stream_gap_notice().is_some(), fail_catalog);
    server.join().map_err(|_| "catalog server panicked")??;
    Ok(())
}

#[test]
fn catalog_hints_use_the_event_type_instead_of_quoted_content() {
    for kind in [
        "session.created",
        "session.updated",
        "session.title.changed",
        "session.execution.started",
        "session.execution.succeeded",
        "session.execution.failed",
        "session.execution.interrupted",
        "session.usage.updated",
    ] {
        assert!(catalog_changed(
            &serde_json::json!({"type":kind}).to_string()
        ));
        assert!(!catalog_changed(
            &serde_json::json!({"type":"session.text.delta","data":{"delta":kind}}).to_string()
        ));
    }
    assert!(!catalog_changed(
        "session.execution.started is not an event"
    ));
}

#[test]
fn permission_hints_use_the_event_type_instead_of_quoted_content() {
    for kind in [
        "permission.asked",
        "permission.replied",
        "permission.cancelled",
    ] {
        assert!(authority_changed(
            &serde_json::json!({"type":kind,"data":{}}).to_string()
        ));
        assert!(!authority_changed(
            &serde_json::json!({"type":"session.text.delta","data":{"delta":kind}}).to_string()
        ));
    }
    // A streamed answer about permissions is text, not an authority change.
    assert!(!authority_changed(
        &serde_json::json!({"type":"session.text.delta","data":{"delta":"ask for permission first"}})
            .to_string()
    ));
}

#[test]
fn a_failed_read_recovers_only_after_the_same_path_and_scope_succeeds() -> Result {
    let (rpc, server) = catalog(vec![
        (0, b"{}".to_vec()),
        (200, b"{}".to_vec()),
        (0, b"{}".to_vec()),
        (200, b"{}".to_vec()),
    ])?;
    let (_events, frames) = mpsc::sync_channel(32);
    let (reconnect, _retry) = mpsc::sync_channel(1);
    let mut bridge = Bridge::assembled(rpc, frames, reconnect);
    let request =
        WireRequest::new(Operation::SessionList, None, None)?.with_query("directory", "/one");
    let other =
        WireRequest::new(Operation::SessionList, None, None)?.with_query("directory", "/two");
    let failed = bridge
        .call(&request)
        .err()
        .ok_or("read unexpectedly succeeded")?;
    assert!(matches!(failed, TransportError::AfterSend { .. }));
    assert!(bridge.read_health.message().is_some());
    assert_eq!(bridge.call(&other)?.status, 200);
    assert!(
        bridge.read_health.message().is_some(),
        "another workspace cleared the failure"
    );
    assert!(bridge
        .read_health
        .transient(&grapt_client::ClientError::Transport(failed)));
    let write = WireRequest::new(Operation::SessionCreate, None, None)?.with_body(b"{}".to_vec());
    let uncertain = bridge
        .call(&write)
        .err()
        .ok_or("write unexpectedly replied")?;
    assert!(matches!(uncertain, TransportError::AfterSend { .. }));
    assert!(
        !bridge
            .read_health
            .transient(&grapt_client::ClientError::Transport(uncertain)),
        "uncertain write was mistaken for a recovering read"
    );
    assert_eq!(bridge.call(&request)?.status, 200);
    assert!(
        bridge.read_health.message().is_none(),
        "successful retry left a stale notice"
    );
    server.join().map_err(|_| "RPC fixture panicked")??;
    Ok(())
}
