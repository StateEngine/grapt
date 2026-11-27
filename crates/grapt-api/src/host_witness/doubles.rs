use core::future::Future;
use core::pin::Pin;
use core::time::Duration;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::deadline::{BoxedDeadline, DeadlineDriver, TokioDeadlineDriver};
use crate::host::{AcceptFailure, AcceptSource};
use crate::limits::ShutdownDrainAllowance;

// ---------------------------------------------------------------------------
// Deterministic doubles.
// ---------------------------------------------------------------------------

/// A drain deadline that never fires, so a graceful drain is decided by the
/// connection settling rather than by racing a clock.
#[derive(Debug)]
pub(super) struct NeverFiringDrain;

impl DeadlineDriver for NeverFiringDrain {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn hyper::rt::Sleep>> {
        TokioDeadlineDriver.sleep(duration)
    }

    fn sleep_until(&self, deadline: std::time::Instant) -> Pin<Box<dyn hyper::rt::Sleep>> {
        TokioDeadlineDriver.sleep_until(deadline)
    }

    fn reset(&self, sleep: &mut Pin<Box<dyn hyper::rt::Sleep>>, deadline: std::time::Instant) {
        TokioDeadlineDriver.reset(sleep, deadline);
    }

    fn now(&self) -> std::time::Instant {
        TokioDeadlineDriver.now()
    }

    fn drain_deadline(&self, _allowance: ShutdownDrainAllowance) -> BoxedDeadline {
        Box::pin(core::future::pending())
    }
}

/// A drain deadline the witness fires by hand.
#[derive(Debug)]
pub(super) struct ManualDrain {
    fired: tokio::sync::watch::Sender<bool>,
}

impl ManualDrain {
    pub(super) fn new() -> Self {
        let (fired, _) = tokio::sync::watch::channel(false);
        Self { fired }
    }

    /// Exhaust the allowance NOW, synchronously, before any drain begins.
    ///
    /// The first version of this waited for the host to SUBSCRIBE and only then
    /// sent, to avoid firing into an empty channel. That is a race the witness
    /// can lose in exactly the case this lane needed to measure: when every
    /// connection has already completed, the drain reaps the whole backlog and
    /// returns without ever awaiting the deadline, so the subscription appears
    /// and disappears between two polls of the waiting loop — which then spins
    /// forever on a condition that has already come and gone. Arming up front
    /// removes the race rather than timing it: `drain_deadline` reads the
    /// current value on its first poll, so an armed deadline is exhausted the
    /// instant the drain asks for it.
    pub(super) fn arm(&self) {
        // `send_replace`, NOT `send`. `watch::Sender::send` FAILS when every
        // receiver has been dropped, and — the part that matters — it does not
        // store the value in that case. This double drops its own receiver at
        // construction and the host does not subscribe until the drain begins,
        // so arming beforehand with `send` set nothing at all: the drain then
        // subscribed, read `false`, and waited forever on a deadline the witness
        // believed it had already exhausted. `send_replace` stores
        // unconditionally, which is the whole point of arming up front.
        self.fired.send_replace(true);
    }
}

impl DeadlineDriver for ManualDrain {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn hyper::rt::Sleep>> {
        TokioDeadlineDriver.sleep(duration)
    }

    fn sleep_until(&self, deadline: std::time::Instant) -> Pin<Box<dyn hyper::rt::Sleep>> {
        TokioDeadlineDriver.sleep_until(deadline)
    }

    fn reset(&self, sleep: &mut Pin<Box<dyn hyper::rt::Sleep>>, deadline: std::time::Instant) {
        TokioDeadlineDriver.reset(sleep, deadline);
    }

    fn now(&self) -> std::time::Instant {
        TokioDeadlineDriver.now()
    }

    fn drain_deadline(&self, _allowance: ShutdownDrainAllowance) -> BoxedDeadline {
        let mut subscribed = self.fired.subscribe();
        Box::pin(async move {
            loop {
                if *subscribed.borrow() {
                    return;
                }
                if subscribed.changed().await.is_err() {
                    return;
                }
            }
        })
    }
}

/// An accept source that fails once, deterministically.
#[derive(Debug)]
pub(super) struct FailingAcceptSource {
    failure: AcceptFailure,
    attempts: AtomicUsize,
}

impl FailingAcceptSource {
    pub(super) fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    pub(super) fn new(failure: AcceptFailure) -> Self {
        Self {
            failure,
            attempts: AtomicUsize::new(0),
        }
    }
}

/// Shares one [`FailingAcceptSource`] between the host and the witness, so the
/// witness can read the attempt count after the host has consumed the source.
#[derive(Debug)]
pub(super) struct CountingAcceptSource {
    inner: Arc<FailingAcceptSource>,
}

impl CountingAcceptSource {
    pub(super) fn new(inner: Arc<FailingAcceptSource>) -> Self {
        Self { inner }
    }
}

impl AcceptSource for CountingAcceptSource {
    fn accept(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<grapt_location::AcceptedConnection, AcceptFailure>>
                + Send
                + '_,
        >,
    > {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            inner.attempts.fetch_add(1, Ordering::SeqCst);
            Err(inner.failure)
        })
    }

    fn observed_address(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1)
    }
}
