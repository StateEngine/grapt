//! Register process cancellation before exposing an interruptible interaction.
//!
//! Three Unix signals end a run, and each is a DELIBERATE close: the stop they
//! send reaches the service as an explicit interrupt, never as the silent loss
//! a grace period covers. `SIGHUP` is included because a terminal that goes away
//! is exactly the "console closed" case, and leaving it unhandled would make the
//! run die with the terminal while the turn kept working unwatched.
//!
//! Windows has no hangup signal, so there the same three roles are covered by
//! Ctrl-C and Ctrl-Break; a window close arrives as a console-close event the
//! terminal layer already handles.

use crate::CancellationCause;
use std::io;

#[cfg(unix)]
use tokio::signal::unix::Signal;
#[cfg(windows)]
use tokio::signal::windows::CtrlC as Signal;

pub(super) struct TerminationSignal {
    interrupt: Signal,
    #[cfg(unix)]
    terminate: Signal,
    #[cfg(unix)]
    hangup: Signal,
    #[cfg(windows)]
    terminate: tokio::signal::windows::CtrlBreak,
}
impl TerminationSignal {
    pub(super) fn register() -> io::Result<Self> {
        #[cfg(unix)]
        let (interrupt, terminate, hangup) = {
            use tokio::signal::unix::{signal, SignalKind};
            (
                signal(SignalKind::interrupt())?,
                signal(SignalKind::terminate())?,
                signal(SignalKind::hangup())?,
            )
        };
        #[cfg(windows)]
        let (interrupt, terminate) = (
            tokio::signal::windows::ctrl_c()?,
            tokio::signal::windows::ctrl_break()?,
        );
        Ok(Self {
            interrupt,
            #[cfg(unix)]
            terminate,
            #[cfg(unix)]
            hangup,
            #[cfg(windows)]
            terminate,
        })
    }
    pub(super) async fn recv(&mut self) -> io::Result<CancellationCause> {
        #[cfg(unix)]
        {
            tokio::select! {
                value = self.interrupt.recv() => value.map(|_| CancellationCause::Sigint).ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "SIGINT stream closed")),
                value = self.terminate.recv() => value.map(|_| CancellationCause::Sigterm).ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "SIGTERM stream closed")),
                value = self.hangup.recv() => value.map(|_| CancellationCause::Sighup).ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "SIGHUP stream closed")),
            }
        }
        #[cfg(windows)]
        {
            tokio::select! {
                value = self.interrupt.recv() => value.map(|_| CancellationCause::Sigint).ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "Ctrl-C stream closed")),
                value = self.terminate.recv() => value.map(|_| CancellationCause::Sigterm).ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "Ctrl-Break stream closed")),
            }
        }
    }
}
pub(super) async fn termination_signal() -> io::Result<CancellationCause> {
    TerminationSignal::register()?.recv().await
}
