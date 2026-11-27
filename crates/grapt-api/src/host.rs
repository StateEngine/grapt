//! The bounded HTTP/1 host: one accept owner, one bounded connection task set,
//! and one typed terminal receipt.
//!
//! Law source: the sync/async constitution's server-socket row (master spec L318, "server
//! sockets are asynchronous"), its bounded-backpressure row (L328), its `Drop` row (L325
//! — "durable finalization that requires I/O has an explicit supervised
//! shutdown/settlement path and cannot be assumed to complete inside `Drop`"), and the
//! HTTP host contract.
//!
//! # The permit is taken BEFORE the accept
//!
//! This ordering is the whole connection bound. Accepting first and then asking
//! for a permit means the socket already exists — it has been taken off the
//! backlog, it consumes a file descriptor, and the only remaining choices are to
//! hold it somewhere unbounded or to close a connection the client believes was
//! established. Acquiring first means an unavailable permit leaves the client in
//! the kernel's backlog, which is a bound the operating system is already
//! enforcing.
//!
//! The permit then lives in the connection task until that task's terminal
//! cleanup, NOT until the response is written. Releasing it earlier admits a
//! replacement while the socket it replaced is still open.
//!
//! # Stop, drain, force: three facts, never two
//!
//! Terminality here is deliberately not a boolean. Stop requested, accept
//! closed, a connection gracefully drained, a connection forcibly aborted, a
//! task joined, and the host terminal are separate facts, and the receipt says
//! which of them happened. In particular [`ApiHostExit::ForcedClose`] can never
//! be rendered as [`ApiHostExit::StoppedGracefully`]: a drain that ran out of
//! allowance cut responses that had not finished, and a receipt that called that
//! graceful would be false about the only thing anyone would consult it for.
//!
//! # `Drop` is a safety net and returns nothing
//!
//! [`RunningApiHost::stop`] is the observable path and yields the receipt.
//! `Drop` aborts the root task and produces no receipt at all, because durable
//! finalization that needs I/O may not be assumed to complete inside `Drop` —
//! the same reason `BoundListener::close` and its `Drop` are separate paths in
//! the accepted location crate.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use grapt_location::{AcceptError, AcceptedConnection, BoundListener};
use thiserror::Error;
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

use crate::admission::{AdmissionService, HostCounters, RequestBudget, RequestPermits};
use crate::deadline::{DeadlineDriver, HostTimer};
use crate::http1::{connection_builder, AuthenticatedApiService};
use crate::limits::{ApiHostConfigError, ApiHostLimits};
use crate::stream::{StreamConnection, StreamIo};

/// Why one accept failed, classified structurally.
///
/// From [`io::ErrorKind`] alone. No branch here reads an error's text: OS
/// messages are localized, versioned and platform-specific, and a string match
/// against one is a defect waiting for a different machine. The accepted
/// location crate classifies its bind and accept failures the same way, which is
/// why H can consume its typed error rather than an `io::Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AcceptFailure {
    /// The operating system refused the accept on authority grounds.
    #[error("accepting was refused: permission denied")]
    PermissionDenied,
    /// The process or system is out of the resources an accept needs.
    #[error("accepting failed: resources are exhausted")]
    ResourceExhausted,
    /// The peer went away between the backlog and the accept.
    #[error("accepting failed: the connection was aborted before it was admitted")]
    ConnectionAborted,
    /// The listener held no socket.
    #[error("the listener holds no socket and can accept nothing")]
    ListenerClosed,
    /// Any other I/O failure, carried by kind.
    #[error("accepting failed with I/O kind {kind:?}")]
    OtherIo {
        /// The classified kind. Never matched from OS text.
        kind: io::ErrorKind,
    },
}

impl AcceptFailure {
    fn classify(error: AcceptError) -> Self {
        match error {
            AcceptError::Closed { .. } => AcceptFailure::ListenerClosed,
            AcceptError::Io { kind, .. } => AcceptFailure::from_kind(kind),
        }
    }

    /// The kind-to-class mapping, as a pure function so it can be witnessed
    /// directly for kinds a witness cannot make a real socket produce.
    fn from_kind(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::PermissionDenied => AcceptFailure::PermissionDenied,
            // The two stable kinds an exhausted process reports. EMFILE has no
            // stable `ErrorKind` on this toolchain and arrives as an
            // uncategorized kind, which is exactly why the fallthrough carries
            // the kind rather than guessing at it.
            io::ErrorKind::OutOfMemory | io::ErrorKind::StorageFull => {
                AcceptFailure::ResourceExhausted
            }
            io::ErrorKind::ConnectionAborted => AcceptFailure::ConnectionAborted,
            other => AcceptFailure::OtherIo { kind: other },
        }
    }
}

/// How one connection ended badly.
///
/// Classified from `hyper::Error`'s own predicates and from [`io::ErrorKind`],
/// never from a rendered message. A source error is retained by hyper for
/// diagnosis; this vocabulary does not promote it to authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ConnectionFailureClass {
    /// The request head did not arrive within the header-read allowance.
    #[error("the request head did not complete within the header-read allowance")]
    HeaderReadTimeout,
    /// The request was not valid HTTP/1.
    #[error("the request could not be parsed as HTTP/1")]
    Parse,
    /// The peer stopped mid-message.
    #[error("the message ended before it was complete")]
    IncompleteMessage,
    /// Transport I/O failed, carried by kind.
    #[error("connection I/O failed with kind {kind:?}")]
    Io {
        /// The classified kind.
        kind: io::ErrorKind,
    },
    /// The service itself failed.
    #[error("the served service failed")]
    Service,
    /// The connection task was aborted, which is what a forced close does.
    #[error("the connection task was cancelled")]
    JoinCancelled,
    /// The connection task panicked.
    #[error("the connection task panicked")]
    JoinPanicked,
}

impl ConnectionFailureClass {
    fn classify(error: &hyper::Error) -> Self {
        // Ordered most specific first. `is_timeout` before `is_parse` because a
        // head that timed out mid-parse is a deadline fact, and reporting it as
        // a malformed request would blame the client for the host's bound.
        if error.is_timeout() {
            return ConnectionFailureClass::HeaderReadTimeout;
        }
        if error.is_parse() {
            return ConnectionFailureClass::Parse;
        }
        if error.is_incomplete_message() {
            return ConnectionFailureClass::IncompleteMessage;
        }
        if error.is_user() {
            return ConnectionFailureClass::Service;
        }
        match io_kind(error) {
            Some(kind) => ConnectionFailureClass::Io { kind },
            // A hyper error with no I/O source and none of the predicates above
            // is a transport failure whose kind this crate cannot name. It is
            // reported as such rather than folded into `Service`, which would
            // blame the route for something it never saw.
            None => ConnectionFailureClass::Io {
                kind: io::ErrorKind::Other,
            },
        }
    }
}

/// Walk an error's source chain for an `io::Error` and take its KIND.
///
/// Structural: `downcast_ref`, never a substring of a rendered message.
fn io_kind(error: &hyper::Error) -> Option<io::ErrorKind> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
    while let Some(current) = source {
        if let Some(io_error) = current.downcast_ref::<io::Error>() {
            return Some(io_error.kind());
        }
        source = std::error::Error::source(current);
    }
    None
}

/// Which phase a host failure happened in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostFailureStage {
    /// The accept loop could not continue.
    Accept,
    /// The root task did not deliver a receipt.
    Root,
    /// The host reached its terminal with owned resources still live.
    TerminalInventory,
}

/// How the root task failed to deliver its receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootLoss {
    /// The root handle had already been taken.
    ///
    /// Unreachable through the public API — [`RunningApiHost::stop`] consumes
    /// the owner, so no caller can stop a host twice — and stated as a typed
    /// outcome rather than a panic or a fabricated inventory. Its witness is the
    /// in-crate unit test below, which constructs the state directly, following
    /// the accepted location crate's handling of `AcceptError::Closed`.
    Detached,
    /// The root task was cancelled.
    Cancelled,
    /// The root task panicked.
    Panicked,
}

/// Why a host failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HostFailureCause {
    /// One accept failed and the loop is fail-fast.
    #[error("{0}")]
    Accept(AcceptFailure),
    /// A counter reached the `u64` ceiling, so no exact receipt can be issued.
    #[error("a host counter overflowed, so the receipt cannot be exact")]
    CounterOverflow,
    /// The root task ended without producing a receipt.
    ///
    /// Its own cause rather than a fabricated inventory: a task that was
    /// cancelled or panicked has told us nothing about what it still owned, and
    /// reporting a count here would be inventing one.
    #[error("the host's root task ended without a receipt: {0:?}")]
    RootTaskLost(RootLoss),
    /// Something was still owned after the terminal.
    #[error(
        "the host reached its terminal holding {connection_permits} connection permit(s) and \
         {request_permits} request permit(s) and {stream_permits} stream permit(s)"
    )]
    LiveInventory {
        /// Connection permits still held.
        connection_permits: usize,
        /// Request permits still held.
        request_permits: usize,
        /// HTTP event-stream permits still held.
        stream_permits: usize,
    },
}

/// The one truthful thing a started host claims.
///
/// The OBSERVED address, taken from the accepted location receipt, and the exact
/// limits in force. It is not a compatibility health state and names no route:
/// a host that is listening has not promised that any particular request can be
/// served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiHostReady {
    observed_address: SocketAddr,
    limits: ApiHostLimits,
}

impl ApiHostReady {
    /// The address the operating system actually assigned to the listener.
    pub fn observed_address(&self) -> SocketAddr {
        self.observed_address
    }

    /// The exact bounds this host is running under.
    pub fn limits(&self) -> &ApiHostLimits {
        &self.limits
    }
}

/// How a host ended.
///
/// The three levels never collapse. See the module note on why a forced close
/// cannot be reported as graceful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiHostExit {
    /// Every admitted connection settled before the drain allowance expired.
    StoppedGracefully {
        /// Sockets admitted over the host's life.
        admitted_connections: u64,
        /// Connections that ended without a failure class.
        completed_connections: u64,
        /// Connections that ended with a failure class.
        failed_connections: u64,
        /// Requests that took both bounds and reached the service.
        admitted_requests: u64,
        /// Requests refused by a bound before the service.
        refused_requests: u64,
    },
    /// The drain allowance expired with connections still live.
    ForcedClose {
        /// Sockets admitted over the host's life.
        admitted_connections: u64,
        /// Connections that settled during the drain, before the deadline.
        completed_before_deadline: u64,
        /// The exact number still live when the deadline fired.
        active_at_deadline: u64,
        /// How many of those were aborted AND joined. Equals
        /// `active_at_deadline` on every return, because the host drains the
        /// task set to empty before it answers.
        aborted_and_joined: u64,
    },
    /// The host could not complete its own contract.
    Failed {
        /// Which phase failed.
        stage: HostFailureStage,
        /// Why.
        cause: HostFailureCause,
        /// Connections that settled gracefully before the host gave up.
        drained_gracefully: u64,
        /// Connections that were aborted and joined.
        forced_and_joined: u64,
    },
}

/// Where a host's sockets come from.
///
/// Production is the accepted `grapt-location` listener. The trait exists so a
/// witness can present one deterministically classified accept failure without
/// making the operating system produce it — an accept-error policy that could
/// only be tested by exhausting a real machine's descriptors is a policy nobody
/// tests.
///
/// It is crate-private on purpose. It carries no authority — a socket is not an
/// identity — but a public seam here would still be a second way to give the
/// host its transport, and this lane owes exactly one.
pub(crate) trait AcceptSource: fmt::Debug + Send + Sync + 'static {
    /// Admit exactly one connection, or fail with a classified reason.
    fn accept(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<AcceptedConnection, AcceptFailure>> + Send + '_>>;

    /// The address the listener actually bound.
    fn observed_address(&self) -> SocketAddr;
}

/// The production source: the accepted single-owner location listener.
#[derive(Debug)]
pub(crate) struct ListenerAcceptSource {
    listener: BoundListener,
}

impl ListenerAcceptSource {
    pub(crate) fn new(listener: BoundListener) -> Self {
        Self { listener }
    }
}

impl AcceptSource for ListenerAcceptSource {
    fn accept(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<AcceptedConnection, AcceptFailure>> + Send + '_>> {
        Box::pin(async move {
            self.listener
                .accept()
                .await
                .map_err(AcceptFailure::classify)
        })
    }

    fn observed_address(&self) -> SocketAddr {
        self.listener.receipt().observed()
    }
}

/// A host that owns everything it needs and has not started.
///
/// Construction CONSUMES the listener. That is what makes reuse by the caller a
/// compile error rather than two owners of one socket, and it is what probe
/// `05_listener_use_after_host_transfer` witnesses.
pub struct PreparedApiHost {
    source: Box<dyn AcceptSource>,
    service: AuthenticatedApiService,
    limits: ApiHostLimits,
    driver: Arc<dyn DeadlineDriver>,
    /// `None` unless this crate's own tests asked to observe the retained set.
    retained: Option<RetainedInventoryHandle>,
}

/// A shared handle to the retained-inventory observer.
type RetainedInventoryHandle = Arc<RetainedInventory>;

impl fmt::Debug for PreparedApiHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedApiHost")
            .field("observed_address", &self.source.observed_address())
            .field("limits", &self.limits)
            .finish()
    }
}

impl PreparedApiHost {
    /// Take ownership of an accepted listener and an A-sealed service.
    ///
    /// Note the second parameter's type. It is not `axum::Router` and not a
    /// generic service: a caller cannot install a layer that invents a principal
    /// or routes around the credential ingress, because it cannot construct the
    /// thing this function requires.
    /// The connection reserve is checked before any accept task starts.
    pub fn new(
        listener: BoundListener,
        service: AuthenticatedApiService,
        limits: ApiHostLimits,
        driver: Arc<dyn DeadlineDriver>,
    ) -> Result<Self, ApiHostConfigError> {
        limits.validate()?;
        Ok(Self::with_source(
            Box::new(ListenerAcceptSource::new(listener)),
            service,
            limits,
            driver,
        ))
    }

    pub(crate) fn with_source(
        source: Box<dyn AcceptSource>,
        service: AuthenticatedApiService,
        limits: ApiHostLimits,
        driver: Arc<dyn DeadlineDriver>,
    ) -> Self {
        Self {
            source,
            service,
            limits,
            driver,
            retained: None,
        }
    }

    /// Observe the root owner's retained connection-task set.
    ///
    /// `#[cfg(test)]`, so it compiles into this crate's own library test binary
    /// and into nothing else — a dependent cannot install an observer and a
    /// production host's inventory is always `None`. It exists because R1
    /// HOLD-1's law is decided inside the root task and the terminal receipt
    /// cannot see it.
    #[cfg(test)]
    pub(crate) fn observe_retained(mut self, retained: RetainedInventoryHandle) -> Self {
        self.retained = Some(retained);
        self
    }

    /// Start accepting, and return the sole owner of the root task.
    ///
    /// There is no `Result` here, and the absence is a property rather than an
    /// omission. Every input was consumed by value and every one was already
    /// validated: the listener is bound (the location receipt proves it), the
    /// service is sealed, and each limit is a checked type. Readiness is not a
    /// separate handshake either — the socket was listening before this host
    /// existed, so a client that connects before the accept loop's first poll
    /// waits in the kernel backlog rather than being refused. A start error
    /// vocabulary would have no constructible variant, which is the same reason
    /// `ApiRefusal` carries no `RouterSealed`.
    pub fn start(self) -> RunningApiHost {
        let ready = ApiHostReady {
            observed_address: self.source.observed_address(),
            limits: self.limits,
        };
        let (stop, stop_rx) = watch::channel(false);
        let root = tokio::spawn(run(
            self.source,
            self.service,
            self.limits,
            self.driver,
            stop_rx,
            self.retained,
        ));
        RunningApiHost {
            stop,
            root: Some(root),
            ready,
        }
    }
}

/// The sole owner of a running host.
///
/// Not `Clone`, and not by convention: it owns a `JoinHandle` and a
/// `watch::Sender`, neither of which is `Clone`, and probe
/// `06_running_host_not_clone` asserts the absence through a `T: Clone` BOUND
/// rather than through a `.clone()` call — autoref makes a call on `&T` compile
/// and pass for the wrong reason.
pub struct RunningApiHost {
    stop: watch::Sender<bool>,
    root: Option<JoinHandle<ApiHostExit>>,
    ready: ApiHostReady,
}

impl fmt::Debug for RunningApiHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunningApiHost")
            .field("ready", &self.ready)
            .finish()
    }
}

impl RunningApiHost {
    /// What this host is listening on, and under which bounds.
    pub fn ready(&self) -> &ApiHostReady {
        &self.ready
    }

    /// Signal once, then join the root task and take its receipt.
    ///
    /// Consumes the owner, so there is no second stop and no way to observe a
    /// host after its terminal.
    pub async fn stop(mut self) -> ApiHostExit {
        // `send` fails only if every receiver is gone, which means the root has
        // already ended. That is not an error: the stop it would have carried
        // has already happened.
        let _ = self.stop.send(true);
        let Some(root) = self.root.take() else {
            return Self::lost(RootLoss::Detached);
        };
        match root.await {
            Ok(exit) => exit,
            Err(error) if error.is_cancelled() => Self::lost(RootLoss::Cancelled),
            Err(_) => Self::lost(RootLoss::Panicked),
        }
    }

    /// The receipt for a root task that produced none.
    ///
    /// The drained and forced counts are zero because they are UNKNOWN, and
    /// zero is the only honest number for a task that never reported: a
    /// cancelled or panicking root has told us nothing about what it still
    /// owned, and any other value here would be invented.
    fn lost(reason: RootLoss) -> ApiHostExit {
        ApiHostExit::Failed {
            stage: HostFailureStage::Root,
            cause: HostFailureCause::RootTaskLost(reason),
            drained_gracefully: 0,
            forced_and_joined: 0,
        }
    }
}

impl Drop for RunningApiHost {
    fn drop(&mut self) {
        // A backstop, and nothing more. It signals and aborts so a forgotten
        // host does not outlive its owner, and it returns no receipt, because a
        // drain that needs I/O cannot be assumed to complete here. An owner that
        // wants the receipt calls `stop`.
        let _ = self.stop.send(true);
        if let Some(root) = self.root.take() {
            root.abort();
        }
    }
}

/// One connection's terminal fact.
type ConnectionOutcome = Result<(), ConnectionFailureClass>;

/// The high-water mark of the root owner's retained connection-task set.
///
/// R1 HOLD-1. The law is that the retained set is bounded by `max_connections`
/// at all times, and it is decided inside the root task where no witness can
/// reach. The terminal receipt is the wrong instrument for it: at any population
/// a witness can drive, the drain reaps a completed backlog far faster than a
/// drain allowance can expire, so the receipt reads `StoppedGracefully` and says
/// nothing about how much was retained on the way there. Measured directly, the
/// same run showed a retained set of forty against a bound of four.
///
/// The only writer of a real value is [`PreparedApiHost::observe_retained`],
/// which is `#[cfg(test)]`, so this is `None` in every production host and no
/// dependent can install one.
#[derive(Debug, Default)]
pub(crate) struct RetainedInventory {
    high_watermark: AtomicUsize,
}

impl RetainedInventory {
    /// Record the retained count, keeping the largest ever seen.
    fn observe(&self, retained: usize) {
        self.high_watermark.fetch_max(retained, Ordering::SeqCst);
    }

    /// The largest retained set this host ever held.
    #[cfg(test)]
    pub(crate) fn high_watermark(&self) -> usize {
        self.high_watermark.load(Ordering::SeqCst)
    }
}

/// Everything a connection task and the drain both need.
///
/// A value type rather than eight parameters, and for the reason this workspace
/// has already recorded once: clippy's argument bar counts `self`, and reaching
/// for `#[allow(clippy::too_many_arguments)]` would be the first lint
/// suppression in a lane whose own architecture gate refuses them. Bundling is
/// also the more honest shape — these five are one runtime, not five unrelated
/// arguments that happen to travel together.
#[derive(Debug, Clone)]
struct HostRuntime {
    service: AuthenticatedApiService,
    limits: ApiHostLimits,
    driver: Arc<dyn DeadlineDriver>,
    requests: RequestPermits,
    streams: RequestPermits,
    counters: Arc<HostCounters>,
    /// `None` in every production host. See [`RetainedInventory`].
    retained: Option<Arc<RetainedInventory>>,
}

/// The root task: accept under a permit, spawn, reap, then drain.
async fn run(
    source: Box<dyn AcceptSource>,
    service: AuthenticatedApiService,
    limits: ApiHostLimits,
    driver: Arc<dyn DeadlineDriver>,
    mut stop: watch::Receiver<bool>,
    retained: Option<Arc<RetainedInventory>>,
) -> ApiHostExit {
    let connections = Arc::new(Semaphore::new(limits.max_connections.value()));
    let runtime = HostRuntime {
        service,
        limits,
        driver,
        requests: RequestPermits::new(limits.max_inflight_requests),
        streams: RequestPermits::streams(limits.max_inflight_streams),
        counters: Arc::new(HostCounters::default()),
        retained,
    };
    let mut tasks: JoinSet<ConnectionOutcome> = JoinSet::new();
    let mut accept_failure: Option<AcceptFailure> = None;

    loop {
        if *stop.borrow() {
            break;
        }
        // REAP UNCONDITIONALLY, before anything else can decide not to.
        //
        // R1 HOLD-1. The reap used to live only as a `select!` branch below,
        // behind an always-ready permit branch in a `biased` block — so it was
        // polled only while the connection semaphore was saturated, which is not
        // the ordinary operating state. Below saturation nothing ever joined a
        // finished task and the set retained one entry for every connection the
        // host had ever admitted: measured at forty against a bound of four.
        // The live bound still held, because the permit is released inside the
        // connection task; what grew without bound was the owned inventory, and
        // `drain` then reported that inventory as the count still LIVE at the
        // deadline.
        //
        // `try_join_next` is non-blocking, so this drains what has finished and
        // returns; every result goes through the same `record` path as any other
        // terminal, so the accounting is one path and not two. No detached
        // helper owner and no second registry: the root owner reaps its own set.
        while let Some(joined) = tasks.try_join_next() {
            record(&runtime.counters, joined);
        }
        if let Some(retained) = runtime.retained.as_ref() {
            retained.observe(tasks.len());
        }
        // THE PERMIT COMES FIRST. See the module note: asking after the accept
        // means the socket already exists and the bound has already been passed.
        let permit = tokio::select! {
            biased;
            _ = stop.changed() => break,
            acquired = Arc::clone(&connections).acquire_owned() => match acquired {
                Ok(permit) => permit,
                // The semaphore is never closed while this loop runs, so this
                // is unreachable through the public API; it is a typed exit
                // rather than an `expect`, which the workspace denies anyway.
                Err(_) => break,
            },
            Some(joined) = tasks.join_next(), if !tasks.is_empty() => {
                record(&runtime.counters, joined);
                continue;
            }
        };

        tokio::select! {
            biased;
            _ = stop.changed() => {
                // The permit is released explicitly rather than left to fall out
                // of scope, because the release is the thing being asserted: a
                // stop must not leak the capacity it was holding for a
                // connection that never arrived.
                drop(permit);
                break;
            }
            accepted = source.accept() => match accepted {
                Ok(connection) => {
                    runtime.counters.admit_connection();
                    // The ONE spawn site in this crate, and it is dominated by
                    // the permit acquired above: there is no path to this line
                    // that did not first take a connection permit.
                    tasks.spawn(serve(connection, permit, runtime.clone(), stop.clone()));
                }
                Err(failure) => {
                    drop(permit);
                    // Fail fast and typed. No retry schedule, no sleep loop and
                    // no second accept: a transient-accept policy needs measured
                    // evidence about which kinds are transient on which
                    // platform, and inventing one here would be a policy nobody
                    // reviewed.
                    accept_failure = Some(failure);
                    break;
                }
            }
        }
    }

    // Closing the listener BEFORE the drain is what makes "no later socket is
    // admitted" a property rather than a race: the source is dropped here, so
    // there is no accept left to win.
    drop(source);

    drain(tasks, &connections, &runtime, accept_failure).await
}

/// Fold one joined connection result into the counters.
fn record(counters: &HostCounters, joined: Result<ConnectionOutcome, tokio::task::JoinError>) {
    match joined {
        Ok(Ok(())) => counters.complete_connection(),
        Ok(Err(_)) => counters.fail_connection(),
        // A cancelled or panicking task is a failed connection, not a completed
        // one. Counting it as completed would let a forced close report a clean
        // drain.
        Err(_) => counters.fail_connection(),
    }
}

/// The graceful drain, the deadline race, and the exact forced inventory.
async fn drain(
    mut tasks: JoinSet<ConnectionOutcome>,
    connections: &Arc<Semaphore>,
    runtime: &HostRuntime,
    accept_failure: Option<AcceptFailure>,
) -> ApiHostExit {
    let counters = runtime.counters.as_ref();
    let limits = &runtime.limits;
    let mut deadline = runtime
        .driver
        .drain_deadline(limits.shutdown_drain_allowance);
    let mut completed_before_deadline: u64 = 0;
    let forced = loop {
        if tasks.is_empty() {
            break false;
        }
        tokio::select! {
            joined = tasks.join_next() => match joined {
                Some(result) => {
                    record(counters, result);
                    completed_before_deadline = completed_before_deadline.saturating_add(1);
                }
                None => break false,
            },
            _ = &mut deadline => break true,
        }
    };

    let mut aborted_and_joined: u64 = 0;
    let active_at_deadline = if forced {
        // Snapshot BEFORE aborting. Counting after `abort_all` would race the
        // tasks that were about to finish anyway and would report a number that
        // depends on scheduler timing.
        let snapshot = tasks.len() as u64;
        tasks.abort_all();
        while let Some(result) = tasks.join_next().await {
            record(counters, result);
            aborted_and_joined = aborted_and_joined.saturating_add(1);
        }
        snapshot
    } else {
        0
    };

    // Every owned permit must be back. A connection task holds its permit until
    // its own terminal cleanup and a response body holds its request permit
    // until end of stream or drop, so both semaphores are full exactly when
    // nothing is live.
    let connection_permits = limits
        .max_connections
        .value()
        .saturating_sub(connections.available_permits());
    let request_permits = limits
        .max_inflight_requests
        .value()
        .saturating_sub(runtime.requests.available());
    let stream_permits = limits
        .max_inflight_streams
        .value()
        .saturating_sub(runtime.streams.available());
    if connection_permits != 0 || request_permits != 0 || stream_permits != 0 {
        return ApiHostExit::Failed {
            stage: HostFailureStage::TerminalInventory,
            cause: HostFailureCause::LiveInventory {
                connection_permits,
                request_permits,
                stream_permits,
            },
            drained_gracefully: completed_before_deadline,
            forced_and_joined: aborted_and_joined,
        };
    }
    if counters.overflowed() {
        return ApiHostExit::Failed {
            stage: HostFailureStage::TerminalInventory,
            cause: HostFailureCause::CounterOverflow,
            drained_gracefully: completed_before_deadline,
            forced_and_joined: aborted_and_joined,
        };
    }
    if let Some(failure) = accept_failure {
        return ApiHostExit::Failed {
            stage: HostFailureStage::Accept,
            cause: HostFailureCause::Accept(failure),
            drained_gracefully: completed_before_deadline,
            forced_and_joined: aborted_and_joined,
        };
    }
    if forced {
        return ApiHostExit::ForcedClose {
            admitted_connections: counters.admitted_connections(),
            completed_before_deadline,
            active_at_deadline,
            aborted_and_joined,
        };
    }
    ApiHostExit::StoppedGracefully {
        admitted_connections: counters.admitted_connections(),
        completed_connections: counters.completed_connections(),
        failed_connections: counters.failed_connections(),
        admitted_requests: counters.admitted_requests(),
        refused_requests: counters.refused_requests(),
    }
}

/// One connection: configure, serve, and keep polling through the drain.
async fn serve(
    connection: AcceptedConnection,
    permit: OwnedSemaphorePermit,
    runtime: HostRuntime,
    mut stop: watch::Receiver<bool>,
) -> ConnectionOutcome {
    let HostRuntime {
        service,
        limits,
        driver,
        requests,
        streams,
        counters,
        // The observer belongs to the root owner's accept loop, which is where
        // the retained set exists; a connection task holds no inventory.
        retained: _,
    } = runtime;
    let budget = Arc::new(RequestBudget::new(limits.max_requests_per_connection));
    let stream = Arc::new(StreamConnection::new(
        driver.clone(),
        limits.stream_write_allowance,
    ));
    let admission = AdmissionService::new(service, requests, budget, counters)
        .with_peer(connection.peer())
        .with_streams(streams, stream.clone());
    let io = StreamIo::new(connection.into_stream(), stream);
    let mut served = Box::pin(
        connection_builder(&limits, HostTimer::new(driver)).serve_connection(io, admission),
    );

    let mut signalled = *stop.borrow();
    if signalled {
        served.as_mut().graceful_shutdown();
    }
    let outcome = loop {
        if signalled {
            // Already signalled: keep polling the SAME future to completion.
            // `graceful_shutdown` only disables keep-alive; a connection that
            // stopped being polled here would never settle, and the drain would
            // time out and force-close a response that was about to finish.
            break served.as_mut().await;
        }
        tokio::select! {
            outcome = served.as_mut() => break outcome,
            changed = stop.changed() => {
                // An `Err` means every sender is gone, which is a stop as much
                // as a `true` is: nobody is left to ask for more work.
                let _ = changed;
                signalled = true;
                served.as_mut().graceful_shutdown();
            }
        }
    };

    // The permit is released HERE, after the connection future has resolved and
    // the socket has been dropped — not when the response was written.
    drop(served);
    drop(permit);
    match outcome {
        Ok(()) => Ok(()),
        Err(error) => Err(ConnectionFailureClass::classify(&error)),
    }
}

#[cfg(test)]
mod tests;
