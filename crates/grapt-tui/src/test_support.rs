//! A disposable service peer and its bridge, shared by the tests that drive them.
//!
//! The restart, replacement and gap fixtures all need the same things: a private
//! profile directory, a local HTTP peer that can rotate its credential or flood
//! its event stream, and the poll loops that settle a reconnect. Keeping one copy
//! here gives those tests one interface instead of three near-copies.
use crate::bridge::Bridge;
use crate::config::Config;
use grapt_api::{ProductHealth, PRODUCT_HEALTH_PATH};
use grapt_client::{wire::session_id, Bounds, Connection, FramePoll, TransportError};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

pub(crate) type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct Profile(pub(crate) PathBuf);
impl Profile {
    pub(crate) fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "r-restart-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
    pub(crate) fn config(&self) -> Result<Config> {
        Ok(Config {
            root: self.0.clone(),
            directory: None,
            session: Some(session_id("ses_000000000001")?),
            expected: None,
            reasoning_effort: None,
            light: false,
            notifications: true,
            preset: None,
            prompt: None,
        })
    }
    pub(crate) fn publish(&self, peer: &Peer, foreign: bool) -> Result {
        fs::write(
            self.0.join("basic.credential"),
            format!("fixture:{}\n", peer.secret),
        )?;
        fs::write(
            self.0.join("serve-ready.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema":"secode.product.ready.v2", "operation":"serve", "pid":12345,
                "build":"r-restart", "environment":"fixture", "service_instance":if foreign { "foreign" } else { peer.secret },
                "observed_endpoint":format!("http://{}", peer.address)
            }))?,
        )?;
        Ok(())
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
pub(crate) struct EventRequests {
    held: AtomicBool,
    headers: std::sync::Mutex<Vec<String>>,
    positioned: AtomicBool,
    certify: AtomicBool,
}
impl EventRequests {
    pub(crate) fn positioned(&self, certify: bool) {
        self.positioned.store(true, Ordering::Release);
        self.certify.store(certify, Ordering::Release);
    }
    pub(crate) fn headers(&self) -> std::result::Result<Vec<String>, String> {
        self.headers
            .lock()
            .map(|held| held.clone())
            .map_err(|_| "request headers lock".into())
    }
    fn opening(&self, request: &str, socket: &mut TcpStream) -> std::result::Result<(), String> {
        const HEAD: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
        if !self.positioned.load(Ordering::Acquire) {
            return socket
                .write_all(HEAD.as_bytes())
                .map_err(|error| error.to_string());
        }
        let offered = request
            .lines()
            .find_map(|line| line.strip_prefix("Last-Event-ID: "));
        let id = if self.certify.load(Ordering::Acquire) {
            offered
        } else {
            None
        }
        .unwrap_or("fixture-1");
        write!(socket, "{HEAD}id: {id}\nevent: message\ndata: {{\"id\":\"evt_connected\",\"type\":\"server.connected\",\"data\":{{}}}}\n\n")
            .or_else(|error| match error.kind() {
                // Withdrawing a stale build closes its subscription socket.
                std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted => Ok(()),
                _ => Err(error),
            })
            .map_err(|error| error.to_string())
    }

    /// The subscription request lines seen so far, oldest first.
    ///
    /// Tests assert on the order a peer received them, so the whole sequence is
    /// the interface rather than any single latest value.
    pub(crate) fn seen(&self) -> std::result::Result<Vec<String>, String> {
        self.headers()?
            .into_iter()
            .map(|request| {
                request
                    .lines()
                    .next()
                    .map(str::to_owned)
                    .ok_or_else(|| "event request line".to_owned())
            })
            .collect()
    }

    /// Hold every later subscription request until [`Self::release`].
    ///
    /// A test that needs a reconnect mid-flight parks the peer here so the
    /// ordering it asserts on is the one it caused.
    pub(crate) fn hold(&self) {
        self.held.store(true, Ordering::Release);
    }

    /// Stop holding subscription requests.
    pub(crate) fn release(&self) {
        self.held.store(false, Ordering::Release);
    }

    fn observe(&self, request: &str, stop: &AtomicBool) -> std::result::Result<(), String> {
        self.headers
            .lock()
            .map_err(|_| "request headers lock")?
            .push(request.to_owned());
        while self.held.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
}

pub(crate) struct Peer {
    pub(crate) address: std::net::SocketAddr,
    pub(crate) secret: &'static str,
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) hold: Arc<AtomicBool>,
    pub(crate) window_requests: Arc<EventRequests>,
    pub(crate) events: Arc<AtomicUsize>,
    pub(crate) event_status: Arc<AtomicUsize>,
    pub(crate) health: Arc<AtomicUsize>,
    pub(crate) calls: Arc<AtomicUsize>,
    pub(crate) worker: Option<std::thread::JoinHandle<std::result::Result<(), String>>>,
}
impl Peer {
    pub(crate) fn start(secret: &'static str) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let hold = Arc::new(AtomicBool::new(false));
        let window_requests = Arc::new(EventRequests::default());
        let observed_windows = window_requests.clone();
        let events = Arc::new(AtomicUsize::new(0));
        let event_status = Arc::new(AtomicUsize::new(200));
        let subscription_status = event_status.clone();
        let health = Arc::new(AtomicUsize::new(0));
        let health_requests = health.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let (done, held, streams, requests) =
            (stop.clone(), hold.clone(), events.clone(), calls.clone());
        let body = ProductHealth::new(12345, "r-restart", "fixture", secret)?.encoded()?;
        let auth = if secret == "old" {
            "Zml4dHVyZTpvbGQ="
        } else {
            "Zml4dHVyZTpuZXc="
        };
        let worker = std::thread::spawn(move || {
            let mut sockets: Vec<TcpStream> = Vec::new();
            while !done.load(Ordering::Acquire) {
                let mut socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(e) => return Err(e.to_string()),
                };
                socket.set_nonblocking(false).map_err(|e| e.to_string())?;
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .map_err(|e| e.to_string())?;
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .map_err(|e| e.to_string())?;
                let mut bytes = Vec::new();
                while !bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                    let mut part = [0; 2048];
                    let count = socket.read(&mut part).map_err(|e| e.to_string())?;
                    if count == 0 || bytes.len() + count > 8192 {
                        return Err("invalid fixture request".into());
                    }
                    bytes.extend_from_slice(&part[..count]);
                }
                let request = String::from_utf8(bytes).map_err(|e| e.to_string())?;
                if !request.contains(&format!("Authorization: Basic {auth}\r\n")) {
                    return Err("replacement used the old credential".into());
                }
                if request
                    .strip_prefix("GET /api/event")
                    .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('?'))
                {
                    streams.fetch_add(1, Ordering::Release);
                    if sockets.len() >= 4 {
                        return Err("unexpected subscription replay".into());
                    }
                    observed_windows.observe(&request, &done)?;
                    let status = subscription_status.load(Ordering::Acquire);
                    if status != 200 {
                        write!(socket, "HTTP/1.1 {status} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .map_err(|e| e.to_string())?;
                        continue;
                    }
                    observed_windows.opening(&request, &mut socket)?;
                    sockets.push(socket);
                } else {
                    let health = request.starts_with(&format!("GET {PRODUCT_HEALTH_PATH} "));
                    if health {
                        health_requests.fetch_add(1, Ordering::Release);
                        while held.load(Ordering::Acquire) && !done.load(Ordering::Acquire) {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    } else {
                        requests.fetch_add(1, Ordering::Release);
                    }
                    let payload = if health { body.as_str() } else { "[]" };
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len()).map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        });
        Ok(Self {
            address,
            secret,
            stop,
            hold,
            window_requests,
            events,
            event_status,
            health,
            calls,
            worker: Some(worker),
        })
    }
    pub(crate) fn close(&mut self) -> Result {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "peer panicked")??;
        }
        Ok(())
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub(crate) fn until(mut ready: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("restart fixture did not settle");
}
/// Drains the handoff until the bridge has seen its stream close: the
/// connection state changes in delivery order, never behind the queue.
pub(crate) fn closed(bridge: &mut Bridge) {
    until(|| {
        let _ = bridge.poll_frame();
        !bridge.subscribed()
    });
}
pub(crate) fn reopen(bridge: &mut Bridge) -> std::result::Result<Option<bool>, TransportError> {
    let mut result = bridge.begin_reconnect();
    until(|| {
        if !matches!(result, Ok(None)) {
            return true;
        }
        result = bridge.poll_reconnect();
        !matches!(result, Ok(None))
    });
    result
}
pub(crate) fn opened(profile: &Profile) -> Result<Bridge> {
    let (wake, _ready) = mpsc::sync_channel(1);
    Ok(Bridge::open(profile.config()?, Bounds::PRODUCTION, wake)?)
}

/// Answers health and ordinary calls, and floods every event subscription
/// faster than a never-draining handoff can hold, so the reader must report a gap.
pub(crate) struct Flood {
    pub(crate) address: std::net::SocketAddr,
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) calls: Arc<AtomicUsize>,
    pub(crate) worker: Option<std::thread::JoinHandle<()>>,
}
impl Flood {
    /// One frame larger than any frame bound: the transport must call a gap.
    pub(crate) fn start() -> Result<Self> {
        Self::serving(format!("data: {}\n\n", "x".repeat(300 * 1024)))
    }

    /// Thousands of small frames at once, more than every queue on the path.
    pub(crate) fn burst() -> Result<Self> {
        Self::serving(
            (0..2000)
                .map(|tick| format!("data: tick-{tick}\n\n"))
                .collect(),
        )
    }

    pub(crate) fn serving(stream: String) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let (done, requests) = (stop.clone(), calls.clone());
        let body = ProductHealth::new(12345, "r-restart", "fixture", "flood")?.encoded()?;
        let worker = std::thread::spawn(move || {
            let mut sockets: Vec<TcpStream> = Vec::new();
            while !done.load(Ordering::Acquire) {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                };
                let _ = socket.set_nonblocking(false);
                let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = socket.set_write_timeout(Some(Duration::from_secs(2)));
                let mut bytes = Vec::new();
                while !bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                    let mut part = [0; 2048];
                    match socket.read(&mut part) {
                        Ok(count) if count > 0 && bytes.len() < 8192 => {
                            bytes.extend_from_slice(&part[..count])
                        }
                        _ => break,
                    }
                }
                let request = String::from_utf8_lossy(&bytes).into_owned();
                if request
                    .strip_prefix("GET /api/event")
                    .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('?'))
                {
                    let mut out =
                        String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
                    out.push_str(&stream);
                    let _ = socket.write_all(out.as_bytes());
                    sockets.push(socket);
                } else {
                    let health = request.starts_with(&format!("GET {PRODUCT_HEALTH_PATH} "));
                    if !health {
                        requests.fetch_add(1, Ordering::Release);
                    }
                    let payload = if health { body.as_str() } else { "[]" };
                    let _ = write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len());
                }
            }
        });
        Ok(Self {
            address,
            stop,
            calls,
            worker: Some(worker),
        })
    }
}
impl Drop for Flood {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Profile {
    pub(crate) fn publish_flood(&self, server: &Flood, foreign: bool) -> Result {
        fs::write(self.0.join("basic.credential"), "fixture:flood\n")?;
        fs::write(
            self.0.join("serve-ready.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema":"secode.product.ready.v2", "operation":"serve", "pid":12345,
                "build":"r-restart", "environment":"fixture",
                "service_instance":if foreign { "foreign" } else { "flood" },
                "observed_endpoint":format!("http://{}", server.address)
            }))?,
        )?;
        Ok(())
    }
}
pub(crate) fn gapped(bridge: &mut Bridge) {
    until(|| matches!(bridge.poll_frame(), Ok(FramePoll::Gap)));
}
