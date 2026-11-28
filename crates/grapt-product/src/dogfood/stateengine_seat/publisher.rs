//! Apply seat commands to the host, degrading without ever failing a turn.
//! The seat's StateEngine session comes from its latest activity receipt; a
//! turn that finishes while no session is known is skipped and reported.
use grapt_stateengine_ipc::{Activity, AgentId, SessionId, Sha256Hex};

/// What the observer asks the publisher to do.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Command {
    Activity(Activity),
    Turn(SeatTurn),
}

/// One succeeded execution, as the hub committed it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SeatTurn {
    /// The Grapt session the turn belongs to.
    pub(super) conversation: String,
    /// The execution, which keys the turn and its retries.
    pub(super) vendor_turn: String,
    /// When the execution's success was committed, so a retry repeats it.
    pub(super) occurred_at_us: i64,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) body: Body,
    /// SHA-256 of the complete body.
    pub(super) digest: Sha256Hex,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Body {
    Whole(String),
    /// Too large or elided: only its size travels.
    Omitted(u64),
}

/// The seat's agent and live session, as its latest activity receipt named them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SeatSession {
    pub(super) agent: AgentId,
    pub(super) session: SessionId,
}

/// The StateEngine side of a seat. Errors are reasons, never panics.
pub(super) trait SeatHost {
    /// Report the seat's state; the receipt names its agent and session.
    async fn activity(&mut self, state: Activity) -> Result<SeatSession, String>;
    async fn turn(&mut self, seat: &SeatSession, turn: &SeatTurn) -> Result<(), String>;
}

pub(super) struct Publisher<H> {
    host: H,
    session: Option<SeatSession>,
    degraded: bool,
}

impl<H: SeatHost> Publisher<H> {
    pub(super) fn new(host: H) -> Self {
        Self {
            host,
            session: None,
            degraded: false,
        }
    }

    pub(super) async fn apply(&mut self, command: Command) {
        let outcome = match &command {
            Command::Activity(state) => match self.host.activity(*state).await {
                Ok(session) => {
                    self.session = Some(session);
                    Ok(())
                }
                Err(reason) => {
                    self.session = None;
                    Err(reason)
                }
            },
            Command::Turn(turn) => match &self.session {
                Some(session) => self.host.turn(session, turn).await,
                None => Err(
                    "no StateEngine seat session is known; the turn was not published".to_owned(),
                ),
            },
        };
        match outcome {
            Ok(()) if self.degraded => {
                self.degraded = false;
                eprintln!("stateengine seat recovered");
            }
            Ok(()) => {}
            Err(reason) if !self.degraded => {
                self.degraded = true;
                super::warn("host-unavailable", &reason);
            }
            Err(_) => {}
        }
    }

    #[cfg(test)]
    pub(super) fn host(&self) -> &H {
        &self.host
    }
}
