//! The hand-off between the protocol thread and the UI. Neither side waits on
//! the other: the backend builds a view only into an empty slot, and the UI
//! takes whatever is there.
//!
//! The newest view wins without any merge. The transcript lives in client
//! state and its row diff is taken against what the UI last consumed, so a view
//! the backend skips while the slot is full loses nothing: the next view it
//! builds carries every change since the one the UI took.
use super::{Failure, Update};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    mpsc::SyncSender,
    Arc, Mutex, PoisonError,
};
use std::time::Duration;
use tokio::time::Instant;

/// How long one backend pass may run before the footer reports lag.
pub const LAG_AFTER: Duration = Duration::from_secs(1);

/// The most quit requests waiting at once: the UI's own, a console close and
/// a panic close.
const QUITS: usize = 4;

/// The acknowledgement a quit request waits on.
pub type QuitReply = SyncSender<Result<(), String>>;

/// Commands waiting at the backend above this depth are shown with the lag.
pub const SHOWN_DEPTH: usize = 8;

/// The identity a command carries from the UI's send to its result. Minted
/// once, by `Handoff::envelope`, and travelling with the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommandId(u64);

#[cfg(test)]
impl CommandId {
    /// A fixed identity for tests that drive one owner directly.
    pub(crate) const TEST: Self = Self(0);
}

#[derive(Clone)]
pub struct Handoff {
    slot: Arc<Mutex<Slot>>,
    liveness: Arc<Liveness>,
}

#[derive(Default)]
struct Slot {
    update: Option<Update>,
    failure: Option<Failure>,
}

/// The backend loop's own liveness: when the pass now running began, and how
/// many commands the UI has handed it that it has not yet received.
struct Liveness {
    base: Instant,
    /// Milliseconds after `base` plus one; zero while the loop waits.
    pass_began: AtomicU64,
    intake: AtomicUsize,
    /// A model refresh the UI asked for on its own: requests coalesce here
    /// instead of queueing behind a stuck backend.
    refresh: AtomicBool,
    /// Quit requests, read apart from the command intake so none waits
    /// behind it.
    quits: Mutex<Vec<QuitReply>>,
    next_command: AtomicU64,
}

/// The connection as the footer names it, from the client's own state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Link {
    #[default]
    Connected,
    Reconnecting(Option<u32>),
    Offline,
}

impl Default for Handoff {
    fn default() -> Self {
        Self {
            slot: Arc::default(),
            liveness: Arc::new(Liveness {
                base: Instant::now(),
                pass_began: AtomicU64::new(0),
                intake: AtomicUsize::new(0),
                refresh: AtomicBool::new(false),
                quits: Mutex::default(),
                next_command: AtomicU64::new(1),
            }),
        }
    }
}

impl Handoff {
    fn slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the UI took the last view. Only the backend fills the slot, so
    /// a vacant slot stays vacant until the backend's next `put`.
    pub fn vacant(&self) -> bool {
        self.slot().update.is_none()
    }

    /// Hand over a view built for a vacant slot. An occupied slot refuses it
    /// whole rather than drop either view.
    pub fn put(&self, update: Update) -> Result<(), String> {
        let mut slot = self.slot();
        if slot.update.is_some() {
            return Err("the view hand-off was still occupied".into());
        }
        slot.update = Some(update);
        Ok(())
    }

    /// The backend ended. Any view still in the slot is delivered first.
    pub fn fail(&self, failure: Failure) {
        self.slot().failure = Some(failure);
    }

    /// The UI's side: the pending view, then a failure once no view is left.
    pub fn take(&self) -> Option<Result<Update, Failure>> {
        let mut slot = self.slot();
        match slot.update.take() {
            Some(update) => Some(Ok(update)),
            None => slot.failure.take().map(Err),
        }
    }

    /// The loop starts a pass.
    pub fn working(&self) {
        let elapsed = self.liveness.base.elapsed().as_millis();
        let stamp = u64::try_from(elapsed).unwrap_or(u64::MAX).saturating_add(1);
        self.liveness.pass_began.store(stamp, Ordering::Release);
    }

    /// The loop waits for work.
    pub fn waiting(&self) {
        self.liveness.pass_began.store(0, Ordering::Release);
    }

    /// How long the running pass has taken, or `None` while the loop waits.
    pub fn pass_age(&self) -> Option<Duration> {
        let began = self
            .liveness
            .pass_began
            .load(Ordering::Acquire)
            .checked_sub(1)?;
        let elapsed = self.liveness.base.elapsed();
        Some(elapsed.saturating_sub(Duration::from_millis(began)))
    }

    /// A command with its newly minted identity, ready for the channel.
    pub fn envelope(&self, command: super::Command) -> (CommandId, super::Command) {
        let id = self.liveness.next_command.fetch_add(1, Ordering::AcqRel);
        (CommandId(id), command)
    }

    /// The UI handed the backend a command.
    pub fn handed(&self) {
        self.liveness.intake.fetch_add(1, Ordering::AcqRel);
    }

    /// The backend received one.
    pub fn received(&self) {
        let _ = self
            .liveness
            .intake
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |depth| {
                depth.checked_sub(1)
            });
    }

    /// Ask for a model refresh; repeated asks before the backend sees one
    /// are one refresh.
    pub fn request_refresh(&self) {
        self.liveness.refresh.store(true, Ordering::Release);
    }

    /// The backend's side of the refresh flag, cleared as it is read.
    pub fn refresh_requested(&self) -> bool {
        self.liveness.refresh.swap(false, Ordering::AcqRel)
    }

    /// Ask the backend to quit, ahead of every queued command.
    pub fn request_quit(&self, reply: QuitReply) -> Result<(), String> {
        let mut quits = self
            .liveness
            .quits
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if quits.len() >= QUITS {
            return Err("A quit is already waiting for the service".into());
        }
        quits.push(reply);
        Ok(())
    }

    /// The backend's side of the quit requests, emptied as they are read.
    pub fn take_quits(&self) -> Vec<QuitReply> {
        std::mem::take(
            &mut *self
                .liveness
                .quits
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Commands handed over and not yet received.
    pub fn intake(&self) -> usize {
        self.liveness.intake.load(Ordering::Acquire)
    }
}

impl Link {
    pub fn of(
        status: grapt_client::client::ConnectionStatus,
        state: &grapt_client::supervise::Connection,
    ) -> Self {
        use grapt_client::{client::ConnectionStatus, supervise::Connection};
        match (status, state) {
            (ConnectionStatus::Live, _) => Self::Connected,
            (_, Connection::Refused { .. }) => Self::Offline,
            (_, Connection::Replacing { attempt } | Connection::Disconnected { attempt, .. }) => {
                Self::Reconnecting(Some(*attempt))
            }
            (ConnectionStatus::Reconnecting, _) => Self::Reconnecting(None),
            (ConnectionStatus::Offline, _) => Self::Offline,
        }
    }
}

/// The footer's heartbeat: the connection first, then the backend's lag.
pub fn heartbeat(link: Link, pass_age: Option<Duration>, intake: usize) -> String {
    match link {
        Link::Reconnecting(Some(attempt)) => format!("reconnecting {attempt}"),
        Link::Reconnecting(None) => "reconnecting".into(),
        Link::Offline => "offline".into(),
        Link::Connected if pass_age.is_some_and(|age| age >= LAG_AFTER) => {
            if intake > SHOWN_DEPTH {
                format!("lagged · {intake} commands waiting")
            } else {
                "lagged".into()
            }
        }
        Link::Connected => "connected".into(),
    }
}
