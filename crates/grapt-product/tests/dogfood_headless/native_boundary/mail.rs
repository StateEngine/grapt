//! Disposable v2 mail peer with two controlled arrivals and joined shutdown.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) struct Mail {
    pub address: SocketAddr,
    phase: tokio::sync::watch::Sender<u8>,
    stop: tokio::sync::watch::Sender<bool>,
    delivered: Arc<AtomicUsize>,
    snapshots: Arc<AtomicUsize>,
    worker: Option<thread::JoinHandle<Result<(), String>>>,
}

impl Mail {
    pub fn start() -> Result<Self, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let (phase, changes) = tokio::sync::watch::channel(0);
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let delivered = Arc::new(AtomicUsize::new(0));
        let recorded = delivered.clone();
        let snapshots = Arc::new(AtomicUsize::new(0));
        let observed = snapshots.clone();
        let worker = thread::spawn(move || -> Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).map_err(|error| error.to_string())?;
                let mut requests = tokio::task::JoinSet::new();
                let mut count = 0;
                loop {
                    tokio::select! {
                        biased;
                        _ = stopped.changed() => break,
                        result = requests.join_next(), if !requests.is_empty() => {
                            result.ok_or("mail request disappeared")?.map_err(|error| error.to_string())??;
                        }
                        incoming = listener.accept(), if requests.len() < 8 => {
                            let (socket, _) = incoming.map_err(|error| error.to_string())?;
                            count += 1;
                            if count > 128 { return Err("mail fixture request bound".into()); }
                            let changes = changes.clone();
                            let recorded = recorded.clone();
                            let snapshots = snapshots.clone();
                            requests.spawn(async move {
                                let result = serve(socket, changes.clone(), recorded, snapshots).await;
                                if let Err(error) = &result { eprintln!("native fixture peer: {error}"); }
                                result
                            });
                        }
                    }
                }
                requests.abort_all();
                while requests.join_next().await.is_some() {}
                Ok(())
            })
        });
        Ok(Self {
            address,
            phase,
            stop,
            worker: Some(worker),
            delivered,
            snapshots: observed,
        })
    }

    pub fn arrive(&self, phase: u8) {
        self.phase.send_replace(phase);
    }

    pub fn wait_delivered(&self, wanted: usize) {
        assert!(
            permission_residuals::wait_until(
                Duration::from_secs(10),
                Duration::from_millis(10),
                || self.delivered.load(Ordering::SeqCst) >= wanted
            )
            .is_some(),
            "mail {wanted} was not durably acknowledged"
        );
    }

    pub fn quiet_boundary(&self) {
        // No third message: a later source observation proves the driver has
        // returned through admission scanning after the second delivery ACK.
        self.advise(3);
    }

    /// One advisory with no new mail: phases above two add no rows, so the
    /// driver re-observes the source and finds nothing new.
    pub fn advise(&self, phase: u8) {
        self.phase.send_replace(phase);
        assert!(
            permission_residuals::wait_until(
                Duration::from_secs(10),
                Duration::from_millis(10),
                || self.snapshots.load(Ordering::SeqCst) >= usize::from(phase)
            )
            .is_some(),
            "source observation {phase} was not seen"
        );
    }

    pub fn finish(mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.stop.send_replace(true);
        self.worker
            .take()
            .ok_or("mail worker missing")?
            .join()
            .map_err(|_| "mail worker panicked")??;
        Ok(())
    }
}

impl Drop for Mail {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(worker) = self.worker.take() {
            let joined = worker.join();
            if thread::panicking() && !matches!(&joined, Ok(Ok(()))) {
                eprintln!("native fixture worker failed during unwind: {joined:?}");
            }
            if !thread::panicking() {
                assert!(
                    joined.is_ok_and(|result| result.is_ok()),
                    "mail fixture failed during cleanup"
                );
            }
        }
    }
}

#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
async fn serve(
    mut socket: tokio::net::TcpStream,
    mut phase: tokio::sync::watch::Receiver<u8>,
    delivered: Arc<AtomicUsize>,
    snapshots: Arc<AtomicUsize>,
) -> Result<(), String> {
    let Some(request) = read(&mut socket).await? else {
        return Ok(());
    };
    let operation = request["operation"].as_str().ok_or("mail operation")?;
    let current = *phase.borrow_and_update();
    eprintln!(
        "native fixture operation={operation} phase={current} after={:?}",
        request["args"]["after"]
    );
    let value = match operation {
        "open" => {
            let profile: serde_json::Value = serde_json::from_str(include_str!(
                "../../../../../fixtures/stateengine-native/stateengine-mail-profile.json"
            ))
            .map_err(|error| error.to_string())?;
            serde_json::json!({"binding_id":"bind-native","protocol":"remote-seat-mail-transport/v2",
                "adapter_version":"stateengine-native-mail/2","projects":{"source":"native-fixture","target":"native-fixture"},
                "verified_principal":"agt_native_fixture","local_recipient":"agt_native_fixture",
                "mapping_version":"exact-seat-id/v1","takeover":{"generation":1,"displaced":null},"limits":profile["limits"],
                "capabilities":{"atomic_subscribe_and_snapshot":true,"durable_delivery_ack":true,"bulk_dismiss":true,
                    "explicit_project_pair":true,"opaque_authenticated_cursors":true,"advisory_payload_bytes":0,"coalesced_pending_signals":1}})
        }
        "subscribe_and_snapshot" => {
            serde_json::json!({"subscription_id":"sub-native","generation":1,
            "snapshot":{"snapshot_id":format!("snap-{current}"),"barrier":"barrier-native",
                "start_after":request["args"]["resume"]["from_checkpoint"],"retained_origin":"retained-origin/v1"}})
        }
        "fetch_snapshot_page" => {
            let frozen: u8 = request["args"]["snapshot"]["snapshot_id"]
                .as_str()
                .and_then(|id| id.strip_prefix("snap-"))
                .ok_or("snapshot identity")?
                .parse()
                .map_err(|_| "snapshot phase")?;
            snapshots.fetch_max(usize::from(frozen), Ordering::SeqCst);
            let after = match request["args"]["after"].as_str() {
                None if request["args"]["after"].is_null() => 0,
                Some(cursor) => cursor
                    .strip_prefix("cursor-mail-")
                    .ok_or("mail cursor prefix")?
                    .parse::<u8>()
                    .map_err(|_| "mail cursor index")?,
                None => return Err("mail cursor type".into()),
            };
            let messages: Vec<_> = (1..=frozen.min(2))
                .filter(|index| *index > after)
                .map(row)
                .collect();
            let last = messages
                .last()
                .map(|message| message["source_cursor"].clone());
            serde_json::json!({"messages":messages,"next_after":null,"refused_rows":[], "last_cursor":last})
        }
        "acknowledge_delivery" => {
            let ack = &request["args"]["ack"];
            let index: usize = ack["source_message"]
                .as_str()
                .and_then(|id| id.strip_prefix("mail-"))
                .ok_or("mail ack source")?
                .parse()
                .map_err(|_| "mail ack index")?;
            delivered.fetch_max(index, Ordering::SeqCst);
            serde_json::json!({"tag":"Recorded","receipt":{"ack_id":ack["ack_id"],"source_message":ack["source_message"],"projects":ack["projects"]},
                "primary_delivery":{"witnessed":true},"floor_projection":{"state":"unarmed","outcome":"refused","disposition":null,
                    "reason":"secondary-unarmed","outcome_record":{"state":"unreadable"},"retry_required":false,"retry":null}})
        }
        "next_change" => {
            // A phase can change between snapshot completion and opening the
            // retained poll. Do not consume that advisory merely by subscribing.
            if usize::from(current) <= snapshots.load(Ordering::SeqCst) {
                let mut byte = [0];
                tokio::select! {
                    changed = phase.changed() => { changed.map_err(|_| "mail phase closed")?; }
                    closed = socket.read(&mut byte) => {
                        if closed.map_err(|error| error.to_string())? != 0 { return Err("unexpected poll input".into()); }
                        eprintln!("native fixture cancelled poll phase={current}");
                        return Ok(());
                    }
                }
            }
            serde_json::json!({"generation":1,"token":null})
        }
        "close" => serde_json::json!({"joined_tasks":0,"ticks_used":1}),
        _ => return Err(format!("unexpected mail operation {operation}")),
    };
    let body = serde_json::to_vec(&serde_json::json!({"ok":true,"value":value}))
        .map_err(|error| error.to_string())?;
    let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
    let response = async {
        socket.write_all(head.as_bytes()).await?;
        socket.write_all(&body).await?;
        socket.shutdown().await
    }
    .await;
    match response {
        Ok(()) => Ok(()),
        // The driver can cancel a completed request when its owned binding
        // changes. Only response-side disconnects are harmless; read/framing
        // failures above must still terminate this fixture with an error.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            ) =>
        {
            eprintln!("native fixture cancelled response operation={operation}: {error}");
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

async fn read(socket: &mut tokio::net::TcpStream) -> Result<Option<serde_json::Value>, String> {
    let mut bytes = Vec::new();
    let mut length = None;
    for _ in 0..64 {
        let mut chunk = [0; 2048];
        let count = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut chunk))
            .await
            .map_err(|_| "mail request timeout")?
            .map_err(|error| error.to_string())?;
        // Cancelling an owned retained poll may close before its HTTP request
        // is written. Partial/truncated requests still fail the fixture.
        if count == 0 && bytes.is_empty() {
            return Ok(None);
        }
        if count == 0 || bytes.len() + count > 65536 {
            return Err("mail request framing".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if length.is_none() {
            if let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let head =
                    std::str::from_utf8(&bytes[..split]).map_err(|error| error.to_string())?;
                if !head.starts_with("POST /remote-seat-mail-transport/v2 HTTP/1.1\r\n")
                    || !head.lines().any(|line| {
                        line.eq_ignore_ascii_case("authorization: Bearer NATIVE_FIXTURE_CAPABILITY")
                    })
                {
                    return Err("mail fixture route or authorization mismatch".into());
                }
                let body = head
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    })
                    .ok_or("mail length")?;
                if body > 32768 {
                    return Err("mail body bound".into());
                }
                length = Some((split + 4, body));
            }
        }
        if let Some((start, count)) = length {
            if bytes.len() >= start + count {
                return serde_json::from_slice(&bytes[start..start + count])
                    .map(Some)
                    .map_err(|error| error.to_string());
            }
        }
    }
    Err("mail framing chunk bound".into())
}

fn row(index: u8) -> serde_json::Value {
    serde_json::json!({"protocol":"remote-seat-mail-transport/v1","source_binding":"bind-native",
        "projects":{"source":"native-fixture","target":"native-fixture"},"source_message":format!("mail-{index}"),
        "source_cursor":format!("cursor-mail-{index}"),"sender":"pilot","recipient":"agt_native_fixture","kind":"handoff",
        "body":format!("private fixture mail {index}"),"refs":[],"supersession":{"supersedes":null,"superseded_by":null},
        "archive":{"disposition":"active","reason":null},"source_created":"2026-10-13T15:00:00.000Z","attention":"wake"})
}
