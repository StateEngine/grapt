//! Shell vocabulary the live terminal reports in.
//!
//! The three fact families are INDEPENDENT: process exit ([`ShellTerminal`]),
//! descendant-pipe drain ([`PipeState`]), and cleanup ([`CleanupState`]). A
//! process exit does NOT imply the pipe drained or cleanup complete.

/// The domain-terminal payload. Terminal does NOT imply the pipe drained or
/// cleanup complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellTerminal {
    /// The domain process exited with a status code.
    Exited(i32),
    /// A scheduled timeout settled the operation without success.
    TimedOut,
    /// Explicitly cancelled.
    Cancelled,
    /// Failed with a typed reason.
    Failed(String),
}

impl ShellTerminal {
    pub fn status_str(&self) -> &'static str {
        match self {
            ShellTerminal::Exited(_) => "exited",
            ShellTerminal::TimedOut => "timed-out",
            ShellTerminal::Cancelled => "cancelled",
            ShellTerminal::Failed(_) => "failed",
        }
    }
}

/// The descendant-pipe drain fact — INDEPENDENT of process exit. A parent may
/// exit while a descendant still holds the pipe; the bounded drain window then
/// either catches the remaining output (`Drained`) or gives up (`TimedOut`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeState {
    Open,
    Drained,
    TimedOut,
}

impl PipeState {
    pub fn as_str(self) -> &'static str {
        match self {
            PipeState::Open => "open",
            PipeState::Drained => "drained",
            PipeState::TimedOut => "timed-out",
        }
    }

    pub fn from_label(raw: &str) -> Option<Self> {
        match raw {
            "open" => Some(PipeState::Open),
            "drained" => Some(PipeState::Drained),
            "timed-out" => Some(PipeState::TimedOut),
            _ => None,
        }
    }

    pub fn is_settled(self) -> bool {
        !matches!(self, PipeState::Open)
    }
}

/// The cleanup fact — INDEPENDENT of process exit. Cleanup completes only after
/// the pipe has settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupState {
    Pending,
    Complete,
}

impl CleanupState {
    pub fn as_str(self) -> &'static str {
        match self {
            CleanupState::Pending => "pending",
            CleanupState::Complete => "complete",
        }
    }

    pub fn from_label(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(CleanupState::Pending),
            "complete" => Some(CleanupState::Complete),
            _ => None,
        }
    }
}
