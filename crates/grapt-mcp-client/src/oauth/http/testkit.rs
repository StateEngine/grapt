//! Bounded TLS fixtures for MCP owner witnesses. Included only in tests or
//! the explicit loopback-testkit feature; production construction uses platform trust.
use super::*;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;
#[derive(Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub authorization: bool,
    pub cookie: bool,
    pub body: Secret<Vec<u8>>,
}
pub struct Reply {
    pub status: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub delay: Duration,
}
impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reply")
            .field("status", &self.status)
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}
impl Reply {
    pub fn json(value: serde_json::Value) -> Self {
        Self {
            status: "200 OK",
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: value.to_string().into_bytes(),
            delay: Duration::ZERO,
        }
    }
}
#[derive(Debug)]
pub struct Server {
    pub base: String,
    pub seen: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<Result<(), String>>>,
}
impl Server {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn new(
        count: usize,
        handler: impl Fn(&str, &Recorded) -> Reply + Send + Sync + 'static,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if count == 0 || count > 32 {
            return Err("TLS fixture connection count is outside 1..=32".into());
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let base = format!("https://{}", listener.local_addr()?);
        let cert = CertificateDer::from(include_bytes!("fixtures/server.der").to_vec());
        let key = PrivatePkcs8KeyDer::from(include_bytes!("fixtures/server-key.der").to_vec());
        let tls = Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(vec![cert], key.into())?,
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let fixture_seen = seen.clone();
        let fixture_stop = stop.clone();
        let fixture_base = base.clone();
        let handler = Arc::new(handler);
        let thread = thread::Builder::new()
            .name("mcp-oauth-tls-fixture".to_owned())
            .spawn(move || {
                let mut workers = Vec::with_capacity(count);
                let accepted = (|| {
                    for _ in 0..count {
                        let mut accepted = None;
                        for _ in 0..2000 {
                            if fixture_stop.load(Ordering::Acquire) {
                                return Err(
                                    "TLS fixture stopped before its expected request".to_owned()
                                );
                            }
                            match listener.accept() {
                                Ok((stream, _)) => {
                                    accepted = Some(stream);
                                    break;
                                }
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                    thread::sleep(Duration::from_millis(5));
                                }
                                Err(_) => return Err("TLS fixture accept failed".to_owned()),
                            }
                        }
                        let stream = accepted.ok_or("TLS fixture request deadline")?;
                        let tls = tls.clone();
                        let handler = handler.clone();
                        let base = fixture_base.clone();
                        let seen = fixture_seen.clone();
                        let peer = thread::Builder::new()
                            .name("mcp-oauth-tls-peer".to_owned())
                            .spawn(move || {
                                // Windows accepts inherit the listener's nonblocking mode;
                                // the synchronous TLS reader needs bounded blocking I/O.
                                stream
                                    .set_nonblocking(false)
                                    .map_err(|_| "TLS fixture blocking mode")?;
                                stream
                                    .set_read_timeout(Some(Duration::from_secs(2)))
                                    .map_err(|_| "TLS fixture read timeout")?;
                                stream
                                    .set_write_timeout(Some(Duration::from_secs(2)))
                                    .map_err(|_| "TLS fixture write timeout")?;
                                let connection = rustls::ServerConnection::new(tls)
                                    .map_err(|_| "TLS fixture connection failed")?;
                                let mut stream = rustls::StreamOwned::new(connection, stream);
                                let request = read_request(&mut stream)?;
                                let reply = handler(&base, &request);
                                let header_bytes = reply
                                    .headers
                                    .iter()
                                    .map(|(name, value)| {
                                        name.len().saturating_add(value.len()).saturating_add(4)
                                    })
                                    .fold(0usize, usize::saturating_add);
                                if reply.delay > Duration::from_secs(2)
                                    || reply.body.len() > 1024 * 1024
                                    || reply.headers.len() > 128
                                    || header_bytes > 65536
                                {
                                    return Err("TLS fixture reply exceeds its bounds".to_owned());
                                }
                                seen.lock()
                                    .map_err(|_| "TLS fixture record lock")?
                                    .push(request);
                                let mut head = format!(
                                    "HTTP/1.1 {}\r\nConnection: close\r\nContent-Length: {}\r\n",
                                    reply.status,
                                    reply.body.len()
                                );
                                for (name, value) in reply.headers {
                                    head.push_str(&format!("{name}: {value}\r\n"));
                                }
                                head.push_str("\r\n");
                                stream
                                    .write_all(head.as_bytes())
                                    .map_err(|_| "TLS fixture header write failed")?;
                                stream
                                    .flush()
                                    .map_err(|_| "TLS fixture header flush failed")?;
                                if !reply.delay.is_zero() {
                                    thread::sleep(reply.delay);
                                }
                                let _ = stream.write_all(&reply.body);
                                let _ = stream.flush();
                                Ok::<(), String>(())
                            })
                            .map_err(|_| "TLS fixture peer spawn failed")?;
                        workers.push(peer);
                    }
                    Ok::<(), String>(())
                })();
                let mut result = accepted;
                for worker in workers {
                    let joined = worker
                        .join()
                        .map_err(|_| "TLS fixture peer panicked".to_owned())
                        .and_then(|value| value);
                    match joined {
                        Err(error) if result.is_ok() => result = Err(error),
                        _ => {}
                    }
                }
                result
            })?;
        Ok(Self {
            base,
            seen,
            stop,
            thread: Some(thread),
        })
    }
    pub fn finish(mut self) -> TestResult {
        self.thread
            .take()
            .ok_or("TLS fixture worker missing")?
            .join()
            .map_err(|_| "TLS fixture worker panicked")??;
        Ok(())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}
fn read_request(stream: &mut impl Read) -> Result<Recorded, String> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    let (end, length) = loop {
        let n = stream
            .read(&mut chunk)
            .map_err(|_| "TLS fixture request read failed")?;
        if n == 0 || bytes.len().saturating_add(n) > 80 * 1024 {
            return Err("TLS fixture request incomplete or oversized".into());
        }
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let head =
                std::str::from_utf8(&bytes[..end]).map_err(|_| "TLS fixture headers UTF-8")?;
            let length = head
                .split("\r\n")
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>())
                .transpose()
                .map_err(|_| "TLS fixture content length")?
                .unwrap_or(0);
            if length > 65536 {
                return Err("TLS fixture request body exceeds bound".into());
            }
            break (end + 4, length);
        }
    };
    while bytes.len() < end + length {
        let n = stream
            .read(&mut chunk)
            .map_err(|_| "TLS fixture body read failed")?;
        if n == 0 || bytes.len() + n > 80 * 1024 {
            return Err("TLS fixture request body incomplete".into());
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    let head = std::str::from_utf8(&bytes[..end]).map_err(|_| "TLS fixture request UTF-8")?;
    let mut lines = head.split("\r\n");
    let mut line = lines
        .next()
        .ok_or("TLS fixture request line")?
        .split_whitespace();
    let method = line.next().ok_or("TLS fixture method")?.to_owned();
    let path = line.next().ok_or("TLS fixture path")?.to_owned();
    let fields: Vec<_> = lines.filter_map(|line| line.split_once(':')).collect();
    Ok(Recorded {
        method,
        path,
        authorization: fields
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("authorization")),
        cookie: fields
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("cookie")),
        body: Secret::new(bytes[end..end + length].to_vec()),
    })
}
pub fn client() -> Result<MetadataHttp, Box<dyn std::error::Error>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(
        include_bytes!("fixtures/root.der").to_vec(),
    ))?;
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(MetadataHttp::with_tls_loopback(tls)?)
}
