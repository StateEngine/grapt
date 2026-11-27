use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;
use std::cell::Cell;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::Request as AxumRequest;
use axum::http::StatusCode;
use axum::response::Response;
use grapt_location::{
    BindHost, BindLimits, BindPort, BoundListener, ListenerBindPlan, ListenerBinder,
    LiteralAddressResolver, MaxBindAttempts, MaxResolvedAddresses,
};
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, IdAuthority, IdToken, SeatRecipientId, Timestamp,
};
use hyper::body::{Frame, SizeHint};
use tokio::sync::oneshot;

use crate::auth::{AuthenticationPolicyVersion, SeatTrustBoundary, SubjectId};
use crate::deadline::DeadlineDriver;
use crate::directory::{MaxPrincipalMappings, SeatDirectoryEntry, SeatPrincipalDirectory};
use crate::host::{PreparedApiHost, RunningApiHost};
use crate::http1::{AuthenticatedApiService, SealedPrincipal};
use crate::limits::{
    ApiHostLimits, HeaderReadAllowance, MaxConnections, MaxHeaders, MaxInflightRequests,
    MaxRequestBodyBytes, MaxRequestHeadBytes, MaxRequestsPerConnection, ShutdownDrainAllowance,
    StreamWriteAllowance,
};
use crate::router::{ApiRouterBuilder, MaxRouterModules, RouteMethod, RoutePath};

// ---------------------------------------------------------------------------
// Injected authorities. H2: nothing below reads an ambient clock or an ambient
// randomness source.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

#[derive(Debug)]
struct CountingIds {
    next: Cell<u64>,
}

impl IdAuthority for CountingIds {
    fn mint(&self) -> IdToken {
        let ordinal = self.next.get();
        self.next.set(ordinal.saturating_add(1));
        match IdToken::new(format!("cli-{ordinal}")) {
            Some(token) => token,
            None => panic!("the counting id authority produced an empty token"),
        }
    }
}

// ---------------------------------------------------------------------------
// Fixture builders. Each panics with a diagnostic rather than returning an
// `Err`: an `Err` from a helper leaves the harness no message to attribute a
// failure to.
// ---------------------------------------------------------------------------

fn subject(raw: &str) -> SubjectId {
    match SubjectId::try_new(raw) {
        Ok(value) => value,
        Err(refusal) => panic!("fixture subject {raw:?} was refused: {refusal}"),
    }
}

fn recipient(raw: &str) -> SeatRecipientId {
    match SeatRecipientId::try_new(raw) {
        Ok(value) => value,
        Err(error) => panic!("fixture recipient {raw:?} was refused: {error:?}"),
    }
}

/// `alpha` owns `seat-alpha`, `beta` owns `seat-beta`.
///
/// Two seats and not one, because the spoof witness needs a REAL second
/// recipient to name: a forged identity that does not exist could be refused for
/// being unknown rather than for being caller-authored.
pub(super) fn two_seat_boundary() -> Arc<SeatTrustBoundary> {
    let clock = FixedClock(1_700_000_000_000);
    let ids = CountingIds { next: Cell::new(0) };
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let Ok(max) = MaxPrincipalMappings::try_new(8) else {
        panic!("a mapping bound of eight must be admitted");
    };
    let directory = match SeatPrincipalDirectory::configure(
        vec![
            SeatDirectoryEntry::new(subject("alpha"), recipient("seat-alpha")),
            SeatDirectoryEntry::new(subject("beta"), recipient("seat-beta")),
        ],
        max,
        &authorities,
    ) {
        Ok(directory) => directory,
        Err(refusal) => panic!("the fixture directory was refused: {refusal}"),
    };
    let Ok(policy) = AuthenticationPolicyVersion::try_new("auth-policy-v1") else {
        panic!("the fixture policy version must be admitted");
    };
    Arc::new(SeatTrustBoundary::new(policy, directory))
}

/// A loopback listener on an ephemeral port, through the accepted L crate.
///
/// The PRODUCTION resolver, not a fake. A literal address reaches no resolver at
/// all — that is a stated property of the accepted crate — so these witnesses
/// acquire real sockets without acquiring a dependency on name resolution being
/// installed, configured or reachable.
pub(super) async fn loopback_listener() -> BoundListener {
    let (Ok(addresses), Ok(attempts)) = (MaxResolvedAddresses::new(4), MaxBindAttempts::new(4))
    else {
        panic!("the fixture bind limits must be admitted");
    };
    let plan = ListenerBindPlan::new(
        BindHost::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        BindPort::Ephemeral,
        BindLimits::new(addresses, attempts),
    );
    let binder = ListenerBinder::new(Arc::new(LiteralAddressResolver));
    match binder.bind(&plan).await {
        Ok(listener) => listener,
        Err(error) => panic!("the fixture listener could not bind: {error}"),
    }
}

/// The bounds every witness starts from; each one overrides what it measures.
pub(super) fn limits_of(
    connections: usize,
    requests_per_connection: u32,
    inflight: usize,
    body_bytes: usize,
) -> ApiHostLimits {
    let bound = |what: &str| -> ! { panic!("the fixture bound for {what} was refused") };
    let Ok(max_connections) = MaxConnections::try_new(connections) else {
        bound("max_connections")
    };
    let Ok(max_requests_per_connection) =
        MaxRequestsPerConnection::try_new(requests_per_connection)
    else {
        bound("max_requests_per_connection")
    };
    let Ok(max_inflight_requests) = MaxInflightRequests::try_new(inflight) else {
        bound("max_inflight_requests")
    };
    let Ok(max_request_head_bytes) = MaxRequestHeadBytes::try_new(8192) else {
        bound("max_request_head_bytes")
    };
    let Ok(max_headers) = MaxHeaders::try_new(16) else {
        bound("max_headers")
    };
    let Ok(max_request_body_bytes) = MaxRequestBodyBytes::try_new(body_bytes) else {
        bound("max_request_body_bytes")
    };
    let Ok(header_read_allowance) = HeaderReadAllowance::try_new(Duration::from_secs(5)) else {
        bound("header_read_allowance")
    };
    let Ok(shutdown_drain_allowance) = ShutdownDrainAllowance::try_new(Duration::from_secs(30))
    else {
        bound("shutdown_drain_allowance")
    };
    let Ok(stream_write_allowance) = StreamWriteAllowance::try_new(Duration::from_secs(30)) else {
        bound("stream_write_allowance")
    };
    ApiHostLimits {
        max_connections,
        max_requests_per_connection,
        max_inflight_requests,
        max_inflight_streams: crate::MaxInflightStreams::disabled(),
        max_request_head_bytes,
        max_headers,
        max_request_body_bytes,
        header_read_allowance,
        shutdown_drain_allowance,
        stream_write_allowance,
    }
}

// ---------------------------------------------------------------------------
// The fixture routes.
// ---------------------------------------------------------------------------

/// What the routes recorded, so a witness can assert on invocation COUNT and on
/// what the principal actually was.
#[derive(Debug, Default)]
pub(super) struct RouteLog {
    invocations: AtomicUsize,
    seen_recipients: Mutex<Vec<String>>,
    body_bytes_seen: AtomicUsize,
    holds: Mutex<Vec<oneshot::Sender<()>>>,
}

impl RouteLog {
    pub(super) fn body_bytes_seen(&self) -> usize {
        self.body_bytes_seen.load(Ordering::SeqCst)
    }

    pub(super) fn invocations(&self) -> usize {
        self.invocations.load(Ordering::SeqCst)
    }

    fn record(&self, recipient: &str) {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        match self.seen_recipients.lock() {
            Ok(mut seen) => seen.push(recipient.to_owned()),
            Err(poisoned) => panic!("the route log mutex was poisoned: {poisoned}"),
        }
    }

    pub(super) fn recipients(&self) -> Vec<String> {
        match self.seen_recipients.lock() {
            Ok(seen) => seen.clone(),
            Err(poisoned) => panic!("the route log mutex was poisoned: {poisoned}"),
        }
    }

    /// Release every held response body.
    pub(super) fn release_all(&self) {
        match self.holds.lock() {
            Ok(mut holds) => {
                for hold in holds.drain(..) {
                    let _ = hold.send(());
                }
            }
            Err(poisoned) => panic!("the hold mutex was poisoned: {poisoned}"),
        }
    }

    fn push_hold(&self, hold: oneshot::Sender<()>) {
        match self.holds.lock() {
            Ok(mut holds) => holds.push(hold),
            Err(poisoned) => panic!("the hold mutex was poisoned: {poisoned}"),
        }
    }
}

/// A response body that emits one frame and then waits to be released.
///
/// This is what makes "the in-flight permit is held through the response BODY"
/// measurable: a body that ended immediately would release its permit before any
/// witness could look.
struct HeldBody {
    sent: bool,
    release: Option<oneshot::Receiver<()>>,
}

impl hyper::body::Body for HeldBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if !this.sent {
            this.sent = true;
            return Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"held")))));
        }
        let Some(release) = this.release.as_mut() else {
            return Poll::Ready(None);
        };
        match Pin::new(release).poll(context) {
            // A dropped sender ends the body too: a witness that forgets to
            // release must not hang the harness.
            Poll::Ready(_) => {
                this.release = None;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        false
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

/// The principal a route sees, or a diagnostic panic.
///
/// A route that could not find the sealed principal is a broken seam, not a
/// route with an anonymous caller: the service does not invoke a route at all
/// without one.
fn seen_recipient(request: &AxumRequest) -> String {
    match request.extensions().get::<SealedPrincipal>() {
        Some(sealed) => sealed.principal().recipient().as_str().to_owned(),
        None => panic!(
            "a route was invoked with no sealed principal in its extensions; the \
             authenticated service must never call a route it did not authenticate"
        ),
    }
}

/// Compose the fixture route set behind A's sealed router.
pub(super) fn fixture_service(
    log: Arc<RouteLog>,
    boundary: Arc<SeatTrustBoundary>,
    limits: &ApiHostLimits,
    hold_bodies: bool,
) -> AuthenticatedApiService {
    let Ok(max) = MaxRouterModules::try_new(8) else {
        panic!("a router bound of eight must be admitted");
    };
    let mut builder = ApiRouterBuilder::new(max);

    let echo_log = Arc::clone(&log);
    let echo = move |request: AxumRequest| {
        let log = Arc::clone(&echo_log);
        async move {
            let recipient = seen_recipient(&request);
            log.record(&recipient);
            if hold_bodies {
                let (sender, receiver) = oneshot::channel();
                log.push_hold(sender);
                let body = Body::new(HeldBody {
                    sent: false,
                    release: Some(receiver),
                });
                let mut response = Response::new(body);
                *response.status_mut() = StatusCode::OK;
                response
            } else {
                let mut response = Response::new(Body::from(recipient));
                *response.status_mut() = StatusCode::OK;
                response
            }
        }
    };
    let Ok(path) = RoutePath::try_new("/witness") else {
        panic!("the fixture route path must be admitted");
    };
    if let Err(refusal) = builder.register(RouteMethod::Get, path, echo) {
        panic!("the fixture route was refused: {refusal}");
    }

    // A route that CONSUMES the body, which is the only shape that can observe
    // the streaming body bound. A route that ignored the body would prove
    // nothing about how many bytes it could have seen.
    let sink_log = Arc::clone(&log);
    let sink_limit = limits.max_request_body_bytes.value();
    let sink = move |request: AxumRequest| {
        let log = Arc::clone(&sink_log);
        async move {
            let recipient = seen_recipient(&request);
            log.record(&recipient);
            let body = request.into_body();
            match axum::body::to_bytes(body, sink_limit).await {
                Ok(bytes) => {
                    log.body_bytes_seen.fetch_add(bytes.len(), Ordering::SeqCst);
                    let mut response = Response::new(Body::from(format!("read {}", bytes.len())));
                    *response.status_mut() = StatusCode::OK;
                    response
                }
                Err(_) => {
                    // The body-limit error maps to 413 HERE, in the consuming
                    // route, because that is where an over-limit streaming body
                    // is discovered. The corpus does not claim the route was
                    // never entered.
                    let mut response = Response::new(Body::empty());
                    *response.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
                    response
                }
            }
        }
    };
    let Ok(sink_path) = RoutePath::try_new("/sink") else {
        panic!("the sink route path must be admitted");
    };
    if let Err(refusal) = builder.register(RouteMethod::Post, sink_path, sink) {
        panic!("the sink route was refused: {refusal}");
    }

    AuthenticatedApiService::fixture(builder.seal(), boundary, limits.max_request_body_bytes)
}

/// Start a host over a fresh loopback listener.
pub(super) async fn start_host(
    log: Arc<RouteLog>,
    limits: ApiHostLimits,
    hold_bodies: bool,
    driver: Arc<dyn DeadlineDriver>,
) -> RunningApiHost {
    let listener = loopback_listener().await;
    let service = fixture_service(log, two_seat_boundary(), &limits, hold_bodies);
    PreparedApiHost::new(listener, service, limits, driver)
        .unwrap_or_else(|error| panic!("fixture host limits were refused: {error}"))
        .start()
}
