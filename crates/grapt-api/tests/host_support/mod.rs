//! Shared fixtures for the R4H-H integration witnesses.
//!
//! These witnesses run against the PRODUCTION credential ingress, which under
//! ruling R4HA-04 refuses every credential. That is not a limitation to work
//! around — it is what makes the contrast below meaningful. A hostile request
//! that receives `431` or `413`, or no response at all, was refused by a bound
//! that sits BEFORE the authenticated service; a well-formed one receives `401`,
//! which proves the service was reached. Asserting only "the route was not
//! invoked" would pass for the wrong reason here, because production A never
//! invokes a route at all.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use grapt_api::auth::AuthenticationPolicyVersion;
use grapt_api::directory::{MaxPrincipalMappings, SeatPrincipalDirectory};
use grapt_api::router::{ApiRouterBuilder, MaxRouterModules, RouteMethod, RoutePath};
use grapt_api::{
    ApiHostLimits, AuthenticatedApiService, HeaderReadAllowance, MaxConnections, MaxHeaders,
    MaxInflightRequests, MaxRequestBodyBytes, MaxRequestHeadBytes, MaxRequestsPerConnection,
    PreparedApiHost, RunningApiHost, SeatTrustBoundary, ShutdownDrainAllowance,
    StreamWriteAllowance, TokioDeadlineDriver,
};
use grapt_location::{
    BindHost, BindLimits, BindPort, BoundListener, ListenerBindPlan, ListenerBinder,
    LiteralAddressResolver, MaxBindAttempts, MaxResolvedAddresses,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A loopback listener on an ephemeral port, through the accepted L crate.
pub async fn loopback_listener() -> BoundListener {
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

/// Bounds with the two the hostile witnesses vary stated explicitly.
pub fn limits_with(
    max_headers: usize,
    head_bytes: usize,
    header_allowance: Duration,
) -> ApiHostLimits {
    let bound = |what: &str| -> ! { panic!("the fixture bound for {what} was refused") };
    let Ok(max_connections) = MaxConnections::try_new(4) else {
        bound("max_connections")
    };
    let Ok(max_requests_per_connection) = MaxRequestsPerConnection::try_new(8) else {
        bound("max_requests_per_connection")
    };
    let Ok(max_inflight_requests) = MaxInflightRequests::try_new(8) else {
        bound("max_inflight_requests")
    };
    let Ok(max_request_head_bytes) = MaxRequestHeadBytes::try_new(head_bytes) else {
        bound("max_request_head_bytes")
    };
    let Ok(max_headers) = MaxHeaders::try_new(max_headers) else {
        bound("max_headers")
    };
    let Ok(max_request_body_bytes) = MaxRequestBodyBytes::try_new(1024) else {
        bound("max_request_body_bytes")
    };
    let Ok(header_read_allowance) = HeaderReadAllowance::try_new(header_allowance) else {
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
        stream_write_allowance,
        max_connections,
        max_requests_per_connection,
        max_inflight_requests,
        max_inflight_streams: grapt_api::MaxInflightStreams::disabled(),
        max_request_head_bytes,
        max_headers,
        max_request_body_bytes,
        header_read_allowance,
        shutdown_drain_allowance,
    }
}

/// The one fixture route, which must never run.
///
/// A named `async fn` with a stated return type rather than a closure returning
/// `!`: a `panic!`-only closure leaves the handler's response type to never-type
/// fallback, which this edition denies. `String` is the smallest concrete type
/// that satisfies the handler bound without naming `axum` — integration tests do
/// not inherit the library's normal dependencies, and adding one to reach a type
/// this route never returns would be a dependency edge bought for nothing.
#[allow(dead_code)]
async fn unreachable_route() -> String {
    panic!(
        "a route ran under the production credential ingress; ruling R4HA-04 says \
         production A authenticates and refuses, so reaching a handler here means \
         the seal failed"
    )
}

#[allow(dead_code)]
/// The production service over a one-route sealed router.
pub fn production_service(limits: &ApiHostLimits) -> AuthenticatedApiService {
    let Ok(max_mappings) = MaxPrincipalMappings::try_new(4) else {
        panic!("a mapping bound of four must be admitted");
    };
    // `configure_empty` rather than `configure(Vec::new(), .., &authorities)`:
    // this fixture configures no subject, and the empty directory is the
    // constructor for exactly that. The clock and id doubles this used to build
    // are gone with it — not tidied away, but unreachable: with no entry to
    // issue for, there was never a question to ask either of them.
    let directory = SeatPrincipalDirectory::configure_empty(max_mappings);
    let Ok(policy) = AuthenticationPolicyVersion::try_new("auth-policy-v1") else {
        panic!("the fixture policy version must be admitted");
    };
    let boundary = Arc::new(SeatTrustBoundary::new(policy, directory));

    let Ok(max_modules) = MaxRouterModules::try_new(4) else {
        panic!("a router bound of four must be admitted");
    };
    let mut builder = ApiRouterBuilder::new(max_modules);
    let Ok(path) = RoutePath::try_new("/witness") else {
        panic!("the fixture route path must be admitted");
    };
    if let Err(refusal) = builder.register(RouteMethod::Get, path, unreachable_route) {
        panic!("the fixture route was refused: {refusal}");
    }

    AuthenticatedApiService::new(builder.seal(), boundary, limits.max_request_body_bytes)
}

#[allow(dead_code)]
/// Start a host over a fresh loopback listener.
pub async fn start(limits: ApiHostLimits) -> RunningApiHost {
    let listener = loopback_listener().await;
    let service = production_service(&limits);
    PreparedApiHost::new(
        listener,
        service,
        limits,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .unwrap_or_else(|error| panic!("fixture host limits were refused: {error}"))
    .start()
}

/// A raw HTTP/1 client: bytes on a socket, not a hyper client.
///
/// Enabling a client stack to probe a server would make the witness depend on
/// the same parser it is probing, and a hostile head is exactly what a
/// well-behaved client will not send.
pub struct RawConnection {
    stream: TcpStream,
    buffer: Vec<u8>,
}

impl RawConnection {
    pub async fn open(address: SocketAddr) -> Self {
        match Self::try_open(address).await {
            Some(connection) => connection,
            None => panic!("the witness client could not connect to {address}"),
        }
    }

    /// Connect, or report that the address refused.
    ///
    /// A refused connect is a real answer for a host that has reached its
    /// terminal: the listening socket is gone, so the kernel refuses at the
    /// handshake and there is no backlog to wait in. It is a STRONGER result
    /// than an unanswered request, and a witness that panicked on it would fail
    /// on exactly the outcome it wanted.
    pub async fn try_open(address: SocketAddr) -> Option<Self> {
        TcpStream::connect(address).await.ok().map(|stream| Self {
            stream,
            buffer: Vec::new(),
        })
    }

    pub async fn send(&mut self, bytes: &[u8]) {
        if let Err(error) = self.stream.write_all(bytes).await {
            panic!("the witness client could not write: {error}");
        }
    }

    /// The response status, or `None` if the connection closed without one.
    ///
    /// `None` is a real answer here rather than a failure: a head that never
    /// completed within its allowance is closed WITHOUT a response, and a client
    /// that treated that as an error could not tell it from a hang.
    pub async fn read_status(&mut self) -> Option<u16> {
        self.read_head().await.map(|head| parse_status(&head))
    }

    /// The bounded response head for witnesses that own header semantics.
    pub async fn read_head(&mut self) -> Option<String> {
        loop {
            if let Some(end) = find_head_end(&self.buffer) {
                return Some(String::from_utf8_lossy(&self.buffer[..end]).into_owned());
            }
            let mut chunk = [0u8; 4096];
            // BOUNDED, and `None` is reserved for its real meaning. A closed
            // connection is `None`; a connection that neither closes nor answers
            // is a HANG, and a mutation that produces one must fail this witness
            // rather than stall the battery until its step timeout. The bound is
            // deliberately longer than the 60-second wrapper the paused-clock
            // witness puts around this call, so that witness keeps deciding its
            // own outcome.
            match tokio::time::timeout(Duration::from_secs(120), self.stream.read(&mut chunk)).await
            {
                Ok(Ok(0)) => return None,
                Ok(Ok(read)) => self.buffer.extend_from_slice(&chunk[..read]),
                Ok(Err(_)) => return None,
                Err(_) => panic!(
                    "the connection neither closed nor answered; read so far: {:?}",
                    String::from_utf8_lossy(&self.buffer)
                ),
            }
        }
    }
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|start| start + 4)
}

fn parse_status(head: &str) -> u16 {
    let Some(first) = head.lines().next() else {
        panic!("an empty response head");
    };
    let mut parts = first.split_whitespace();
    let _version = parts.next();
    let Some(code) = parts.next() else {
        panic!("a response head with no status code: {first:?}");
    };
    match code.parse::<u16>() {
        Ok(status) => status,
        Err(error) => panic!("a response head with a non-numeric status {code:?}: {error}"),
    }
}
