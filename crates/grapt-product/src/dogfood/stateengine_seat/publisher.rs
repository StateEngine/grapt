//! Apply seat commands to the host, degrading without ever failing a turn.
//! The seat's busy or idle state is shared rather than queued, so a full queue
//! or a refused report never loses it: the publisher reports the latest state
//! until the host holds it. The seat's StateEngine session comes from the
//! receipt of that report and is forgotten after any failure, since a failed
//! call may mean a new connection or a restarted host.
use grapt_stateengine_ipc::{Activity, AgentId, SessionId, Sha256Hex};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::time::Instant;

/// The first wait after a failure; each further failure doubles it.
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(60);

/// What the observer asks the publisher to do.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Command {
    /// Bring the host up to the latest desired state.
    Reconcile,
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

/// The state the seat should show, as the observer last saw it.
#[derive(Debug, Default)]
pub(super) struct Desired(AtomicU8);

impl Desired {
    pub(super) fn set(&self, state: Activity) {
        let value = match state {
            Activity::Busy => 1,
            Activity::Idle => 2,
        };
        self.0.store(value, Ordering::SeqCst);
    }

    pub(super) fn get(&self) -> Option<Activity> {
        match self.0.load(Ordering::SeqCst) {
            1 => Some(Activity::Busy),
            2 => Some(Activity::Idle),
            _ => None,
        }
    }
}

pub(super) struct Publisher<H> {
    host: H,
    desired: std::sync::Arc<Desired>,
    /// The state the host acknowledged on the current session.
    reported: Option<(Activity, SeatSession)>,
    retry: Option<(Instant, Duration)>,
    degraded: bool,
}

impl<H: SeatHost> Publisher<H> {
    pub(super) fn new(host: H, desired: std::sync::Arc<Desired>) -> Self {
        Self {
            host,
            desired,
            reported: None,
            retry: None,
            degraded: false,
        }
    }

    pub(super) async fn apply(&mut self, command: Command) {
        let outcome = match command {
            Command::Reconcile => self.reconcile().await.map(drop),
            Command::Turn(turn) => self.publish(&turn).await,
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

    /// How long until the host should be asked again, while it does not yet
    /// hold the desired state.
    pub(super) fn retry_in(&self) -> Option<Duration> {
        let desired = self.desired.get()?;
        if self.reported.as_ref().map(|(state, _)| *state) == Some(desired) {
            return None;
        }
        let now = Instant::now();
        Some(match self.retry {
            Some((at, _)) if at > now => at - now,
            _ => Duration::ZERO,
        })
    }

    /// Ask once more now, whatever the wait: the service is stopping.
    pub(super) async fn finish(&mut self) {
        self.retry = None;
        self.apply(Command::Reconcile).await;
    }

    /// Report the desired state unless the host already holds it, and name
    /// the session it belongs to.
    async fn reconcile(&mut self) -> Result<Option<SeatSession>, String> {
        let Some(desired) = self.desired.get() else {
            return Ok(None);
        };
        if let Some((state, session)) = &self.reported {
            if *state == desired {
                return Ok(Some(session.clone()));
            }
        }
        self.waited()?;
        let outcome = self.host.activity(desired).await;
        let session = self.settle(outcome)?;
        self.reported = Some((desired, session.clone()));
        Ok(Some(session))
    }

    /// A failed publication may have lost its connection and session, so it
    /// is tried once more on a freshly reported one; its key makes a repeat
    /// safe.
    async fn publish(&mut self, turn: &SeatTurn) -> Result<(), String> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let session = self.reconcile().await?.ok_or(
                "no StateEngine seat session is known; the turn was not published".to_owned(),
            )?;
            self.waited()?;
            let outcome = self.host.turn(&session, turn).await;
            match self.settle(outcome) {
                Ok(()) => return Ok(()),
                Err(reason) if attempts == 2 => return Err(reason),
                Err(_) => self.retry = None,
            }
        }
    }

    fn waited(&self) -> Result<(), String> {
        match self.retry {
            Some((at, _)) if at > Instant::now() => {
                Err("StateEngine refused recently; this report was skipped".to_owned())
            }
            _ => Ok(()),
        }
    }

    /// Record a call's outcome: a failure forgets the session and backs off.
    fn settle<T>(&mut self, outcome: Result<T, String>) -> Result<T, String> {
        match outcome {
            Ok(value) => {
                self.retry = None;
                Ok(value)
            }
            Err(reason) => {
                self.reported = None;
                let wait = self
                    .retry
                    .map_or(FIRST_RETRY, |(_, wait)| (wait * 2).min(LAST_RETRY));
                self.retry = Some((Instant::now() + wait, wait));
                Err(reason)
            }
        }
    }

    #[cfg(test)]
    pub(super) fn host(&self) -> &H {
        &self.host
    }

    #[cfg(test)]
    pub(super) fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }
}
