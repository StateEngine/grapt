//! Measure actual TLS handshakes under old/new idle settings.
//! The certificate and key are public test-only material for exact loopback.
use super::*;
use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;

const CERT: &[u8] = include_bytes!("../../../tests/fixtures/tls/loopback-test-only.der");
const KEY: &[u8] = include_bytes!("../../../tests/fixtures/tls/loopback-test-only-key.pk8");
const STEPS: usize = 5;
struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Result<usize, String>>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Server {
    fn start() -> Result<Self, String> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?
            .with_no_client_auth()
            .with_single_cert(
                vec![rustls::pki_types::CertificateDer::from(CERT.to_vec())],
                rustls::pki_types::PrivateKeyDer::Pkcs8(
                    rustls::pki_types::PrivatePkcs8KeyDer::from(KEY.to_vec()),
                ),
            )
            .map_err(|error| error.to_string())?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let url = format!(
            "https://{}/",
            listener.local_addr().map_err(|error| error.to_string())?
        );
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            let config = Arc::new(config);
            let mut handshakes = 0usize;
            for _ in 0..5000 {
                if stopped.load(Ordering::SeqCst) {
                    return Ok(handshakes);
                }
                let socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => return Err(error.to_string()),
                };
                socket
                    .set_nonblocking(false)
                    .map_err(|error| error.to_string())?;
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .map_err(|error| error.to_string())?;
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .map_err(|error| error.to_string())?;
                let connection = rustls::ServerConnection::new(Arc::clone(&config))
                    .map_err(|error| error.to_string())?;
                let mut stream = rustls::StreamOwned::new(connection, socket);
                while stream.conn.is_handshaking() {
                    stream
                        .conn
                        .complete_io(&mut stream.sock)
                        .map_err(|error| error.to_string())?;
                }
                handshakes += 1;
                for _ in 0..STEPS {
                    let mut head = Vec::new();
                    let mut byte = [0_u8; 1];
                    while head.len() < 65_536 && !head.ends_with(b"\r\n\r\n") {
                        match Read::read(&mut stream, &mut byte) {
                            Ok(1) => head.push(byte[0]),
                            _ => break,
                        }
                    }
                    if !head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                    Write::write_all(
                        &mut stream,
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok",
                    )
                    .map_err(|error| error.to_string())?;
                    Write::flush(&mut stream).map_err(|error| error.to_string())?;
                }
            }
            Err("TLS fixture did not finish within its bounded accept loop".into())
        });
        Ok(Self {
            url,
            stop,
            worker: Some(worker),
        })
    }
    fn finish(mut self) -> Result<usize, String> {
        self.stop.store(true, Ordering::SeqCst);
        self.worker
            .take()
            .ok_or("TLS worker missing")?
            .join()
            .map_err(|_| "TLS worker panicked")?
    }
}
fn client(old: bool) -> Result<reqwest::Client, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(CERT.to_vec()))
        .map_err(|error| error.to_string())?;
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| error.to_string())?
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = crate::transport::client::tls_config()
        .map_err(|error| error.to_string())?
        .alpn_protocols;
    let builder = crate::transport::client::base_client_builder(deadlines())
        .use_preconfigured_tls(tls)
        .https_only(true)
        .timeout(Duration::from_secs(5));
    let builder = if old {
        builder.pool_max_idle_per_host(0)
    } else {
        builder
    };
    builder.build().map_err(|error| error.to_string())
}
#[tokio::test]
async fn five_steps_use_five_tls_handshakes_before_pooling_and_one_after() -> Result<(), String> {
    let mut counts = Vec::new();
    for old in [true, false] {
        let server = Server::start()?;
        let client = client(old)?;
        for _ in 0..STEPS {
            let body = client
                .get(&server.url)
                .bearer_auth("public-loopback-fixture-token")
                .send()
                .await
                .map_err(|error| error.to_string())?
                .bytes()
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(body.as_ref(), b"ok");
        }
        drop(client);
        counts.push(server.finish()?);
    }
    eprintln!(
        "SC_CACHE_01_TLS steps={STEPS} before_handshakes={} after_handshakes={}",
        counts[0], counts[1]
    );
    assert_eq!(counts, [STEPS, 1]);
    Ok(())
}
