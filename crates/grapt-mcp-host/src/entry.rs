//! What configuration DECLARED, and what the registry currently holds for it.
//!
//! Split out of `mcp.rs` under the workspace god-file ceiling. This is the
//! status vocabulary: the shapes a reader is told about, and the rule that an
//! observed failure outranks a startup that once succeeded.

use grapt_mcp::session::Unusable;

use crate::startup::StartPhase;

/// Nonblocking status supplied by the owner of a live connection.
pub trait LiveStatus: std::fmt::Debug {
    fn observed_unusable(&self) -> Option<Unusable>;
    fn identity(&self) -> &grapt_mcp::ServerIdentity;
    fn catalog_len(&self) -> usize;
    fn stderr_tail(&self) -> Option<String>;
}

/// What the product knows about one declared server right now.
#[derive(Debug)]
pub enum ServerEntry<L: LiveStatus> {
    Disabled,
    /// Started, handshaken and discovered. The connection is HELD: this is a
    /// live session, not the memory of a successful probe, so reporting it as
    /// connected is a statement about the present.
    Live(Box<L>),
    /// Unusable, and WHERE it became so.
    ///
    /// The phase is carried because the reason cannot supply it: `TimedOut` is
    /// the same value for a handshake that never answered and for discovery
    /// that ran long, and those point an operator at different settings now
    /// that the two are separately configurable.
    /// Unusable, and WHERE it became so when that is known.
    ///
    /// The phase is carried because the reason cannot supply it: `TimedOut` is
    /// the same value for a handshake that never answered and for discovery
    /// that ran long, and those point an operator at different settings now
    /// that the two are separately configurable. `None` is for the states
    /// that have no startup phase at all, such as a connection observed
    /// unusable long after it started.
    Unusable(Unusable, Option<StartPhase>, String),
}

impl<L: LiveStatus> ServerEntry<L> {
    /// A failure that knows its phase.
    pub fn unusable_in(reason: Unusable, phase: StartPhase) -> Self {
        Self::Unusable(reason, Some(phase), String::new())
    }

    /// A failure with no startup phase to name.
    pub fn unusable(reason: Unusable) -> Self {
        Self::Unusable(reason, None, String::new())
    }

    pub fn status_label(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            // An OBSERVED failure outranks the startup that once succeeded.
            Self::Live(live) => match live.observed_unusable() {
                Some(reason) => reason.label(),
                None => "connected",
            },
            Self::Unusable(reason, _, _) => reason.label(),
        }
    }

    /// Why this server is in the state it is, for the operator-facing message.
    /// A live server reports its own identity; nothing here invents a reason.
    pub fn detail(&self) -> String {
        let mut detail = match self {
            Self::Disabled => "it is disabled in configuration".to_owned(),
            // The negotiated revision is named because it is the server's
            // answer, not our offer, and an operator diagnosing a server that
            // behaves oddly needs to know which revision the conversation
            // actually settled on.
            Self::Live(server) => match server.observed_unusable() {
                Some(reason) => Self::unusable(reason).detail(),
                None => format!(
                    "connected to {} over MCP {} with {} discovered tool(s)",
                    server.identity().name(),
                    server.identity().negotiated_protocol(),
                    self.tool_count()
                ),
            },
            // NAMES THE PHASE THAT ACTUALLY FAILED. Every timeout used to be
            // reported as the startup budget, so a server that handshook
            // cleanly and was slow to list its tools sent the operator to the
            // wrong setting entirely.
            Self::Unusable(Unusable::TimedOut, Some(phase), _) => format!(
                "{} did not complete within the {} budget",
                phase.operation(),
                phase.budget_name()
            ),
            Self::Unusable(Unusable::TimedOut, None, _) => {
                "it did not answer within its budget".to_owned()
            }
            Self::Unusable(Unusable::Disconnected, _, _) => "it closed the channel".to_owned(),
            Self::Unusable(Unusable::Disabled, _, _) => "it is disabled".to_owned(),
            Self::Unusable(Unusable::Failed(reason), _, _) => reason.clone(),
        };
        let tail = match self {
            Self::Live(server) if server.observed_unusable().is_some() => server.stderr_tail(),
            Self::Live(_) => None,
            Self::Unusable(_, _, tail) => Some(tail.clone()),
            Self::Disabled => None,
        };
        if let Some(tail) = tail.filter(|tail| !tail.is_empty()) {
            detail.push_str("; stderr: ");
            detail.push_str(&tail);
        }
        detail
    }

    /// Why a turn cannot use this server, for a durable user-facing notice;
    /// `None` when it can, or when the user turned it off.
    ///
    /// A CLASSIFICATION, never `detail()`: a failure's own text can carry
    /// what the server said (`the server refused: …`) or echo its frames, and
    /// a notice is kept in the Session's history. The live status keeps the
    /// detail.
    pub fn unavailable_reason(&self) -> Option<String> {
        let (reason, phase) = match self {
            Self::Disabled => return None,
            Self::Live(server) => (server.observed_unusable()?, None),
            Self::Unusable(reason, phase, _) => (reason.clone(), *phase),
        };
        Some(match (reason, phase) {
            (Unusable::Disabled, _) => return None,
            (Unusable::TimedOut, Some(phase)) => format!(
                "{} did not complete within its {} budget",
                phase.operation(),
                phase.budget_name()
            ),
            (Unusable::TimedOut, None) => "it did not answer within its budget".to_owned(),
            (Unusable::Disconnected, _) => "it closed the channel".to_owned(),
            (Unusable::Failed(_), _) => {
                "it could not be used; its MCP status names the reason".to_owned()
            }
        })
    }

    /// The discovered tools, or an empty slice for a server that has none to
    /// offer BECAUSE it is not usable. A caller must not read the two the same
    /// way, which is why `status_label` is reported alongside.
    /// How many tools this server published.
    ///
    /// Read from the catalog the connection recorded at discovery, NOT from
    /// its live session: a status read must never wait on the I/O lock, or one
    /// slow tool call would stall every status poll — the coupling this split
    /// exists to remove.
    pub fn tool_count(&self) -> usize {
        match self {
            // A connection observed dead offers nothing. Its catalog is what
            // it published while it was alive, and advertising that now would
            // hand a model tools no call can reach.
            Self::Live(server) if server.observed_unusable().is_none() => server.catalog_len(),
            Self::Live(_) | Self::Disabled | Self::Unusable(..) => 0,
        }
    }
}
