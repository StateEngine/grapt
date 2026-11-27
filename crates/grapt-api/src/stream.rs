//! HTTP stream ownership and liveness, independent of ordinary request slots.
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use hyper::body::{Frame, SizeHint};
use hyper::rt::Sleep;
use tokio::sync::OwnedSemaphorePermit;

use crate::deadline::DeadlineDriver;
use crate::limits::StreamWriteAllowance;
mod transport;
pub(crate) use transport::StreamIo;

const HEARTBEAT: Duration = Duration::from_secs(15);

pub(crate) fn is_event_stream(method: &axum::http::Method, path: &str) -> bool {
    *method == axum::http::Method::GET
        && matches!(
            path,
            crate::u3a::DOGFOOD_EVENT_PATH | crate::u3a::CAPTURE_EVENT_PATH
        )
}

/// The connection owner releases the stream permit after transport teardown.
/// A body ending alone does not prove its buffered bytes have been written.
pub(crate) struct StreamConnection {
    pub(crate) driver: Arc<dyn DeadlineDriver>,
    /// How long one write batch may stall before this connection is reclaimed.
    ///
    /// One duration, enforced independently: the driver sleeps for it here, and
    /// on Linux the socket's user timeout is armed with the same value. They
    /// share the number, not a clock.
    write_allowance: Duration,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}

impl StreamConnection {
    pub(crate) fn new(driver: Arc<dyn DeadlineDriver>, allowance: StreamWriteAllowance) -> Self {
        Self {
            driver,
            write_allowance: allowance.value(),
            permit: Mutex::new(None),
        }
    }

    /// The same value the driver sleeps for and the socket's user timeout
    /// takes, so one policy duration reaches both enforcement routes.
    pub(crate) fn write_allowance(&self) -> Duration {
        self.write_allowance
    }

    pub(crate) fn activate(&self, permit: OwnedSemaphorePermit) -> io::Result<()> {
        let mut held = self
            .permit
            .lock()
            .map_err(|_| io::Error::other("stream custody is unavailable"))?;
        if held.is_some() {
            return Err(io::Error::other("this connection already owns a stream"));
        }
        *held = Some(permit);
        Ok(())
    }

    pub(crate) fn active(&self) -> io::Result<bool> {
        self.permit
            .lock()
            .map(|held| held.is_some())
            .map_err(|_| io::Error::other("stream custody is unavailable"))
    }

    pub(crate) fn release(&self) {
        let mut held = match self.permit.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        *held = None;
    }
}

pub(crate) struct HeartbeatBody {
    inner: Body,
    driver: Arc<dyn DeadlineDriver>,
    heartbeat: Pin<Box<dyn Sleep>>,
}

impl HeartbeatBody {
    pub(crate) fn new(inner: Body, driver: Arc<dyn DeadlineDriver>) -> Self {
        let heartbeat = driver.sleep(HEARTBEAT);
        Self {
            inner,
            driver,
            heartbeat,
        }
    }
}

impl hyper::body::Body for HeartbeatBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Pending => {}
            frame => {
                this.driver
                    .reset(&mut this.heartbeat, this.driver.now() + HEARTBEAT);
                return frame;
            }
        }
        if std::future::Future::poll(this.heartbeat.as_mut(), cx).is_ready() {
            this.driver
                .reset(&mut this.heartbeat, this.driver.now() + HEARTBEAT);
            return Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(
                b": keepalive\n\n",
            )))));
        }
        Poll::Pending
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        // A heartbeat can add bytes while the underlying stream is idle.
        SizeHint::default()
    }
}

/// A write batch keeps one deadline through partial progress until flush.
/// Idle streams have no write deadline; their heartbeat supplies the next write.
pub(crate) struct StreamWrites {
    connection: Arc<StreamConnection>,
    pending: Option<Pin<Box<dyn Sleep>>>,
}

impl StreamWrites {
    pub(crate) fn new(connection: Arc<StreamConnection>) -> Self {
        Self {
            connection,
            pending: None,
        }
    }

    pub(crate) fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if !self.connection.active()? {
            self.pending = None;
            return Ok(());
        }
        let deadline = self.pending.get_or_insert_with(|| {
            self.connection
                .driver
                .sleep(self.connection.write_allowance())
        });
        if std::future::Future::poll(deadline.as_mut(), cx).is_ready() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "event stream write allowance elapsed",
            ));
        }
        Ok(())
    }

    pub(crate) fn flushed(&mut self) {
        self.pending = None;
    }

    pub(crate) fn check_flush(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.pending.is_some() {
            self.check(cx)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deadline::TokioDeadlineDriver;
    use std::task::{Context, Waker};

    /// A stream that owns capacity, which is what arms a write allowance.
    fn activated() -> Arc<StreamConnection> {
        let connection = Arc::new(StreamConnection::new(
            Arc::new(TokioDeadlineDriver::new()),
            StreamWriteAllowance::try_new(Duration::from_secs(30))
                .unwrap_or_else(|error| panic!("fixture allowance refused: {error}")),
        ));
        let Ok(permit) = Arc::new(tokio::sync::Semaphore::new(1)).try_acquire_owned() else {
            panic!("the fixture stream permit must be available");
        };
        if let Err(error) = connection.activate(permit) {
            panic!("the fixture stream must own its permit: {error}");
        }
        connection
    }

    /// A write remains admitted before the thirty-second contract expires.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_write_batch_fails_only_once_the_allowance_elapses() {
        let mut writes = StreamWrites::new(activated());
        let mut context = Context::from_waker(Waker::noop());

        assert!(
            writes.check(&mut context).is_ok(),
            "arming the allowance must not fail a batch that has not stalled"
        );
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(
            writes.check(&mut context).is_ok(),
            "the write must remain admitted before thirty seconds"
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        match writes.check(&mut context) {
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            other => panic!("an elapsed allowance must fail the batch: {other:?}"),
        }
    }

    /// Releasing capacity clears a deadline that was armed while active.
    #[tokio::test(start_paused = true)]
    async fn a_released_stream_clears_its_armed_allowance() {
        let connection = activated();
        let mut writes = StreamWrites::new(connection.clone());
        let mut context = Context::from_waker(Waker::noop());

        assert!(
            writes.check(&mut context).is_ok(),
            "the active stream must arm its allowance without failing"
        );
        tokio::time::advance(Duration::from_secs(31)).await;
        connection.release();
        assert!(
            writes.check(&mut context).is_ok(),
            "release must discard the elapsed deadline instead of failing a write"
        );
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(writes.check(&mut context).is_ok());
    }
}
