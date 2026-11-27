//! Once Windows begins closing the console, no terminal cleanup or diagnostics
//! may consume the remaining interrupt acknowledgement grace period.
use std::{cell::Cell, io::Write};

#[derive(Debug, Default)]
pub struct Access {
    closing: Cell<bool>,
}

impl Access {
    pub fn close(&self) {
        self.closing.set(true);
    }

    /// Whether this console is on its way out.
    ///
    /// Callers need it to tell a startup refusal, which must be seen, from a
    /// close-path diagnostic, which must never delay the exit.
    pub fn is_closing(&self) -> bool {
        self.closing.get()
    }

    pub fn with_output(&self, operation: impl FnOnce()) {
        if !self.closing.get() {
            operation();
        }
    }

    pub fn report<W: Write>(&self, sink: impl FnOnce() -> W, reason: &str) {
        self.with_output(|| {
            let _ = writeln!(sink(), "grapt-tui: {reason}");
        });
    }

    /// Report a close-path diagnostic without ever waiting on the terminal.
    ///
    /// A close must not be delayed by output. stderr is often a pipe, and a
    /// reader that stopped draining it blocks `write` until the pipe is read or
    /// the process dies, so an ordinary report would hold the exit open. A
    /// thread does the write, and the caller never waits for it: losing a
    /// diagnostic is always better than delaying a close.
    ///
    /// This deliberately ignores the closing flag. Terminal restoration must be
    /// skipped once a close begins, but a lost terminal is often the reason the
    /// close began, and its diagnostic is the one worth keeping.
    pub fn report_on_close(&self, reason: &str) {
        let line = format!("grapt-tui: {reason}\n");
        let _ = std::thread::Builder::new()
            .name("tui-r-close-report".into())
            .spawn(move || {
                let _ = std::io::stderr().write_all(line.as_bytes());
            });
    }
}

#[derive(Debug)]
pub struct Restore<'a, W: Write> {
    access: &'a Access,
    sink: W,
}

impl<'a, W: Write> Restore<'a, W> {
    pub fn new(access: &'a Access, sink: W) -> Self {
        Self { access, sink }
    }
}

impl<W: Write> Drop for Restore<'_, W> {
    fn drop(&mut self) {
        self.access.with_output(|| {
            let _ = crossterm::terminal::disable_raw_mode();
            #[cfg(not(windows))]
            let _ = crossterm::execute!(self.sink, crossterm::event::DisableMouseCapture);
            let _ = crossterm::execute!(
                self.sink,
                crossterm::terminal::BeginSynchronizedUpdate,
                crossterm::event::DisableBracketedPaste,
                crossterm::event::DisableFocusChange,
                crossterm::style::Print("\x1b[?1003l\x1b[?1006l"),
                // Re-enable alternate scroll on the way out, so a terminal left
                // above the alternate screen keeps scrolling normally.
                crossterm::style::Print(RESTORE_ALTERNATE_SCROLL),
                crossterm::terminal::LeaveAlternateScreen,
                crossterm::cursor::Show,
                crossterm::terminal::EndSynchronizedUpdate
            );
        });
    }
}

/// Alternate scroll is one terminal mode, so both halves of its toggle live
/// here with the code that sequences them: the interface turns it off when it
/// enters, and restoration turns it back on.
pub const DISABLE_ALTERNATE_SCROLL: &str = "\x1b[?1007l";
pub const RESTORE_ALTERNATE_SCROLL: &str = "\x1b[?1007h";
