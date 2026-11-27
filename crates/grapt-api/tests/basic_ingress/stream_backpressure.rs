//! Socket backpressure must release a subscription without client cooperation.
use super::*;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axum::body::{Body, Bytes};
use axum::http::{header::CONTENT_TYPE, HeaderValue, Response};
use grapt_api::{MaxConnections, MaxInflightRequests, MaxInflightStreams, StreamWriteAllowance};
use hyper::body::Frame;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};

const FRAME_COUNT: usize = 4096;
/// The injected stream write allowance: long enough for the whole pre-expiry
/// setup to land inside it, short enough to keep the test quick.
const ALLOWANCE: Duration = Duration::from_secs(8);
/// How long the stalled body is observed to stay owned. Well inside the
/// injected allowance, so the retention window and the expiry budget never
/// overlap, and well past the ~216 ms the setup measured, so the window is a
/// real interval and not a formality.
const RETENTION_WINDOW: Duration = Duration::from_secs(2);
/// An outer bound on expiry, so a body that never drops fails here.
const EXPIRY_BOUND: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Progress {
    frames: AtomicUsize,
    dropped: AtomicBool,
}

struct Replay {
    progress: Arc<Progress>,
    remaining: usize,
    frame: Bytes,
}

impl Drop for Replay {
    fn drop(&mut self) {
        self.progress.dropped.store(true, Ordering::SeqCst);
    }
}

impl hyper::body::Body for Replay {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        if this.remaining == 0 {
            return Poll::Ready(None);
        }
        this.remaining -= 1;
        this.progress.frames.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Some(Ok(Frame::data(this.frame.clone()))))
    }
}

fn replay_service(limits: &ApiHostLimits, progress: Arc<Progress>) -> AuthenticatedApiService {
    let mut builder = ApiRouterBuilder::new(admitted(MaxRouterModules::try_new(4), "module bound"));
    admitted(
        builder.register(
            RouteMethod::Get,
            admitted(RoutePath::try_new("/witness"), "ordinary route"),
            authenticated_route,
        ),
        "ordinary registration",
    );
    admitted(
        builder.register(
            RouteMethod::Get,
            admitted(RoutePath::try_new("/api/event"), "stream route"),
            move || {
                let progress = Arc::clone(&progress);
                async move {
                    // A finite replay larger than TCP's buffers, with complete SSE
                    // records. No receiver or arbitrary producer can end it early.
                    let mut frame = b"data: ".to_vec();
                    frame.resize(16 * 1024 - 2, b'x');
                    frame.extend_from_slice(b"\n\n");
                    let mut response = Response::new(Body::new(Replay {
                        progress,
                        remaining: FRAME_COUNT,
                        frame: Bytes::from(frame),
                    }));
                    response
                        .headers_mut()
                        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
                    response
                }
            },
        ),
        "stream registration",
    );
    AuthenticatedApiService::new(
        builder.seal(),
        Arc::new(boundary()),
        limits.max_request_body_bytes,
    )
}

async fn open_stream(address: std::net::SocketAddr) -> (TcpStream, u16) {
    let socket = admitted(TcpSocket::new_v4(), "subscriber socket");
    admitted(
        socket.set_recv_buffer_size(1024),
        "subscriber receive window",
    );
    let mut stream = admitted(socket.connect(address).await, "subscriber connect");
    admitted(stream.write_all(b"GET /api/event HTTP/1.1\r\nHost: fixture\r\nAuthorization: Basic YWxwaGE6emVwaHly\r\nConnection: close\r\n\r\n").await, "subscriber request");
    let mut head = Vec::new();
    while head.len() < 16 * 1024 && !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8; 1];
        admitted(
            tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut byte)).await,
            "subscriber head deadline",
        )
        .unwrap_or_else(|error| panic!("subscriber head: {error}"));
        head.push(byte[0]);
    }
    let status = String::from_utf8_lossy(&head)
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("subscriber status absent"));
    (stream, status)
}

#[tokio::test]
async fn stalled_output_reclaims_the_stream_slot_while_its_client_stays_open() {
    let mut limits = limits_with(64, 8192, Duration::from_secs(5));
    limits.max_connections = admitted(MaxConnections::try_new(4), "connection bound");
    limits.max_inflight_requests = admitted(MaxInflightRequests::try_new(1), "ordinary bound");
    limits.max_inflight_streams = admitted(MaxInflightStreams::try_new(1), "stream bound");
    let progress = Arc::new(Progress::default());
    limits.stream_write_allowance =
        admitted(StreamWriteAllowance::try_new(ALLOWANCE), "write allowance");
    let host = start(replay_service(&limits, Arc::clone(&progress)), limits).await;
    let address = host.ready().observed_address();
    let started = tokio::time::Instant::now();
    let (stalled, status) = open_stream(address).await;
    assert_eq!(status, 200);
    // Observe actual backpressure, not merely a connected socket. The body
    // must stop progressing before it finishes, and remain owned by the host.
    let settling = tokio::time::Instant::now();
    let mut previous = progress.frames.load(Ordering::SeqCst);
    let mut stable = 0;
    while stable < 4 && settling.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let current = progress.frames.load(Ordering::SeqCst);
        stable = if current == previous { stable + 1 } else { 0 };
        previous = current;
    }
    assert!(
        stable == 4 && previous > 0 && previous < FRAME_COUNT,
        "fixture never blocked socket output: produced {previous} frames"
    );
    assert!(
        !progress.dropped.load(Ordering::SeqCst),
        "fixture ended before its deadline"
    );
    let (overflow, status) = open_stream(address).await;
    assert_eq!(
        status, 503,
        "stalled response did not retain its stream permit"
    );
    drop(overflow);
    let mut ordinary = RawConnection::open(address).await;
    ordinary.send(&request(VALID_AUTHORIZATION)).await;
    assert_eq!(ordinary.read_status().await, Some(200));
    drop(ordinary);
    // O3, as a WINDOW rather than one sample: through the whole retention
    // window the stalled body stays owned. One assertion at setup time would
    // let an implementation that expires far too early pass, because the
    // later wait only has an upper bound. The window is well inside the
    // injected allowance, so it cannot overlap the expiry budget.
    assert!(
        started.elapsed() < RETENTION_WINDOW,
        "the setup alone took {:?}, past the {RETENTION_WINDOW:?} retention window",
        started.elapsed()
    );
    while started.elapsed() < RETENTION_WINDOW {
        assert!(
            !progress.dropped.load(Ordering::SeqCst),
            "stalled output was dropped at {:?}, before the {ALLOWANCE:?} allowance",
            started.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Expiry is bounded separately from the retention window, so a stalled
    // body that never drops fails on this bound rather than hanging.
    let released = tokio::time::timeout(EXPIRY_BOUND, async {
        while !progress.dropped.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        released.is_ok(),
        "stalled response outlived the write allowance"
    );
    let (replacement, status) = open_stream(address).await;
    assert_eq!(
        status, 200,
        "body cleanup preceded release of the socket's permit"
    );
    // The original client remained alive, with its receive window blocked,
    // until after another subscription acquired the reclaimed permit.
    drop(stalled);
    drop(replacement);
    let _ = host.stop().await;
}
