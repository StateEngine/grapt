//! Enforce stream write progress at the socket, including vectored writes.
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::rt::{Read, ReadBufCursor, Write};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

use super::{StreamConnection, StreamWrites};

struct Custody(Arc<StreamConnection>);

impl Drop for Custody {
    fn drop(&mut self) {
        self.0.release();
    }
}

pub(crate) struct StreamIo {
    // Field drop order matters: close the socket before returning its permit.
    inner: TokioIo<TcpStream>,
    custody: Custody,
    writes: StreamWrites,
    keepalive: bool,
}

impl StreamIo {
    pub(crate) fn new(stream: TcpStream, connection: Arc<StreamConnection>) -> Self {
        Self {
            inner: TokioIo::new(stream),
            custody: Custody(connection.clone()),
            writes: StreamWrites::new(connection),
            keepalive: false,
        }
    }

    fn prepare_write(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if !self.keepalive && self.custody.0.active()? {
            let socket = socket2::SockRef::from(self.inner.inner());
            socket.set_tcp_keepalive(
                &socket2::TcpKeepalive::new()
                    .with_time(Duration::from_secs(15))
                    .with_interval(Duration::from_secs(5))
                    .with_retries(3),
            )?;
            // Heartbeat data can be unacknowledged while keepalive is not idle.
            // Bound that case as well on platforms exposing TCP_USER_TIMEOUT.
            #[cfg(any(target_os = "linux", target_os = "android"))]
            socket.set_tcp_user_timeout(Some(self.custody.0.write_allowance()))?;
            self.keepalive = true;
        }
        self.writes.check(cx)
    }
}

impl Read for StreamIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buffer)
    }
}

impl Write for StreamIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.prepare_write(cx)?;
        Pin::new(&mut this.inner).poll_write(cx, bytes)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.prepare_write(cx)?;
        Pin::new(&mut this.inner).poll_write_vectored(cx, bytes)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.writes.check_flush(cx)?;
        let flushed = Pin::new(&mut this.inner).poll_flush(cx);
        if flushed.is_ready() {
            this.writes.flushed();
        }
        flushed
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.writes.check_flush(cx)?;
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}
