//! The real SSE decoder must drive the production admission inactivity signal.
use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

struct Wire {
    base: grapt_provider::route::LoopbackBase,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Wire {
    fn start(normalized: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .unwrap_or_else(|error| panic!("wire listener: {error}"));
        listener
            .set_nonblocking(true)
            .unwrap_or_else(|error| panic!("wire listener bound: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("wire address: {error}"));
        let base = grapt_provider::route::LoopbackBase::parse(&format!("http://{address}"))
            .unwrap_or_else(|error| panic!("wire base: {error}"));
        let worker = std::thread::spawn(move || {
            let mut socket = (0..400)
                .find_map(|_| match listener.accept() {
                    Ok((socket, _)) => Some(socket),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        None
                    }
                    Err(error) => panic!("wire accept: {error}"),
                })
                .unwrap_or_else(|| panic!("wire request did not arrive"));
            socket
                .set_nonblocking(false)
                .unwrap_or_else(|error| panic!("wire blocking mode: {error}"));
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap_or_else(|error| panic!("wire read bound: {error}"));
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap_or_else(|error| panic!("wire write bound: {error}"));
            let mut request = Vec::new();
            let mut buffer = [0_u8; 2048];
            loop {
                let n = socket
                    .read(&mut buffer)
                    .unwrap_or_else(|error| panic!("wire read: {error}"));
                assert!(
                    n > 0 && request.len() + n <= 16_384,
                    "missing or oversized fixture request"
                );
                request.extend_from_slice(&buffer[..n]);
                let head = String::from_utf8_lossy(&request);
                if let Some(end) = head.find("\r\n\r\n") {
                    let length = head[..end]
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .unwrap_or_else(|| panic!("fixture request length missing"));
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let payload = if normalized {
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"progress\"}\n\n"
            } else {
                ": keepalive\n\ndata: {\"type\":\"fixture.unknown\"}\n\n"
            };
            let terminal = "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n";
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len() * 10 + terminal.len())
                .unwrap_or_else(|error| panic!("wire response head: {error}"));
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(100));
                socket
                    .write_all(payload.as_bytes())
                    .unwrap_or_else(|error| panic!("wire progress: {error}"));
            }
            socket
                .write_all(terminal.as_bytes())
                .unwrap_or_else(|error| panic!("wire terminal: {error}"));
        });
        Self {
            base,
            worker: Some(worker),
        }
    }
}
impl Drop for Wire {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            assert!(worker.join().is_ok(), "wire worker failed");
        }
    }
}

async fn exercise(normalized: bool) {
    let wire = Wire::start(normalized);
    let opener = Arc::new(
        AttemptOpener::loopback_testkit(
            grapt_provider::request::ProtocolFamily::ChatGptCodex,
            CodexDeadlines::new(Duration::from_secs(2), Duration::from_secs(2))
                .unwrap_or_else(|error| panic!("wire deadlines: {error}")),
            grapt_provider::CodexLimits::new(
                std::num::NonZeroUsize::new(1).unwrap_or_else(|| panic!("wire capacity")),
            ),
            wire.base.clone(),
            route(),
            credential(),
        )
        .unwrap_or_else(|error| panic!("wire opener: {error:?}")),
    );
    let authority = authority_of(issue(&ModelRegistry::install(catalog("wire-capacity"))));
    let request = ProviderRequest::ChatGptCodex(request_for(&authority));
    let opened = opener
        .open(attempt("pat_wire_holder"), authority, request)
        .unwrap_or_else(|error| panic!("wire open: {error:?}"));
    let (_custody, mut reader) = opened.into_parts();
    let holder = async {
        let mut terminal = false;
        while let Some(fragment) = reader.next_fragment().await {
            assert!(
                !matches!(
                    fragment,
                    grapt_provider::AttemptFragment::ProviderFailure { .. }
                ),
                "wire holder failed: {fragment:?}"
            );
            terminal |= matches!(
                fragment,
                grapt_provider::AttemptFragment::ProviderTerminal { .. }
            );
        }
        assert!(terminal, "wire holder never completed");
    };
    let mut waiter = harness(
        "ses_capacity_wire",
        opener,
        Box::new(RegistryPlan {
            registry: ModelRegistry::install(catalog("wire-waiter")),
        }),
    );
    let progress = waiter.source.capacity_progress();
    let wait = crate::step::capacity::until_stalled(
        waiter.source.reserve_capacity(),
        progress,
        Duration::from_millis(500),
    );
    let ((), result) = tokio::join!(holder, wait);
    assert_eq!(
        result.is_some(),
        normalized,
        "decoded output must extend admission; comments and unknown frames must not"
    );
    assert_eq!(waiter.source.capacity_error.is_none(), normalized);
}

#[tokio::test]
async fn decoded_provider_progress_extends_capacity_wait_until_acquisition() {
    exercise(true).await;
}

#[tokio::test]
async fn comments_and_unknown_provider_frames_cannot_extend_capacity_wait() {
    exercise(false).await;
}
