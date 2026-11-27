//! Session identity. Law: `PRESERVE` — never interchangeable with message,
//! operation, or execution identity (oracle evidence `schema/src/session-id.ts`).

crate::id::prefixed_id! {
    /// Validated `ses_*` stable session identity.
    SessionId, "ses"
}

/// How a Session was launched: whether it may legitimately run with no window.
///
/// This is a fact about the *launch*, never an inference from who happens to be
/// subscribed. A headless launch is consent to run unwatched, so a window that
/// attaches later and leaves must not stop it. A closed two-valued vocabulary
/// rather than a bare bool or a free string, because it crosses the wire (a
/// launch header) and the durable log (a `session.launched` fact), and the two
/// edges must agree on exactly one spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionLaunch {
    /// Launched with a window (the TUI, or an ordinary client).
    Interactive,
    /// Launched to run with no window at all (the headless CLI).
    Headless,
}

impl SessionLaunch {
    /// The one wire and durable spelling of this mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Headless => "headless",
        }
    }

    /// Parse the wire spelling. Case-sensitive and closed: an unrecognized
    /// value is `None`, so a client cannot invent a third mode that half the
    /// readers would then treat as windowed.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "interactive" => Some(Self::Interactive),
            "headless" => Some(Self::Headless),
            _ => None,
        }
    }

    /// Whether this launch may run with no window.
    pub fn is_headless(self) -> bool {
        matches!(self, Self::Headless)
    }
}

#[cfg(test)]
mod tests {
    use super::SessionId;

    #[test]
    fn parses_and_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let id: SessionId = "ses_033601c26ffe".parse()?;
        assert_eq!(id.to_string(), "ses_033601c26ffe");
        assert_eq!(id.as_str(), "ses_033601c26ffe");
        Ok(())
    }

    #[test]
    fn refuses_foreign_prefix() {
        assert!("msg_033601c26ffe".parse::<SessionId>().is_err());
    }
}
