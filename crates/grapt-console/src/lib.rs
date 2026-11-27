//! OS console events are distinct from raw keyboard events.
//!
//! Windows reports console control events; Unix reports termination signals.
//! Both are turned into the same two intents, because the behaviour is one
//! rule: an interrupt asks the turn to stop, and an unavoidable exit closes the
//! window deliberately.
//!
//! These two things must not be reachable from the rest of the program: the
//! console's own events, and the rule that once a close begins nothing may
//! write to the terminal again. This crate owns neither product behaviour nor
//! the terminal interface: it reports an [`Event`] with an [`Intent`], and it
//! carries the [`Access`] flag that keeps cleanup and diagnostics off a
//! terminal that is going away.
//!
//! The variants are platform-split rather than shared with `cfg` on each arm.
//! A variant only one platform can construct is dead code on the other, and a
//! lint that says so is right: an `Event::Close` on Unix would be a claim this
//! platform never makes.
mod access;
mod listener;

pub use access::{Access, Restore, DISABLE_ALTERNATE_SCROLL, RESTORE_ALTERNATE_SCROLL};
pub use listener::Listener;

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    ControlC,
    Break,
    Close,
    Logoff,
    Shutdown,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// `SIGHUP`: the terminal that owned the seat went away.
    Hangup,
    /// `SIGTERM`: a service manager asked the seat to stop.
    Terminate,
    /// `SIGINT`: a signal rather than a key, kept from being fatal by accident.
    SignalInterrupt,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Intent {
    Interrupt,
    Close,
}

impl Event {
    pub fn intent(self) -> Intent {
        match self {
            #[cfg(windows)]
            Self::ControlC | Self::Break => Intent::Interrupt,
            #[cfg(windows)]
            Self::Close | Self::Logoff | Self::Shutdown => Intent::Close,
            // A closed terminal or a service shutdown is a deliberate close: the
            // seat is going away, so the turn must stop at once rather than be
            // left running with no window until its grace expires.
            #[cfg(unix)]
            Self::Hangup | Self::Terminate => Intent::Close,
            #[cfg(unix)]
            Self::SignalInterrupt => Intent::Interrupt,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn console_events_distinguish_interrupt_from_unavoidable_exit() {
        for event in [Event::ControlC, Event::Break] {
            assert_eq!(event.intent(), Intent::Interrupt);
        }
        for event in [Event::Close, Event::Logoff, Event::Shutdown] {
            assert_eq!(event.intent(), Intent::Close);
        }
    }

    /// A terminal that goes away is the Unix form of a console close, so it must
    /// take the close path rather than kill the process and strand its turn.
    #[cfg(unix)]
    #[test]
    fn unix_termination_signals_close_deliberately() {
        for event in [Event::Hangup, Event::Terminate] {
            assert_eq!(event.intent(), Intent::Close);
        }
        assert_eq!(Event::SignalInterrupt.intent(), Intent::Interrupt);
    }
}
