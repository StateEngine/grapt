//! Test builds only: lets a test see the provider reader's full-channel
//! reservation actually waiting, so an interrupt can be timed against it.
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// Called each time one full-channel reservation first returns `Pending`.
#[derive(Clone)]
pub struct ReservationProbe(Arc<dyn Fn() + Send + Sync>);

impl ReservationProbe {
    pub fn new(parked: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(parked))
    }
}

impl fmt::Debug for ReservationProbe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReservationProbe")
    }
}

/// A reservation that reports its first `Pending` poll to the probe.
pub(super) struct Observed<F> {
    reservation: Pin<Box<F>>,
    probe: Option<ReservationProbe>,
}

pub(super) fn observe<F: Future>(reservation: F, probe: Option<ReservationProbe>) -> Observed<F> {
    Observed {
        reservation: Box::pin(reservation),
        probe,
    }
}

impl<F: Future> Future for Observed<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let polled = self.reservation.as_mut().poll(cx);
        if polled.is_pending() {
            if let Some(probe) = self.probe.take() {
                (probe.0)();
            }
        }
        polled
    }
}
