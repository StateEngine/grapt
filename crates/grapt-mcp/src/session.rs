//! The deterministic connection lifecycle for one configured MCP server.
//!
//! J02 asks for bounded startup, and for timeout, disconnect and unavailable to
//! be demonstrably different states, with no duplicate execution and no
//! resurrected grant after a restart. Those are decisions about ORDER, not
//! about bytes on a socket, so they live here as a state machine with no I/O
//! and no clock: the transport adapter above supplies the events, and every
//! answer is a pure function of the events already seen.
//!
//! Keeping it separate is what makes the hard cases testable at all. A
//! duplicate terminal, a reply arriving after a disconnect, and a lease minted
//! before a restart being presented after it are all races that a live process
//! produces rarely and a state machine produces on demand.
//!
//! Two ideas carry most of the weight:
//!
//! - A **generation**. Every start mints a new one. A lease, a call and a
//!   reply all carry the generation they were minted under, and anything
//!   presented under a retired generation is refused. That is what makes a
//!   grant unable to survive a restart: it is not forgotten, it is *refused by
//!   name*, which a reader can see in a receipt.
//! - A **call ledger**. A call id reaches a terminal exactly once. The second
//!   terminal for one call is refused rather than folded in, because a tool
//!   that ran once and reported twice and a tool that ran twice are
//!   indistinguishable in an aggregate.

use crate::McpToolName;
use thiserror::Error;

/// How many calls one connection may have in flight, and how many completed
/// calls it remembers for duplicate detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionBounds {
    max_in_flight: usize,
    max_remembered: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SessionBoundsError {
    #[error("every session bound must be nonzero")]
    Zero,
    #[error("a connection must remember at least as many calls as it may run at once")]
    ForgetsInFlight,
}

impl SessionBounds {
    pub fn new(max_in_flight: usize, max_remembered: usize) -> Result<Self, SessionBoundsError> {
        if max_in_flight == 0 || max_remembered == 0 {
            return Err(SessionBoundsError::Zero);
        }
        // Remembering fewer calls than may be in flight would let a live call's
        // own terminal look like a duplicate, or let a real duplicate pass as
        // the first terminal it had already forgotten.
        if max_remembered < max_in_flight {
            return Err(SessionBoundsError::ForgetsInFlight);
        }
        Ok(Self {
            max_in_flight,
            max_remembered,
        })
    }

    pub fn conservative() -> Result<Self, SessionBoundsError> {
        Self::new(8, 512)
    }
}

/// Which start attempt a lease, call or reply belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(u64);

impl Generation {
    pub fn value(self) -> u64 {
        self.0
    }
}

/// Why a connection is not usable. These are deliberately separate variants:
/// collapsing them into one "unavailable" is what makes an operator unable to
/// tell a server that is misconfigured from one that is merely slow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusable {
    /// Declared with `disabled`/`enabled: false`. Never contacted.
    Disabled,
    /// Startup or a request passed its deadline. The server may be healthy and
    /// slow; nothing is known about whether it received the request.
    TimedOut,
    /// The transport closed under us. Distinct from a timeout: we know the
    /// channel is gone.
    Disconnected,
    /// The server was reached and answered in a way that cannot be used, or
    /// could not be started at all. Carries the reason it gave.
    Failed(String),
}

impl Unusable {
    /// The stable label a status projection and a receipt both use.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::TimedOut => "timed-out",
            Self::Disconnected => "disconnected",
            Self::Failed(_) => "failed",
        }
    }
}

/// The lifecycle position of one configured server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// Configured and not yet started. Declaring a server contacts nothing.
    Declared,
    /// A start is in progress under this generation; no tool may be called.
    Starting(Generation),
    /// Handshaken and discovered. This is the only state that may run a tool.
    Ready(Generation),
    /// Not usable. Carries the generation it failed under, so a receipt can say
    /// which attempt this was.
    Unusable(Generation, Unusable),
}

impl SessionState {
    pub fn generation(&self) -> Option<Generation> {
        match self {
            Self::Declared => None,
            Self::Starting(generation) | Self::Ready(generation) => Some(*generation),
            Self::Unusable(generation, _) => Some(*generation),
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionError {
    #[error("this connection exhausted its generation space")]
    GenerationExhausted,
    #[error("a start was requested while a start was already in progress")]
    AlreadyStarting,
    #[error("a start was requested while the connection was already ready")]
    AlreadyReady,
    #[error("the connection is not ready: it is {0}")]
    NotReady(&'static str),
    #[error("a lease minted under generation {minted} was presented under generation {current}")]
    RetiredGeneration { minted: u64, current: u64 },
    #[error("call {0} has already reached a terminal")]
    DuplicateTerminal(String),
    #[error("call {0} is not in flight")]
    UnknownCall(String),
    #[error("call {0} is already in flight")]
    RepeatedCall(String),
    #[error("this connection already has {limit} calls in flight")]
    InFlightBound { limit: usize },
    #[error("tool {0:?} is not in this connection's discovered catalog")]
    UndiscoveredTool(String),
}

/// A caller's identity for one tool call, valid only under one generation.
///
/// It is not `Clone`: a lease that could be copied could be presented twice,
/// which is the duplicate-execution defect the ledger below exists to catch,
/// re-created one layer lower.
#[derive(Debug, PartialEq, Eq)]
pub struct CallLease {
    generation: Generation,
    call: String,
    tool: McpToolName,
}

impl CallLease {
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn call(&self) -> &str {
        &self.call
    }
    pub fn tool(&self) -> &McpToolName {
        &self.tool
    }
}

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    /// The server returned a result the caller may continue from.
    Completed,
    /// The server reported the call itself failed. The call is finished; the
    /// connection is not necessarily unusable.
    Refused,
    /// The call passed its deadline. Nothing is known about whether the server
    /// ran it, which is why this is not `Refused`.
    TimedOut,
    /// The transport closed while this call was in flight.
    Disconnected,
}

/// One configured server's lifecycle and call ledger.
#[derive(Debug)]
pub struct McpSession {
    bounds: SessionBounds,
    state: SessionState,
    generations: u64,
    catalog: Vec<McpToolName>,
    in_flight: Vec<(String, McpToolName)>,
    settled: Vec<(String, CallOutcome)>,
}

impl McpSession {
    pub fn declared(bounds: SessionBounds) -> Self {
        Self {
            bounds,
            state: SessionState::Declared,
            generations: 0,
            catalog: Vec::new(),
            in_flight: Vec::new(),
            settled: Vec::new(),
        }
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// The discovered catalog. Empty until a start succeeds, and emptied again
    /// by every restart: a catalog is a fact about one live connection, not a
    /// cached property of the configuration.
    pub fn catalog(&self) -> &[McpToolName] {
        &self.catalog
    }

    /// Begin a start attempt, minting a fresh generation.
    ///
    /// A restart retires everything the previous generation held: the catalog
    /// is cleared and every in-flight call is settled as disconnected. Nothing
    /// is carried across, which is what makes a pre-restart lease unusable
    /// afterwards rather than quietly honoured.
    pub fn start(&mut self) -> Result<Generation, SessionError> {
        match &self.state {
            SessionState::Starting(_) => return Err(SessionError::AlreadyStarting),
            SessionState::Ready(_) => return Err(SessionError::AlreadyReady),
            SessionState::Declared | SessionState::Unusable(_, _) => {}
        }
        let generation = self
            .generations
            .checked_add(1)
            .ok_or(SessionError::GenerationExhausted)?;
        self.generations = generation;
        self.catalog.clear();
        let abandoned: Vec<String> = self.in_flight.drain(..).map(|(call, _)| call).collect();
        for call in abandoned {
            self.remember(call, CallOutcome::Disconnected);
        }
        let generation = Generation(generation);
        self.state = SessionState::Starting(generation);
        Ok(generation)
    }

    /// The start succeeded and discovery returned this catalog.
    pub fn ready(&mut self, catalog: Vec<McpToolName>) -> Result<Generation, SessionError> {
        let generation = match &self.state {
            SessionState::Starting(generation) => *generation,
            other => return Err(SessionError::NotReady(state_label(other))),
        };
        self.catalog = catalog;
        self.state = SessionState::Ready(generation);
        Ok(generation)
    }

    /// The connection became unusable, for a named and distinct reason.
    ///
    /// Every call in flight settles with the outcome that matches: a timeout
    /// settles calls as timed out, a disconnect as disconnected. Neither is
    /// reported as a refusal, because in both cases we do not know whether the
    /// server ran the call.
    pub fn unusable(&mut self, reason: Unusable) -> Generation {
        let generation = match self.state.generation() {
            Some(generation) => generation,
            None => {
                // A server declared `disabled` is unusable without ever having
                // been started; give it generation zero rather than inventing
                // a start that never happened.
                Generation(0)
            }
        };
        let outcome = match &reason {
            Unusable::TimedOut => CallOutcome::TimedOut,
            Unusable::Disabled | Unusable::Disconnected | Unusable::Failed(_) => {
                CallOutcome::Disconnected
            }
        };
        let abandoned: Vec<String> = self.in_flight.drain(..).map(|(call, _)| call).collect();
        for call in abandoned {
            self.remember(call, outcome);
        }
        self.catalog.clear();
        self.state = SessionState::Unusable(generation, reason);
        generation
    }

    /// Reserve one call. Only a ready connection, and only a discovered tool.
    pub fn begin_call(
        &mut self,
        call: &str,
        tool: &McpToolName,
    ) -> Result<CallLease, SessionError> {
        let generation = match &self.state {
            SessionState::Ready(generation) => *generation,
            other => return Err(SessionError::NotReady(state_label(other))),
        };
        if !self.catalog.contains(tool) {
            return Err(SessionError::UndiscoveredTool(tool.as_str().to_owned()));
        }
        if self.in_flight.iter().any(|(held, _)| held == call) {
            return Err(SessionError::RepeatedCall(call.to_owned()));
        }
        if self.settled.iter().any(|(held, _)| held == call) {
            return Err(SessionError::DuplicateTerminal(call.to_owned()));
        }
        if self.in_flight.len() >= self.bounds.max_in_flight {
            return Err(SessionError::InFlightBound {
                limit: self.bounds.max_in_flight,
            });
        }
        self.in_flight.push((call.to_owned(), tool.clone()));
        Ok(CallLease {
            generation,
            call: call.to_owned(),
            tool: tool.clone(),
        })
    }

    /// Settle one call, consuming its lease.
    ///
    /// The lease is taken by value, so the type system already prevents
    /// presenting it twice; the ledger check below is the belt for the case
    /// where a caller reconstructs a call id from a wire message instead.
    pub fn settle(&mut self, lease: CallLease, outcome: CallOutcome) -> Result<(), SessionError> {
        let current = self.state.generation().unwrap_or(Generation(0));
        if lease.generation != current {
            return Err(SessionError::RetiredGeneration {
                minted: lease.generation.value(),
                current: current.value(),
            });
        }
        let position = self
            .in_flight
            .iter()
            .position(|(held, _)| held == &lease.call)
            .ok_or_else(|| {
                if self.settled.iter().any(|(held, _)| held == &lease.call) {
                    SessionError::DuplicateTerminal(lease.call.clone())
                } else {
                    SessionError::UnknownCall(lease.call.clone())
                }
            })?;
        self.in_flight.remove(position);
        self.remember(lease.call, outcome);
        Ok(())
    }

    /// How a call ended, for a caller reconciling a late wire message against
    /// the ledger. `None` means this connection never settled that call.
    pub fn outcome(&self, call: &str) -> Option<CallOutcome> {
        self.settled
            .iter()
            .find(|(held, _)| held == call)
            .map(|(_, outcome)| *outcome)
    }

    fn remember(&mut self, call: String, outcome: CallOutcome) {
        if self.settled.iter().any(|(held, _)| held == &call) {
            return;
        }
        if self.settled.len() >= self.bounds.max_remembered {
            // Bounded memory: the oldest settled call is dropped. The bound is
            // required to exceed the in-flight bound, so no live call's own
            // terminal can be forgotten out from under it.
            self.settled.remove(0);
        }
        self.settled.push((call, outcome));
    }
}

fn state_label(state: &SessionState) -> &'static str {
    match state {
        SessionState::Declared => "declared",
        SessionState::Starting(_) => "starting",
        SessionState::Ready(_) => "ready",
        SessionState::Unusable(_, reason) => reason.label(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, SessionError>;

    fn tool(name: &str) -> McpToolName {
        match McpToolName::new(name) {
            Some(tool) => tool,
            None => panic!("fixture tool name {name:?} is invalid"),
        }
    }

    fn bounds() -> SessionBounds {
        match SessionBounds::conservative() {
            Ok(bounds) => bounds,
            Err(_) => panic!("the conservative bounds must be constructible"),
        }
    }

    fn ready_session() -> TestResult<McpSession> {
        let mut session = McpSession::declared(bounds());
        session.start()?;
        session.ready(vec![tool("fixture_read")])?;
        Ok(session)
    }

    #[test]
    fn declaring_a_server_contacts_nothing_and_a_start_makes_it_usable() -> TestResult {
        let mut session = McpSession::declared(bounds());
        assert_eq!(session.state(), &SessionState::Declared);
        assert!(
            session.catalog().is_empty(),
            "a declared server has no catalog because it has not been asked"
        );
        assert!(
            matches!(
                session.begin_call("call-1", &tool("fixture_read")),
                Err(SessionError::NotReady("declared"))
            ),
            "a declared server ran a tool"
        );
        let generation = session.start()?;
        assert_eq!(session.state(), &SessionState::Starting(generation));
        assert!(
            matches!(
                session.begin_call("call-1", &tool("fixture_read")),
                Err(SessionError::NotReady("starting"))
            ),
            "a tool ran before the handshake finished"
        );
        session.ready(vec![tool("fixture_read")])?;
        assert!(session.state().is_ready());
        assert!(matches!(session.start(), Err(SessionError::AlreadyReady)));
        Ok(())
    }

    #[test]
    fn timed_out_disconnected_and_failed_stay_distinct_actionable_states() -> TestResult {
        for (reason, label) in [
            (Unusable::TimedOut, "timed-out"),
            (Unusable::Disconnected, "disconnected"),
            (Unusable::Failed("bad handshake".to_owned()), "failed"),
            (Unusable::Disabled, "disabled"),
        ] {
            let mut session = ready_session()?;
            let generation = session.unusable(reason.clone());
            assert_eq!(
                session.state(),
                &SessionState::Unusable(generation, reason.clone()),
                "{label} did not survive as its own state"
            );
            assert_eq!(reason.label(), label);
            assert!(
                session.catalog().is_empty(),
                "{label} left a catalog behind for a connection that is gone"
            );
            assert!(matches!(
                session.begin_call("call-1", &tool("fixture_read")),
                Err(SessionError::NotReady(named)) if named == label
            ));
        }
        Ok(())
    }

    #[test]
    fn a_call_reaches_a_terminal_exactly_once() -> TestResult {
        let mut session = ready_session()?;
        let lease = session.begin_call("call-1", &tool("fixture_read"))?;
        session.settle(lease, CallOutcome::Completed)?;
        assert_eq!(session.outcome("call-1"), Some(CallOutcome::Completed));
        // The same call id cannot be started again, so a retry that reuses an
        // id cannot execute the tool a second time.
        assert!(
            matches!(
                session.begin_call("call-1", &tool("fixture_read")),
                Err(SessionError::DuplicateTerminal(_))
            ),
            "a settled call was allowed to run again under its own id"
        );
        // A second in-flight reservation for one id is refused too.
        let live = session.begin_call("call-2", &tool("fixture_read"))?;
        assert!(matches!(
            session.begin_call("call-2", &tool("fixture_read")),
            Err(SessionError::RepeatedCall(_))
        ));
        session.settle(live, CallOutcome::Refused)?;
        assert_eq!(session.outcome("call-2"), Some(CallOutcome::Refused));
        assert_eq!(session.outcome("never-ran"), None);
        Ok(())
    }

    #[test]
    fn a_lease_minted_before_a_restart_is_refused_by_name_after_it() -> TestResult {
        let mut session = ready_session()?;
        let stale = session.begin_call("call-1", &tool("fixture_read"))?;
        assert_eq!(stale.generation().value(), 1);

        session.unusable(Unusable::Disconnected);
        let restarted = session.start()?;
        assert_eq!(restarted.value(), 2, "a restart mints a new generation");
        session.ready(vec![tool("fixture_read")])?;

        assert!(
            matches!(
                session.settle(stale, CallOutcome::Completed),
                Err(SessionError::RetiredGeneration {
                    minted: 1,
                    current: 2
                })
            ),
            "a grant from before the restart was honoured after it"
        );
        // The disconnect settled the abandoned call as disconnected, and the
        // restart did not resurrect it as completable.
        assert_eq!(session.outcome("call-1"), Some(CallOutcome::Disconnected));
        Ok(())
    }

    #[test]
    fn a_restart_discards_the_previous_catalog_rather_than_reusing_it() -> TestResult {
        let mut session = ready_session()?;
        assert_eq!(session.catalog(), &[tool("fixture_read")]);
        session.unusable(Unusable::Disconnected);
        session.start()?;
        assert!(
            session.catalog().is_empty(),
            "a catalog outlived the connection it described"
        );
        // The new connection advertises a different tool; the old one is gone.
        session.ready(vec![tool("fixture_write")])?;
        assert!(
            matches!(
                session.begin_call("call-9", &tool("fixture_read")),
                Err(SessionError::UndiscoveredTool(_))
            ),
            "a tool from the retired catalog was still callable"
        );
        let lease = session.begin_call("call-9", &tool("fixture_write"))?;
        session.settle(lease, CallOutcome::Completed)?;
        Ok(())
    }

    #[test]
    fn an_undiscovered_tool_and_an_unbounded_call_load_are_both_refused() -> TestResult {
        let mut session = ready_session()?;
        assert!(matches!(
            session.begin_call("call-x", &tool("never_advertised")),
            Err(SessionError::UndiscoveredTool(_))
        ));
        let small = SessionBounds::new(1, 4).map_err(|_| SessionError::GenerationExhausted)?;
        let mut session = McpSession::declared(small);
        session.start()?;
        session.ready(vec![tool("fixture_read")])?;
        let held = session.begin_call("call-1", &tool("fixture_read"))?;
        assert!(matches!(
            session.begin_call("call-2", &tool("fixture_read")),
            Err(SessionError::InFlightBound { limit: 1 })
        ));
        session.settle(held, CallOutcome::Completed)?;
        let lease = session.begin_call("call-2", &tool("fixture_read"))?;
        session.settle(lease, CallOutcome::TimedOut)?;
        assert_eq!(session.outcome("call-2"), Some(CallOutcome::TimedOut));
        Ok(())
    }

    #[test]
    fn the_bounds_refuse_a_memory_that_could_forget_a_live_call() {
        assert!(SessionBounds::new(0, 1).is_err());
        assert!(SessionBounds::new(1, 0).is_err());
        assert!(
            matches!(
                SessionBounds::new(8, 4),
                Err(SessionBoundsError::ForgetsInFlight)
            ),
            "a ledger smaller than the in-flight bound could forget a live call"
        );
        assert!(SessionBounds::new(8, 8).is_ok());
    }
}
