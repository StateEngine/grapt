//! The one named adapter that owns time for the HTTP/1 host.
//!
//! Law source: H2 deterministic authority — "clock and ID creation at
//! durable/lifecycle boundaries comes from injected, enforced authorities;
//! ambient production time/UUID calls are adapter-only" (master spec L331).
//! This module is that adapter. Nothing in `host.rs`, `http1.rs` or
//! `admission.rs` calls `tokio::time`, `std::time::Instant::now` or
//! `SystemTime::now`, and `tests/host_architecture.rs` is the machine gate that
//! keeps it that way.
//!
//! # Host deadlines
//!
//! The host owns header-read, event-stream-write and shutdown-drain deadlines,
//! using this same injected clock. It owns no
//! general request-execution timeout, and the omission is deliberate rather than
//! unfinished: dropping an arbitrary application future can leave a durable or
//! external effect ambiguous, and binding a request timeout to its reconciliation
//! law belongs to the endpoint lane that has one. A timeout with no
//! reconciliation is not a bound, it is a way to lose an answer.
//!
//! # Why the timer is injected rather than taken from `hyper-util`
//!
//! `hyper_util::rt::TokioTimer` is a correct timer and the production driver
//! delegates its sleeps to exactly that. What it cannot be is *injected*: the
//! shutdown drain has to be controllable by a witness, because the entire
//! graceful-versus-forced distinction is decided by whether the drain deadline
//! fires before the last connection settles. A witness that raced a real clock
//! to prove that would be measuring its own machine.
//!
//! # `now` is the method that matters
//!
//! [`hyper::rt::Timer::now`] has a DEFAULT implementation returning
//! `std::time::Instant::now()`, and hyper's HTTP/1 connection computes its
//! header deadline as `timer.now() + header_read_timeout`
//! (`hyper-1.11.0/src/proto/h1/conn.rs:221`) before handing it to `sleep_until`.
//! An adapter that inherits that default therefore mixes an ambient wall-clock
//! reading with a virtual-clock sleep, and the deadline lands wherever the two
//! clocks happen to disagree. [`DeadlineDriver::now`] exists so that reading is
//! the driver's, and [`HostTimer`] overrides all four trait methods rather than
//! inheriting any.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::time::Duration;
use std::sync::Arc;
use std::time::Instant;

use hyper::rt::{Sleep, Timer};
use hyper_util::rt::TokioTimer;

use crate::limits::{HeaderReadAllowance, ShutdownDrainAllowance};

/// A future the host awaits and never inspects.
pub type BoxedDeadline = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The process-local time authority the host runs under.
///
/// Deliberately narrow: four timing methods hyper needs, plus the one drain
/// deadline H owns. It is not a general scheduler, it hands out no absolute
/// timestamps to durable code, and nothing it returns is recorded.
pub trait DeadlineDriver: fmt::Debug + Send + Sync + 'static {
    /// A sleep of `duration`.
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Sleep>>;

    /// A sleep that resolves at `deadline`.
    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Sleep>>;

    /// Re-point an existing sleep at `deadline`.
    fn reset(&self, sleep: &mut Pin<Box<dyn Sleep>>, deadline: Instant);

    /// This driver's reading of now.
    ///
    /// The one method whose default would be an ambient clock. See the module
    /// note: hyper adds this to the configured allowance and sleeps until the
    /// sum, so a reading from a different clock than [`Self::sleep_until`] uses
    /// is not a small inaccuracy — it is a deadline in the wrong epoch.
    fn now(&self) -> Instant;

    /// Resolves when the graceful-drain allowance is exhausted.
    ///
    /// Returned as an owned future rather than driven by the driver, so the host
    /// selects it against the drain and never has to ask whether it has fired.
    fn drain_deadline(&self, allowance: ShutdownDrainAllowance) -> BoxedDeadline;
}

/// The production driver. The only place in this crate that calls `tokio::time`.
///
/// Its sleeps are `hyper_util::rt::TokioTimer`'s, which is a deliberate
/// non-duplication: reimplementing a `Sleep` type here would give the
/// `reset` downcast a second shape to fail on and would carry upstream's law a
/// second time. What this driver adds is injectability and the `now` reading,
/// and `tokio::time::Instant::now` is what makes a paused clock work end to end.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioDeadlineDriver;

impl TokioDeadlineDriver {
    pub fn new() -> Self {
        Self
    }
}

impl DeadlineDriver for TokioDeadlineDriver {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Sleep>> {
        TokioTimer::new().sleep(duration)
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Sleep>> {
        TokioTimer::new().sleep_until(deadline)
    }

    fn reset(&self, sleep: &mut Pin<Box<dyn Sleep>>, deadline: Instant) {
        TokioTimer::new().reset(sleep, deadline);
    }

    fn now(&self) -> Instant {
        // `tokio::time::Instant::now`, NOT `std::time::Instant::now`. Under a
        // paused runtime clock the two disagree by however long the test has
        // been running, and hyper compares this reading against a sleep taken
        // from the same virtual clock.
        tokio::time::Instant::now().into()
    }

    fn drain_deadline(&self, allowance: ShutdownDrainAllowance) -> BoxedDeadline {
        Box::pin(tokio::time::sleep(allowance.value()))
    }
}

/// The `hyper::rt::Timer` face of a [`DeadlineDriver`].
///
/// Every one of the trait's four methods is written out. `now` is the
/// load-bearing one and `reset` is delegated rather than left to its default so
/// that a driver supplying a non-Tokio sleep cannot be silently re-pointed
/// through a `sleep_until` it did not author.
#[derive(Clone)]
pub struct HostTimer {
    driver: Arc<dyn DeadlineDriver>,
}

impl HostTimer {
    pub fn new(driver: Arc<dyn DeadlineDriver>) -> Self {
        Self { driver }
    }
}

impl fmt::Debug for HostTimer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostTimer")
            .field("driver", &self.driver)
            .finish()
    }
}

impl Timer for HostTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Sleep>> {
        self.driver.sleep(duration)
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Sleep>> {
        self.driver.sleep_until(deadline)
    }

    fn reset(&self, sleep: &mut Pin<Box<dyn Sleep>>, new_deadline: Instant) {
        self.driver.reset(sleep, new_deadline);
    }

    fn now(&self) -> Instant {
        self.driver.now()
    }
}

/// The header-read allowance as hyper wants it.
///
/// A one-line function so that the conversion from H's checked type to hyper's
/// `Option<Duration>` has a name and a single site. `Some(..)` is the whole
/// point: `header_read_timeout(None)` disables the deadline, and a mutation that
/// substitutes it is a real defect rather than a formatting change.
pub(crate) fn header_read_timeout(allowance: HeaderReadAllowance) -> Option<Duration> {
    Some(allowance.value())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A driver that counts which methods the host actually reached.
    #[derive(Debug, Default)]
    struct CountingDriver {
        nows: AtomicUsize,
        sleeps: AtomicUsize,
        sleep_untils: AtomicUsize,
        resets: AtomicUsize,
    }

    impl DeadlineDriver for CountingDriver {
        fn sleep(&self, duration: Duration) -> Pin<Box<dyn Sleep>> {
            self.sleeps.fetch_add(1, Ordering::SeqCst);
            TokioDeadlineDriver.sleep(duration)
        }

        fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Sleep>> {
            self.sleep_untils.fetch_add(1, Ordering::SeqCst);
            TokioDeadlineDriver.sleep_until(deadline)
        }

        fn reset(&self, sleep: &mut Pin<Box<dyn Sleep>>, deadline: Instant) {
            self.resets.fetch_add(1, Ordering::SeqCst);
            TokioDeadlineDriver.reset(sleep, deadline);
        }

        fn now(&self) -> Instant {
            self.nows.fetch_add(1, Ordering::SeqCst);
            TokioDeadlineDriver.now()
        }

        fn drain_deadline(&self, allowance: ShutdownDrainAllowance) -> BoxedDeadline {
            TokioDeadlineDriver.drain_deadline(allowance)
        }
    }

    // A runtime is required, not incidental: the production driver's sleeps are
    // Tokio's, and constructing one outside a reactor is a panic rather than a
    // pending future.
    #[tokio::test]
    async fn the_timer_delegates_every_method_to_its_driver() {
        // The adapter is only injected if all four methods route to the driver.
        // A method left to its trait default would be invisible here, and `now`'s
        // default is the ambient clock this lane forbids.
        let driver = Arc::new(CountingDriver::default());
        let timer = HostTimer::new(driver.clone());

        let now = Timer::now(&timer);
        let mut sleep = timer.sleep(Duration::from_millis(1));
        // `drop` rather than `let _`: a non-binding `let` on a future is a lint
        // this workspace denies, and rightly — it usually means an await was
        // forgotten. Here the sleep is deliberately never awaited, because what
        // is being counted is that the driver was ASKED for one.
        drop(timer.sleep_until(now + Duration::from_millis(1)));
        timer.reset(&mut sleep, now + Duration::from_millis(2));

        assert_eq!(
            driver.nows.load(Ordering::SeqCst),
            1,
            "now was not delegated"
        );
        assert_eq!(
            driver.sleeps.load(Ordering::SeqCst),
            1,
            "sleep was not delegated"
        );
        assert_eq!(
            driver.sleep_untils.load(Ordering::SeqCst),
            1,
            "sleep_until was not delegated"
        );
        assert_eq!(
            driver.resets.load(Ordering::SeqCst),
            1,
            "reset was not delegated; its default would re-point through a \
             sleep_until the driver did not author"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_production_driver_reads_the_runtime_clock_not_the_wall_clock() {
        // The exact defect the module note describes, asserted directly. Under a
        // paused runtime the virtual clock stands still while the wall clock does
        // not, so a driver that had inherited hyper's default `now` would drift
        // away from its own sleeps. Advancing the runtime clock must move this
        // reading by exactly the advance.
        let driver = TokioDeadlineDriver::new();
        let before = driver.now();
        tokio::time::advance(Duration::from_secs(30)).await;
        let after = driver.now();

        assert_eq!(
            after.duration_since(before),
            Duration::from_secs(30),
            "the driver's clock must be the runtime's, so a witness can advance it"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_drain_deadline_resolves_only_after_its_allowance() {
        let Ok(allowance) = ShutdownDrainAllowance::try_new(Duration::from_secs(5)) else {
            panic!("a five second drain allowance must be admitted");
        };
        let driver = TokioDeadlineDriver::new();
        let mut deadline = driver.drain_deadline(allowance);

        // Just short of the allowance it is still pending: a deadline that fired
        // early would render every graceful drain as a forced close.
        tokio::time::advance(Duration::from_secs(4)).await;
        assert!(
            futures_pending(&mut deadline),
            "the drain deadline fired before its allowance"
        );

        tokio::time::advance(Duration::from_secs(2)).await;
        deadline.await;
    }

    /// Polls once and reports whether the future is still pending.
    ///
    /// `Waker::noop` rather than a hand-rolled `RawWakerVTable`: the workspace
    /// forbids `unsafe_code`, and one poll needs no wakeup. It also means this
    /// crate takes no `futures-util` edge to ask a one-line question.
    fn futures_pending(future: &mut BoxedDeadline) -> bool {
        use core::task::{Context, Poll, Waker};

        let mut context = Context::from_waker(Waker::noop());
        matches!(future.as_mut().poll(&mut context), Poll::Pending)
    }
}
