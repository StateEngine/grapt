//! Real HTTP consumers subscribe with positions and refuse a service that
//! predates them.
use grapt_client::{
    BasicCredential, Bounds, Connection, FramePoll, HttpConnection, TransportError,
};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const POSITIONED: &str =
    "id: epoch-1\ndata: {\"id\":\"evt_connected\",\"type\":\"server.connected\",\"data\":{}}\n\n";
type Worker = std::thread::JoinHandle<Result<Vec<String>, String>>;
struct Peer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<Worker>,
}
fn read_request(stream: &mut TcpStream) -> Result<String, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).map_err(|e| e.to_string())?;
        bytes.push(byte[0]);
        if bytes.len() > 8192 {
            return Err("fixture header bound".into());
        }
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}
impl Peer {
    fn start(replies: Vec<(u16, &'static str)>) -> TestResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let worker = std::thread::spawn(move || -> Result<Vec<String>, String> {
            let mut replies = VecDeque::from(replies);
            let mut streams = Vec::new();
            let mut requests = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = read_request(&mut stream)?;
                        requests.push(request.lines().next().ok_or("request line")?.to_owned());
                        let (status, body) =
                            replies.pop_front().ok_or("unexpected subscribe replay")?;
                        if status == 200 {
                            write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}"
                            )
                            .map_err(|e| e.to_string())?;
                            streams.push(stream);
                        } else {
                            write!(stream, "HTTP/1.1 {status} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if tokio::time::Instant::now() >= deadline {
                            return Err("fixture deadline".into());
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
            if !replies.is_empty() {
                return Err("expected subscribe was never made".into());
            }
            Ok(requests)
        });
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }
    fn connection(&self) -> TestResult<HttpConnection> {
        Ok(HttpConnection::attach(
            self.address,
            BasicCredential::new("fixture", "DUMMY_AUTH")?,
            Bounds::PRODUCTION,
        ))
    }
    fn finish(mut self) -> TestResult<Vec<String>> {
        self.stop.store(true, Ordering::Release);
        Ok(self
            .worker
            .take()
            .ok_or("fixture worker")?
            .join()
            .map_err(|_| "fixture worker panicked")??)
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn subscribe(connection: &mut HttpConnection, incremental: bool) -> Result<(), TransportError> {
    if !incremental {
        connection.resubscribe();
        return if connection.subscribed() {
            Ok(())
        } else {
            match connection.poll_frame() {
                Err(error) => Err(error),
                _ => Err(TransportError::NotConnected),
            }
        };
    }
    let mut result = connection.begin_reconnect()?;
    for _ in 0..5000 {
        match result {
            Some(true) => return Ok(()),
            Some(false) => return Err(TransportError::NotConnected),
            None => {
                std::thread::sleep(Duration::from_millis(1));
                result = connection.poll_reconnect()?;
            }
        }
    }
    Err(TransportError::NotConnected)
}
fn frame(connection: &mut HttpConnection) -> TestResult<FramePoll> {
    for _ in 0..5000 {
        let frame = connection.poll_frame()?;
        if frame != FramePoll::Idle {
            return Ok(frame);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Err("stream frame deadline".into())
}
#[test]
fn both_builders_subscribe_with_positions() -> TestResult {
    for incremental in [false, true] {
        let peer = Peer::start(vec![(200, POSITIONED)])?;
        let mut connection = peer.connection()?;
        subscribe(&mut connection, incremental)?;
        assert!(matches!(
            frame(&mut connection)?,
            FramePoll::Positioned { .. }
        ));
        let requests = peer.finish()?;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("positions=1"));
    }
    Ok(())
}
#[test]
fn both_builders_refuse_a_400_for_positions_as_a_server_too_old() -> TestResult {
    for incremental in [false, true] {
        let peer = Peer::start(vec![(400, "")])?;
        let mut connection = peer.connection()?;
        let refusal = subscribe(&mut connection, incremental);
        assert!(
            matches!(refusal, Err(TransportError::ServerTooOld { .. })),
            "{refusal:?}"
        );
        if let Err(refusal) = refusal {
            assert!(refusal.to_string().contains("Upgrade the Grapt server"));
        }
        let requests = peer.finish()?;
        assert_eq!(requests.len(), 1, "no subscribe without positions");
        assert!(requests[0].contains("positions=1"));
    }
    Ok(())
}
#[test]
fn both_builders_report_401_and_do_not_retry_other_refusals() -> TestResult {
    for incremental in [false, true] {
        let peer = Peer::start(vec![(401, "")])?;
        let mut connection = peer.connection()?;
        assert!(matches!(
            subscribe(&mut connection, incremental),
            Err(TransportError::Status { status: 401 })
        ));
        assert_eq!(
            connection.poll_frame()?,
            FramePoll::Closed,
            "status must be consumed once"
        );
        assert_eq!(peer.finish()?.len(), 1);
    }
    Ok(())
}
