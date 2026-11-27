//! Unix termination signals as console-level events.
//!
//! A terminal that goes away is the Unix form of "the console closed": the
//! process gets `SIGHUP`, or `SIGTERM` from a service manager shutting the seat
//! down. Left unhandled, both kill the process where it stands, the event
//! subscription drops, and a turn nobody is watching keeps running until its
//! grace expires. Reading them here lets the same deliberate close a Windows
//! console event takes stop the turn at once.
//!
//! Tokio installs a signal handler through a reactor, and the TUI's main loop is
//! synchronous, so registration cannot happen on that thread. One worker owns a
//! current-thread runtime and hands what it saw to the loop as STATE, never as a
//! queued event: a close that arrived while an interrupt was unread is still a
//! close, because nothing between the handler and the loop can discard it.
use crate::Event;
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    mpsc::SyncSender,
    Arc,
};
use tokio::signal::unix::{signal, SignalKind};

#[cfg(all(test, target_os = "linux"))]
#[path = "unix/tests.rs"]
mod signal_tests;

/// No close is pending.
const NO_CLOSE: u8 = 0;
/// A hangup is pending: the terminal that owned the seat went away.
const HANGUP: u8 = 1;
/// A termination is pending: a service manager asked the seat to stop.
const TERMINATE: u8 = 2;

/// The signals seen since the loop last looked.
///
/// A close and an interrupt are different facts, so they are separate flags
/// rather than two entries in one queue: coalescing either one away could make
/// the seat ignore a shutdown, and a one-slot channel did exactly that. Setting
/// a flag cannot fail, so no signal is ever dropped for lack of room.
#[derive(Debug, Default)]
struct Seen {
    close: AtomicU8,
    interrupt: AtomicBool,
}

impl Seen {
    /// Record a close. A hangup and a termination mean the same thing to the
    /// seat; the first one seen names it, and a later one changes nothing it
    /// would decide.
    fn close(&self, kind: u8) {
        let _ = self
            .close
            .compare_exchange(NO_CLOSE, kind, Ordering::AcqRel, Ordering::Acquire);
    }

    fn interrupt(&self) {
        self.interrupt.store(true, Ordering::Release);
    }

    /// The next event, with a pending close always ahead of a pending interrupt.
    fn take(&self) -> Option<Event> {
        match self.close.swap(NO_CLOSE, Ordering::AcqRel) {
            HANGUP => return Some(Event::Hangup),
            TERMINATE => return Some(Event::Terminate),
            _ => {}
        }
        // A close is never converted into an interrupt: an unread interrupt
        // waits but can never stand in front of one.
        self.interrupt
            .swap(false, Ordering::AcqRel)
            .then_some(Event::SignalInterrupt)
    }
}

#[derive(Debug)]
pub struct Listener {
    seen: Arc<Seen>,
}

impl Listener {
    pub fn new(wake: SyncSender<()>) -> Result<Self, String> {
        let seen = Arc::new(Seen::default());
        let owner = Arc::clone(&seen);
        std::thread::Builder::new()
            .name("tui-r-console".into())
            .spawn(move || {
                // A registration failure must not kill the seat: the seat then
                // behaves exactly as it did before this listener existed, which
                // is a signal's default action rather than a crash on startup.
                if let Err(reason) = watch(&owner, &wake) {
                    eprintln!("console control unavailable: {reason}");
                }
            })
            .map_err(|error| {
                format!("Cannot start the console signal owner: {:?}", error.kind())
            })?;
        Ok(Self { seen })
    }

    /// The next console event, if a signal has been seen since the last call.
    ///
    /// This reads flags the loop shares with the signal owner, never a reactor:
    /// the loop owns no runtime, and a lost wake costs nothing because the loop
    /// asks again on its next tick.
    pub fn poll(&mut self) -> Option<Event> {
        self.seen.take()
    }
}

/// Own the runtime that Tokio's signal registration needs.
fn watch(seen: &Seen, wake: &SyncSender<()>) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| format!("signal runtime refused: {:?}", error.kind()))?;
    runtime.block_on(async move {
        let mut interrupt = signal(SignalKind::interrupt()).map_err(io_reason)?;
        let mut terminate = signal(SignalKind::terminate()).map_err(io_reason)?;
        let mut hangup = signal(SignalKind::hangup()).map_err(io_reason)?;
        loop {
            // Biased so a hangup wins a tie: the terminal going away is never
            // an interrupt the user could answer.
            tokio::select! {
                biased;
                _ = hangup.recv() => seen.close(HANGUP),
                _ = terminate.recv() => seen.close(TERMINATE),
                // SIGINT still arrives as a key event through the terminal; this
                // arm exists so a signal racing the reader is never fatal, not
                // to change how Ctrl-C already behaves.
                _ = interrupt.recv() => seen.interrupt(),
                // `recv` only ends if its handler is removed, which no owner
                // does: stop watching rather than spin on a closed source.
                else => break,
            }
            // A lost wake is harmless: the flag is already set, and the loop
            // re-reads it on its next tick.
            let _ = wake.try_send(());
        }
        Ok(())
    })
}

fn io_reason(error: std::io::Error) -> String {
    format!("signal registration refused: {:?}", error.kind())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A close that lands while an interrupt is unread must still close.
    ///
    /// The one-slot channel this replaced dropped exactly this case: the
    /// interrupt filled the slot and the hangup was discarded, so the seat kept
    /// running after its terminal went away.
    #[test]
    fn a_close_is_never_lost_behind_an_unread_interrupt() -> Result<(), String> {
        let seen = Seen::default();
        seen.interrupt();
        seen.close(HANGUP);
        assert_eq!(seen.take(), Some(Event::Hangup));
        // The interrupt is still owed, and it is reported afterwards rather
        // than being swallowed by the close.
        assert_eq!(seen.take(), Some(Event::SignalInterrupt));
        assert_eq!(seen.take(), None);
        Ok(())
    }

    /// The first close names the event; a second cannot replace it, and a
    /// termination that arrives alone is still a close.
    #[test]
    fn one_close_is_enough_and_a_termination_closes_too() -> Result<(), String> {
        let seen = Seen::default();
        seen.close(TERMINATE);
        assert_eq!(seen.take(), Some(Event::Terminate));
        seen.close(HANGUP);
        seen.close(TERMINATE);
        assert_eq!(seen.take(), Some(Event::Hangup));
        assert_eq!(seen.take(), None);
        Ok(())
    }
}
