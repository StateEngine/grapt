//! Independent ordinary-request and HTTP-stream admission budgets.
//!
//! Law source: the bounded-everything row of the sync/async constitution (master spec
//! L328, "backpressure is bounded and visible") and the HTTP host contract —
//! "request permit through response-body EOF/drop; immediate overload".
//!
//! # The permit is released by the BODY, not by the head
//!
//! This is the bound that is easiest to get wrong and hardest to notice. A
//! handler returns response headers long before its body has been written, and
//! releasing the in-flight permit at that moment lets an unbounded number of
//! streaming bodies stay live while the counter claims one request is in
//! flight. The permit is therefore moved into [`PermitBody`], which releases it
//! at end of stream or when the body is dropped — whichever happens first, so a
//! client that disconnects mid-body does not leak a permit either.
//! Event stream permits instead move to their socket owner through teardown,
//! leaving ordinary request slots available while a stream stays open.
//!
//! # Overload refuses, it does not queue
//!
//! Global admission is `try_acquire_owned`, never `acquire_owned`. An awaiting
//! acquisition would turn a bound into a queue: the caller would see latency
//! instead of a refusal, the queue itself would be unbounded, and the host would
//! have no bounded inventory of what it had promised to do. Immediate refusal is
//! the whole point of the number.
//!
//! Body-size refusals remain owned by the existing request-body layer.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::convert::Infallible;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{header, Request, Response, StatusCode};
use hyper::body::{Frame, Incoming, SizeHint};
use hyper::service::Service;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::limits::{MaxInflightRequests, MaxInflightStreams, MaxRequestsPerConnection};
use crate::stream::{HeartbeatBody, StreamConnection};

/// Why a request was refused before it reached the authenticated service.
///
/// Stream saturation names its own budget and closes that refused connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestRefusal {
    /// This connection has already spent its whole monotonic request budget.
    PerConnectionBudgetExhausted,
    /// Every in-flight request permit is held by a live response body.
    GlobalConcurrencyExhausted,
    /// Every independent HTTP event-stream slot is held.
    StreamConcurrencyExhausted,
}

impl RequestRefusal {
    /// The status a refused request receives.
    ///
    /// All are `503`: the host is declining work it is bounded from doing, not
    /// judging the request. A `429` would name a rate policy this lane does not
    /// own and does not measure.
    fn status(self) -> StatusCode {
        StatusCode::SERVICE_UNAVAILABLE
    }

    /// Whether the connection must close after this refusal.
    ///
    /// A connection that has spent its budget will refuse every
    /// subsequent request forever, so leaving it open invites a client to spin
    /// on a socket that can never serve it again. Global overload is transient
    /// by construction — a permit returns when some body ends — so that
    /// connection stays usable. A refused stream closes so it cannot retain a
    /// socket reserved for ordinary calls.
    fn closes_connection(self) -> bool {
        matches!(
            self,
            RequestRefusal::PerConnectionBudgetExhausted
                | RequestRefusal::StreamConcurrencyExhausted
        )
    }
}

impl fmt::Display for RequestRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestRefusal::PerConnectionBudgetExhausted => {
                f.write_str("this connection has spent its request budget")
            }
            RequestRefusal::GlobalConcurrencyExhausted => {
                f.write_str("the host is at its in-flight request bound")
            }
            RequestRefusal::StreamConcurrencyExhausted => {
                f.write_str("event stream capacity is busy; retry the subscription")
            }
        }
    }
}

/// The monotonic request budget of ONE keep-alive connection.
///
/// Monotonic: spending is never returned. A budget that refilled would make
/// "requests per connection" a rate rather than a bound, and a client could hold
/// one socket open indefinitely.
#[derive(Debug)]
pub(crate) struct RequestBudget {
    max: u32,
    spent: AtomicU32,
}

impl RequestBudget {
    pub(crate) fn new(max: MaxRequestsPerConnection) -> Self {
        Self {
            max: max.value(),
            spent: AtomicU32::new(0),
        }
    }

    /// Spend one unit, or report the budget exhausted.
    ///
    /// `fetch_update` rather than a load followed by a store. One connection is
    /// driven by one task today, so the compare-and-set is uncontended — but a
    /// bound that is only correct because of how it happens to be called is a
    /// bound waiting for the call site to change.
    fn admit(&self) -> bool {
        self.spent
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |spent| {
                if spent < self.max {
                    Some(spent + 1)
                } else {
                    None
                }
            })
            .is_ok()
    }
}

/// Exact counts for the terminal receipt.
///
/// Every increment is checked. A receipt that silently wrapped or saturated
/// would still be arithmetically consistent and would be false, which is worse
/// than a receipt that refuses: [`HostCounters::overflowed`] is what the host
/// reads to fail typed instead of reporting a number it no longer believes.
#[derive(Debug, Default)]
pub(crate) struct HostCounters {
    admitted_requests: AtomicU64,
    refused_requests: AtomicU64,
    admitted_connections: AtomicU64,
    completed_connections: AtomicU64,
    failed_connections: AtomicU64,
    overflowed: AtomicBool,
}

impl HostCounters {
    fn bump(&self, counter: &AtomicU64) {
        let updated = counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
            value.checked_add(1)
        });
        if updated.is_err() {
            self.overflowed.store(true, Ordering::SeqCst);
        }
    }

    pub(crate) fn admit_request(&self) {
        self.bump(&self.admitted_requests);
    }

    pub(crate) fn refuse_request(&self) {
        self.bump(&self.refused_requests);
    }

    pub(crate) fn admit_connection(&self) {
        self.bump(&self.admitted_connections);
    }

    pub(crate) fn complete_connection(&self) {
        self.bump(&self.completed_connections);
    }

    pub(crate) fn fail_connection(&self) {
        self.bump(&self.failed_connections);
    }

    pub(crate) fn admitted_requests(&self) -> u64 {
        self.admitted_requests.load(Ordering::SeqCst)
    }

    pub(crate) fn refused_requests(&self) -> u64 {
        self.refused_requests.load(Ordering::SeqCst)
    }

    pub(crate) fn admitted_connections(&self) -> u64 {
        self.admitted_connections.load(Ordering::SeqCst)
    }

    pub(crate) fn completed_connections(&self) -> u64 {
        self.completed_connections.load(Ordering::SeqCst)
    }

    pub(crate) fn failed_connections(&self) -> u64 {
        self.failed_connections.load(Ordering::SeqCst)
    }

    /// Whether any counter reached the `u64` ceiling.
    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed.load(Ordering::SeqCst)
    }
}

/// The global in-flight request bound.
#[derive(Debug, Clone)]
pub(crate) struct RequestPermits {
    permits: Arc<Semaphore>,
    stream_capacity: Option<usize>,
}

impl RequestPermits {
    pub(crate) fn streams(max: MaxInflightStreams) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max.value())),
            stream_capacity: Some(max.value()),
        }
    }
    /// The bound is already checked against `Semaphore::MAX_PERMITS` by
    /// [`MaxInflightRequests::try_new`], which is why this cannot panic.
    pub(crate) fn new(max: MaxInflightRequests) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max.value())),
            stream_capacity: None,
        }
    }

    /// Take a permit, or report overload immediately.
    fn try_admit(&self) -> Option<OwnedSemaphorePermit> {
        let permit = Arc::clone(&self.permits).try_acquire_owned().ok()?;
        if let Some(capacity) = self.stream_capacity {
            let occupied = capacity.saturating_sub(self.available());
            // Integer comparison avoids overflow and warns strictly above 75%.
            if occupied > capacity - capacity.div_ceil(4) {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: HTTP event stream occupancy exceeds 75% occupied={occupied} capacity={capacity}; close unused subscriptions or adjust max_inflight_streams in service.conf and restart the service"
                );
            }
        }
        Some(permit)
    }

    /// How many permits are not currently held, for pressure and live inventory.
    pub(crate) fn available(&self) -> usize {
        self.permits.available_permits()
    }
}

/// A response body that owns an in-flight permit until it ends.
///
/// The permit is released on end of stream or on drop. Both paths matter: a
/// body that finishes normally must not hold its permit until the connection
/// closes, and a client that vanishes mid-body must not hold one forever.
pub struct PermitBody {
    inner: Body,
    permit: Option<OwnedSemaphorePermit>,
}

impl PermitBody {
    fn new(inner: Body, permit: Option<OwnedSemaphorePermit>) -> Self {
        Self { inner, permit }
    }
}

impl fmt::Debug for PermitBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PermitBody")
            .field("holds_permit", &self.permit.is_some())
            .finish()
    }
}

impl hyper::body::Body for PermitBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_frame(context);
        // End of stream is the release point. `Poll::Ready(Some(Err(..)))` is
        // NOT: an errored body has stopped producing frames but the connection
        // owner has not finished with it, and releasing early here would let a
        // replacement request in while this one is still being torn down.
        if matches!(polled, Poll::Ready(None)) {
            this.permit = None;
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// The outermost request-facing service: bounds first, then everything else.
///
/// It wraps the authenticated service rather than being wrapped by it, because
/// the counts must include requests that are refused for authentication and for
/// body size. A bound applied after authentication would measure the wrong
/// population and would let a hostile client spend the host's capacity on
/// requests that never reach a route.
pub struct AdmissionService<S> {
    inner: S,
    permits: RequestPermits,
    budget: Arc<RequestBudget>,
    counters: Arc<HostCounters>,
    peer: Option<TransportPeer>,
    streams: Option<(RequestPermits, Arc<StreamConnection>)>,
}

/// Transport-observed address, never derived from forwarded HTTP headers.
#[derive(Debug, Clone, Copy)]
pub struct TransportPeer(std::net::SocketAddr);

impl TransportPeer {
    pub fn is_loopback(self) -> bool {
        self.0.ip().is_loopback()
    }

    #[cfg(test)]
    pub(crate) fn fixture(address: std::net::SocketAddr) -> Self {
        Self(address)
    }
}

impl<S> AdmissionService<S> {
    pub(crate) fn new(
        inner: S,
        permits: RequestPermits,
        budget: Arc<RequestBudget>,
        counters: Arc<HostCounters>,
    ) -> Self {
        Self {
            inner,
            permits,
            budget,
            counters,
            streams: None,
            peer: None,
        }
    }

    pub(crate) fn with_peer(mut self, peer: std::net::SocketAddr) -> Self {
        self.peer = Some(TransportPeer(peer));
        self
    }

    pub(crate) fn with_streams(
        mut self,
        permits: RequestPermits,
        connection: Arc<StreamConnection>,
    ) -> Self {
        self.streams = Some((permits, connection));
        self
    }
}

impl<S> fmt::Debug for AdmissionService<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdmissionService")
            .field("budget", &self.budget)
            .finish()
    }
}

/// The refusal response. Named so the two call sites cannot drift.
fn refusal_response(refusal: RequestRefusal) -> Response<PermitBody> {
    let mut response = Response::new(PermitBody::new(Body::empty(), None));
    *response.status_mut() = refusal.status();
    if refusal == RequestRefusal::StreamConcurrencyExhausted {
        *response.body_mut() = PermitBody::new(
            Body::from("event stream capacity is busy; retry the subscription"),
            None,
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, header::HeaderValue::from_static("1"));
    }
    if refusal.closes_connection() {
        // A `HeaderValue` from a `&'static str` that is known-valid ASCII. The
        // fallible parse is still matched rather than unwrapped, because the
        // workspace denies `expect_used` and a header this crate authored is not
        // a reason to make an exception.
        if let Ok(value) = header::HeaderValue::from_str("close") {
            response.headers_mut().insert(header::CONNECTION, value);
        }
    }
    response
}

fn admitted_response<B>(
    mut response: Response<B>,
    permit: OwnedSemaphorePermit,
    stream: Option<Arc<StreamConnection>>,
) -> Response<PermitBody>
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<axum::BoxError>,
{
    let is_sse = response.status().is_success()
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value == "text/event-stream");
    if let Some(connection) = stream.filter(|_| is_sse) {
        if let Err(error) = connection.activate(permit) {
            let mut refused = Response::new(PermitBody::new(Body::from(error.to_string()), None));
            *refused.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
            refused.headers_mut().insert(
                header::CONNECTION,
                header::HeaderValue::from_static("close"),
            );
            return refused;
        }
        // An ended stream closes its HTTP/1 socket after flushing. The socket
        // owner, rather than the response EOF, returns the stream capacity.
        response.headers_mut().insert(
            header::CONNECTION,
            header::HeaderValue::from_static("close"),
        );
        return response.map(|body| {
            PermitBody::new(
                Body::new(HeartbeatBody::new(
                    Body::new(body),
                    connection.driver.clone(),
                )),
                None,
            )
        });
    }
    response.map(|body| PermitBody::new(Body::new(body), Some(permit)))
}

impl<S, B> Service<Request<Incoming>> for AdmissionService<S>
where
    S: Service<Request<Incoming>, Response = Response<B>, Error = Infallible> + Send + Sync,
    S::Future: Send + 'static,
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<axum::BoxError>,
{
    type Response = Response<PermitBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, mut request: Request<Incoming>) -> Self::Future {
        // Overwrite a caller's in-process extension as well as ignoring headers.
        request.extensions_mut().remove::<TransportPeer>();
        if let Some(peer) = self.peer {
            request.extensions_mut().insert(peer);
        }
        // Budget first. It is the cheaper question and the one whose refusal
        // ends the connection, so asking it second would spend a global permit
        // on a request that was already doomed.
        if !self.budget.admit() {
            self.counters.refuse_request();
            return Box::pin(core::future::ready(Ok(refusal_response(
                RequestRefusal::PerConnectionBudgetExhausted,
            ))));
        }
        let stream = self
            .streams
            .as_ref()
            .filter(|_| crate::stream::is_event_stream(request.method(), request.uri().path()));
        let permits = stream.map_or(&self.permits, |(permits, _)| permits);
        let Some(permit) = permits.try_admit() else {
            self.counters.refuse_request();
            return Box::pin(core::future::ready(Ok(refusal_response(
                if stream.is_some() {
                    RequestRefusal::StreamConcurrencyExhausted
                } else {
                    RequestRefusal::GlobalConcurrencyExhausted
                },
            ))));
        };
        self.counters.admit_request();
        let stream = stream.map(|(_, connection)| Arc::clone(connection));
        let future = self.inner.call(request);
        Box::pin(async move {
            let response = future.await?;
            Ok(admitted_response(response, permit, stream))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    fn budget_of(max: u32) -> RequestBudget {
        match MaxRequestsPerConnection::try_new(max) {
            Ok(bound) => RequestBudget::new(bound),
            Err(error) => panic!("fixture budget {max} was refused: {error}"),
        }
    }

    #[test]
    fn a_connection_budget_admits_exactly_its_bound_and_never_refills() {
        let budget = budget_of(3);
        for attempt in 1..=3 {
            assert!(budget.admit(), "request {attempt} of 3 must be admitted");
        }
        assert!(!budget.admit(), "request 4 of 3 must be refused");
        // Monotonic: nothing returns a spent unit, so the refusal is permanent
        // for this connection. A budget that refilled would be a rate.
        assert!(!budget.admit(), "a spent budget must stay spent");
    }

    #[tokio::test]
    async fn global_admission_refuses_immediately_rather_than_queueing() {
        let Ok(bound) = MaxInflightRequests::try_new(1) else {
            panic!("a bound of one must be admitted");
        };
        let permits = RequestPermits::new(bound);
        let Some(held) = permits.try_admit() else {
            panic!("the first request must be admitted");
        };

        // The second attempt must ANSWER, not wait. Racing it against a timer
        // states that positively: an awaiting acquisition would still be pending
        // when the timeout elapsed.
        let refused =
            tokio::time::timeout(Duration::from_millis(50), async { permits.try_admit() }).await;
        match refused {
            Ok(None) => {}
            Ok(Some(_)) => panic!("the bound admitted a second concurrent request"),
            Err(_) => panic!("admission waited instead of refusing; overload must never queue"),
        }

        drop(held);
        assert!(
            permits.try_admit().is_some(),
            "releasing a permit must admit later work"
        );
    }

    #[test]
    fn the_budget_refusal_closes_the_connection_and_overload_does_not() {
        let budget = refusal_response(RequestRefusal::PerConnectionBudgetExhausted);
        assert_eq!(budget.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            budget
                .headers()
                .get(header::CONNECTION)
                .map(|v| v.as_bytes()),
            Some(&b"close"[..]),
            "a connection that can never serve again must be told to close"
        );

        let overload = refusal_response(RequestRefusal::GlobalConcurrencyExhausted);
        assert_eq!(overload.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            overload.headers().get(header::CONNECTION).is_none(),
            "transient overload must leave a usable connection open"
        );
    }

    #[test]
    fn counters_report_overflow_rather_than_wrapping() {
        let counters = HostCounters::default();
        counters.admitted_requests.store(u64::MAX, Ordering::SeqCst);
        assert!(!counters.overflowed(), "nothing has overflowed yet");

        counters.admit_request();
        assert!(
            counters.overflowed(),
            "an increment past the ceiling must be reported, not wrapped"
        );
        assert_eq!(
            counters.admitted_requests(),
            u64::MAX,
            "the counter must hold its last true value rather than wrap to zero"
        );
    }
}
