//! The Sessions one window is a window onto.
use grapt_types::SessionId;

/// The Session a window shows and, newest first, the Sessions it left.
///
/// Leaving a Session is not closing it: a window that switches Sessions or
/// opens a subagent's view can go back, so every subscription it opens keeps a
/// claim on the Sessions it left as well. This is window state, so a
/// replacement connection adopts it whole rather than starting empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchedSessions {
    current: Option<SessionId>,
    retained: Vec<SessionId>,
}

impl WatchedSessions {
    /// A window that starts by showing `current`.
    pub fn showing(current: Option<SessionId>) -> Self {
        Self {
            current,
            retained: Vec::new(),
        }
    }

    /// The Session shown now.
    pub fn current(&self) -> Option<&SessionId> {
        self.current.as_ref()
    }

    /// The Sessions left, newest first, at most
    /// [`grapt_api::u3a::MAX_RETAINED_SESSIONS`].
    pub fn retained(&self) -> &[SessionId] {
        &self.retained
    }

    /// Show `session`; the Session left stays retained. Returns whether the
    /// shown Session changed.
    pub fn show(&mut self, session: Option<SessionId>) -> bool {
        if self.current == session {
            return false;
        }
        if let Some(left) = std::mem::replace(&mut self.current, session) {
            self.retained.retain(|kept| *kept != left);
            self.retained.insert(0, left);
        }
        if let Some(current) = &self.current {
            self.retained.retain(|kept| kept != current);
        }
        self.retained
            .truncate(grapt_api::u3a::MAX_RETAINED_SESSIONS);
        true
    }
}
