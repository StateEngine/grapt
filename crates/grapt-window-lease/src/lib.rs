//! "Close means closed": when the last window for a Session goes away.
//!
//! Law source: a Session must never keep running and spending tokens after the
//! last window that can watch it is gone. A quit or a terminal close is an
//! explicit stop the window sends before it exits. The case this module exists
//! for is the one no window can report: a crash, a hangup or a lost connection
//! announces nothing, so its subscription ending is the only truthful signal,
//! and a brief grace lets a reconnect that is already in flight keep the turn.
//!
//! The service owns this answer, not the window: a window's own idea of its
//! state is advisory. Several windows may watch one Session, so only the LAST
//! one going is a close; every window therefore carries its own identity here.
//!
//! A Session launched to run without a window never registers a window at all,
//! so it never starts this grace.
//!
//! This module decides *when* the last window is gone. It starts no timer: the
//! caller drives it with a monotonic reading, which keeps the decision testable
//! without a real clock.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use grapt_types::SessionId;

/// How long the turn survives after its last window disappears without saying
/// so.
///
/// A lease is connectivity, not intent, so a sleeping laptop, a stalled service
/// or a reconnect already in flight all look like absence and none of them is a
/// close. Remote windows sit behind tunnels, so this is generous rather than
/// tight; it is still short enough that an abandoned turn is not a token sink.
/// 30 s is the default, and a deployment may configure it.
pub const DEFAULT_CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// The longest gap the service tolerates between two readings of its own loop
/// before the gap counts as a stall rather than as absence.
///
/// A grace is measured on a monotonic clock, which is immune to wall-clock
/// jumps but not to the loop not running. When the loop is blocked for longer
/// than this, the elapsed time belongs to the stall: `CloseRegistry::rebase`
/// restarts every running grace from the moment the loop recovered, so a stall
/// cannot spend the grace that a usable window was still going to use.
pub const STALL_TICK: std::time::Duration = std::time::Duration::from_secs(5);

/// The longest name a window may use for itself.
///
/// A window's name is compared, never parsed, and never becomes a durable fact,
/// so this bound only keeps one subscription's identity small.
pub const MAX_WINDOW_NAME_BYTES: usize = 128;

/// One window's own name for itself.
///
/// A window chooses this and sends the same name on its event subscription and
/// on its explicit release, so the service can tell which of several windows is
/// leaving. It names an attachment, not a Session or an execution: nothing
/// persists it, and an unknown name simply has no window here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(String);

impl WindowId {
    /// Bounded and non-empty: a name must identify exactly one live window.
    pub fn try_new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() || raw.len() > MAX_WINDOW_NAME_BYTES {
            return None;
        }
        Some(Self(raw))
    }

    /// The service's own name for the next window it accepts.
    ///
    /// A decimal ordinal renders to at most twenty digits, so the result is
    /// never empty and never over the bound: this cannot fail.
    pub fn from_ordinal(ordinal: u64) -> Self {
        Self(format!("w{ordinal}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One Session's windows and, when it has none, when it lost its last window.
#[derive(Debug, Default)]
struct SessionWindows {
    windows: BTreeSet<WindowId>,
    /// Set when the last window goes; cleared by a window that returns in time.
    absent_since: Option<tokio::time::Instant>,
}

/// Tracks which Sessions still have a window, and decides when a turn must
/// stop.
///
/// Deliberately synchronous and time-injected: it holds no lock across an
/// await and starts no task.
#[derive(Debug)]
pub struct CloseRegistry {
    sessions: BTreeMap<SessionId, SessionWindows>,
    grace: std::time::Duration,
}

impl Default for CloseRegistry {
    fn default() -> Self {
        Self::with_grace(DEFAULT_CLOSE_GRACE)
    }
}

impl CloseRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// One registry whose silent-loss grace is the deployment's own.
    pub fn with_grace(grace: std::time::Duration) -> Self {
        Self {
            sessions: BTreeMap::new(),
            grace,
        }
    }

    /// The grace this registry applies to a silent loss.
    pub fn grace(&self) -> std::time::Duration {
        self.grace
    }

    /// Record that one window is watching `session`.
    ///
    /// The same name attaching twice is one window reconnecting: it replaces
    /// its earlier claim rather than counting twice, so a resumed stream cannot
    /// keep a turn alive after its socket is gone. A window that returns within
    /// the grace cancels the pending stop, whether or not it resumed a stream.
    pub fn attach_window(&mut self, session: &SessionId, window: WindowId) {
        let held = self.sessions.entry(session.clone()).or_default();
        held.windows.insert(window);
        // A window is present again, so nothing is waiting to expire.
        held.absent_since = None;
    }

    /// Record that one window's subscription ended without saying why.
    ///
    /// Another window still watching is not a stop. Losing the last one starts
    /// the grace, which a returning window cancels.
    pub fn detach_window(
        &mut self,
        session: &SessionId,
        window: &WindowId,
        now: tokio::time::Instant,
    ) {
        let Some(held) = self.sessions.get_mut(session) else {
            return;
        };
        held.windows.remove(window);
        if held.windows.is_empty() {
            held.absent_since.get_or_insert(now);
        }
    }

    /// Detach because the Session is being handed to another window, so the
    /// replacement has `handover` to arrive instead of racing the silent-loss
    /// grace.
    ///
    /// Unlike a silent loss, a handover never ends in a stop by itself: it only
    /// holds the Session unwatched for longer, so a slower upgrade still finds
    /// its turn. A replacement that attaches cancels it; an absent handover
    /// returns the Session to the ordinary grace rather than stopping at once.
    pub fn detach_for_handover(
        &mut self,
        session: &SessionId,
        window: &WindowId,
        now: tokio::time::Instant,
        handover: std::time::Duration,
    ) {
        let Some(held) = self.sessions.get_mut(session) else {
            return;
        };
        held.windows.remove(window);
        if held.windows.is_empty() {
            held.absent_since = Some(now + handover);
        }
    }

    /// The Session whose grace has expired at `now`, if any, already forgotten.
    ///
    /// Every unobserved Session is considered, not just the first: a Session
    /// that lost a window a moment ago must not hide one whose grace has
    /// already run out.
    pub fn expired(&mut self, now: tokio::time::Instant) -> Option<SessionId> {
        let expired = self
            .sessions
            .iter()
            .find(|(_, held)| {
                held.windows.is_empty()
                    && held
                        .absent_since
                        .is_some_and(|since| now.saturating_duration_since(since) >= self.grace)
            })
            .map(|(session, _)| session.clone())?;
        self.sessions.remove(&expired);
        Some(expired)
    }

    /// The next moment a grace expires, so a deadline owner can sleep exactly
    /// that long instead of polling.
    ///
    /// `None` means nothing is waiting to expire, so it can sleep until a
    /// window changes.
    pub fn next_deadline(&self, now: tokio::time::Instant) -> Option<tokio::time::Instant> {
        self.sessions
            .values()
            .filter(|held| held.windows.is_empty())
            .filter_map(|held| held.absent_since)
            .map(|since| since + self.grace)
            .filter(|deadline| *deadline > now)
            .min()
    }

    /// Restart every running grace from `now`, because the service's own loop
    /// was blocked and that time was the stall's, not the window's absence.
    ///
    /// Without this, a stall longer than the grace would stop turns whose
    /// windows were about to return as soon as the loop unblocked. Returns how
    /// many graces restarted, so the caller can account for them.
    pub fn rebase(&mut self, now: tokio::time::Instant) -> usize {
        let mut restarted = 0;
        for held in self.sessions.values_mut() {
            if held.windows.is_empty() && held.absent_since.is_some() {
                held.absent_since = Some(now);
                restarted += 1;
            }
        }
        restarted
    }
}

/// The hub's shared window registry: which windows are attached, the name of
/// the next one, and the signal a deadline owner waits on.
///
/// This is the whole of the hub's part in "close means closed". Splitting it
/// from the frame fan-out keeps each one's reason to change separate: the
/// registry changes when the *rule* changes, the fan-out when *frames* change.
pub struct WindowWatch {
    registry: Mutex<CloseRegistry>,
    next_ordinal: AtomicU64,
    changed: tokio::sync::watch::Sender<()>,
}

impl core::fmt::Debug for WindowWatch {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("WindowWatch")
            .finish_non_exhaustive()
    }
}

impl WindowWatch {
    pub fn new() -> Self {
        Self::with_grace(DEFAULT_CLOSE_GRACE)
    }

    /// The same watch with a deployment's own silent-loss grace.
    pub fn with_grace(grace: std::time::Duration) -> Self {
        Self {
            registry: Mutex::new(CloseRegistry::with_grace(grace)),
            next_ordinal: AtomicU64::new(0),
            changed: tokio::sync::watch::channel(()).0,
        }
    }

    /// Register one window on `session`, returning its claim.
    ///
    /// The claim releases the window on drop, and this process names it: two
    /// windows on one Session must not collapse into one claim, or closing
    /// either would look like closing both.
    pub fn attach(self: &Arc<Self>, session: &SessionId) -> WindowClaim {
        let window = WindowId::from_ordinal(self.next_ordinal.fetch_add(1, Ordering::Relaxed));
        self.with_registry(|registry| registry.attach_window(session, window.clone()));
        self.changed.send_replace(());
        WindowClaim {
            session: session.clone(),
            window,
            watch: Arc::clone(self),
        }
    }

    /// Every Session whose grace has expired at `now`, already forgotten.
    ///
    /// The deadline owner drains this and stops each turn.
    pub fn expired(&self, now: tokio::time::Instant) -> Vec<SessionId> {
        let mut expired = Vec::new();
        self.with_registry(|registry| {
            while let Some(session) = registry.expired(now) {
                expired.push(session);
            }
        });
        expired
    }

    /// The next moment a grace expires, so a deadline owner can sleep exactly
    /// that long instead of polling.
    pub fn next_deadline(&self, now: tokio::time::Instant) -> Option<tokio::time::Instant> {
        self.with_registry(|registry| registry.next_deadline(now))
    }

    /// Restart every running grace from `now` after the service itself stalled.
    ///
    /// The registry applies the same reading to every Session, so one blocked
    /// loop cannot spend the grace of turns whose windows were coming back.
    /// Returns how many graces restarted.
    pub fn rebase(&self, now: tokio::time::Instant) -> usize {
        let restarted = self.with_registry(|registry| registry.rebase(now));
        if restarted > 0 {
            self.changed.send_replace(());
        }
        restarted
    }

    /// Every Session this registry holds with no window, for diagnostics and
    /// for a test that needs to see the loss before its grace expires.
    pub fn unwatched(&self) -> Vec<SessionId> {
        self.with_registry(|registry| {
            registry
                .sessions
                .iter()
                .filter(|(_, held)| held.windows.is_empty())
                .map(|(session, _)| session.clone())
                .collect()
        })
    }

    /// Whether any window currently holds a lease on `session`.
    ///
    /// A Session this registry has never seen has no window, which is the
    /// truthful answer: absence from the registry is absence of a claim.
    pub fn has_window(&self, session: &SessionId) -> bool {
        self.with_registry(|registry| {
            registry
                .sessions
                .get(session)
                .is_some_and(|held| !held.windows.is_empty())
        })
    }

    /// Wake when a window attaches or detaches. The receiver starts from the
    /// current state, so a deadline owner marks it seen before computing its
    /// first deadline.
    pub fn changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.changed.subscribe()
    }

    /// Run one registry operation under the lock, taking a poisoned lock rather
    /// than losing the window that was being recorded.
    fn with_registry<T>(&self, operate: impl FnOnce(&mut CloseRegistry) -> T) -> T {
        let mut registry = match self.registry.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        operate(&mut registry)
    }
}

impl Default for WindowWatch {
    fn default() -> Self {
        Self::new()
    }
}

/// One HTTP window's claim on a Session, released when its stream ends.
///
/// Dropping this is what tells the service the window is gone, so the body that
/// holds it never reads it.
pub struct WindowClaim {
    session: SessionId,
    window: WindowId,
    watch: Arc<WindowWatch>,
}

impl core::fmt::Debug for WindowClaim {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("WindowClaim")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl Drop for WindowClaim {
    fn drop(&mut self) {
        let now = tokio::time::Instant::now();
        self.watch.with_registry(|registry| {
            registry.detach_window(&self.session, &self.window, now);
        });
        // Wake the deadline owner: it may now owe a stop after the grace.
        self.watch.changed.send_replace(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn session(id: &str) -> Result<SessionId> {
        id.parse().map_err(|_| "session id".into())
    }

    fn window(name: &str) -> Result<WindowId> {
        WindowId::try_new(name).ok_or_else(|| "window name".into())
    }

    #[test]
    fn a_configured_grace_replaces_the_default() -> Result {
        let one = session("ses_close_configured")?;
        let only = window("window-configured")?;
        let configured = std::time::Duration::from_secs(300);
        let mut registry = CloseRegistry::with_grace(configured);
        let start = tokio::time::Instant::now();

        assert_eq!(registry.grace(), configured);
        registry.attach_window(&one, only.clone());
        registry.detach_window(&one, &only, start);
        assert!(
            registry.expired(start + DEFAULT_CLOSE_GRACE).is_none(),
            "the shorter default must not override a longer configured grace"
        );
        assert_eq!(
            registry.expired(start + configured),
            Some(one.clone()),
            "the configured grace is what expires"
        );
        Ok(())
    }

    #[test]
    fn a_handover_holds_the_session_for_its_replacement() -> Result {
        let one = session("ses_close_handover")?;
        let only = window("window-handover")?;
        let replacement = window("window-replacement")?;
        let mut registry = CloseRegistry::new();
        let start = tokio::time::Instant::now();
        let handover = std::time::Duration::from_secs(60);

        registry.attach_window(&one, only.clone());
        registry.detach_for_handover(&one, &only, start, handover);
        assert!(
            registry.expired(start + DEFAULT_CLOSE_GRACE).is_none(),
            "a handover must not race the silent-loss grace"
        );
        // A replacement that arrives inside the handover keeps the turn, and
        // its own attach cancels what the handover was holding.
        registry.attach_window(&one, replacement);
        assert!(registry.next_deadline(start + handover).is_none());
        assert!(registry.expired(start + handover * 2).is_none());
        Ok(())
    }

    #[test]
    fn an_absent_handover_falls_back_to_the_silent_loss_grace() -> Result {
        let one = session("ses_close_handover_absent")?;
        let only = window("window-handover-absent")?;
        let mut registry = CloseRegistry::new();
        let start = tokio::time::Instant::now();
        let handover = std::time::Duration::from_secs(60);

        registry.attach_window(&one, only.clone());
        registry.detach_for_handover(&one, &only, start, handover);
        // No replacement ever arrives: the Session is not held forever, it is
        // stopped once the handover window and the ordinary grace are both
        // spent, so an abandoned upgrade does not run unseen.
        assert!(
            registry.expired(start + handover).is_none(),
            "the handover window itself is not a stop"
        );
        assert_eq!(
            registry.expired(start + handover + DEFAULT_CLOSE_GRACE),
            Some(one.clone()),
            "an absent replacement stops the turn after the grace"
        );
        Ok(())
    }

    #[test]
    fn a_stall_restarts_a_running_grace_instead_of_spending_it() -> Result {
        let one = session("ses_close_stall")?;
        let only = window("window-stall")?;
        let mut registry = CloseRegistry::new();
        let start = tokio::time::Instant::now();

        registry.attach_window(&one, only.clone());
        registry.detach_window(&one, &only, start);
        // The service loop was blocked well past the grace. Without a rebase the
        // first reading alone would stop a turn whose window may be returning.
        let recovered = start + DEFAULT_CLOSE_GRACE * 3;
        assert_eq!(registry.rebase(recovered), 1);
        assert!(
            registry.expired(recovered).is_none(),
            "a stall within the service must not stop a turn"
        );
        assert_eq!(
            registry.expired(recovered + DEFAULT_CLOSE_GRACE),
            Some(one.clone()),
            "an absence after the stall still stops the turn"
        );
        Ok(())
    }

    #[test]
    fn a_rebase_leaves_a_watched_session_and_its_deadline_alone() -> Result {
        let watched = session("ses_close_watched_again")?;
        let window = window("window-returned")?;
        let mut registry = CloseRegistry::new();
        let start = tokio::time::Instant::now();

        registry.attach_window(&watched, window);
        assert_eq!(registry.rebase(start), 0, "a watched Session owes no grace");
        assert!(registry.next_deadline(start).is_none());
        Ok(())
    }

    #[test]
    fn losing_the_only_window_starts_the_grace_which_a_return_cancels() -> Result {
        let one = session("ses_close_one")?;
        let only = window("window-one")?;
        let mut registry = CloseRegistry::new();
        let start = tokio::time::Instant::now();

        registry.attach_window(&one, only.clone());
        registry.detach_window(&one, &only, start);
        assert!(
            registry.expired(start).is_none(),
            "the grace has not expired"
        );
        registry.attach_window(&one, only.clone());
        assert!(
            registry.expired(start + DEFAULT_CLOSE_GRACE).is_none(),
            "a window returning within the grace keeps the turn"
        );

        registry.detach_window(&one, &only, start + DEFAULT_CLOSE_GRACE);
        assert_eq!(
            registry.expired(start + DEFAULT_CLOSE_GRACE * 2),
            Some(one.clone()),
            "a silent loss stops the turn once the grace expires"
        );
        Ok(())
    }

    #[test]
    fn only_the_last_window_counts() -> Result {
        let one = session("ses_close_windows")?;
        let first = window("window-first")?;
        let second = window("window-second")?;
        let mut registry = CloseRegistry::new();
        let start = tokio::time::Instant::now();

        registry.attach_window(&one, first.clone());
        registry.attach_window(&one, second.clone());
        registry.detach_window(&one, &second, start);
        assert!(
            registry.expired(start + DEFAULT_CLOSE_GRACE).is_none(),
            "one window is still watching"
        );

        registry.detach_window(&one, &first, start);
        assert_eq!(
            registry.expired(start + DEFAULT_CLOSE_GRACE),
            Some(one.clone()),
            "losing the last window starts its grace"
        );
        Ok(())
    }

    #[test]
    fn an_unknown_window_name_is_not_evidence_that_any_window_left() -> Result {
        let one = session("ses_close_stale")?;
        let live = window("window-live")?;
        let stale = window("window-stale")?;
        let mut registry = CloseRegistry::new();
        let start = tokio::time::Instant::now();

        registry.attach_window(&one, live);
        // A detach for a name this registry never attached changes nothing:
        // the live window still holds the Session.
        registry.detach_window(&one, &stale, start);
        assert!(
            registry.expired(start + DEFAULT_CLOSE_GRACE).is_none(),
            "a name the service never accepted must not start the grace"
        );

        // The same is true for a Session this registry has never seen.
        registry.detach_window(&session("ses_close_absent")?, &stale, start);
        assert!(registry.expired(start).is_none());
        Ok(())
    }

    #[test]
    fn a_resumed_window_is_one_claim_not_two() -> Result {
        let one = session("ses_close_resume")?;
        let only = window("window-resumed")?;
        let mut registry = CloseRegistry::new();
        let start = tokio::time::Instant::now();

        // A reconnect re-sends the same window name before the old stream is
        // gone. Counting it twice would leave a phantom watcher behind.
        registry.attach_window(&one, only.clone());
        registry.attach_window(&one, only.clone());
        registry.detach_window(&one, &only, start);
        assert_eq!(
            registry.expired(start + DEFAULT_CLOSE_GRACE),
            Some(one.clone()),
            "the resumed window's one claim ended with its last stream"
        );
        Ok(())
    }

    #[test]
    fn a_fresh_loss_does_not_hide_one_whose_grace_already_expired() -> Result {
        let older = session("ses_close_older")?;
        let newer = session("ses_close_newer")?;
        let mut registry = CloseRegistry::new();
        let start = tokio::time::Instant::now();

        // The older loss is recorded first, then a later one, so iteration
        // order alone would surface the fresh Session and mask the expired one.
        let old_window = window("window-older")?;
        registry.attach_window(&older, old_window.clone());
        registry.detach_window(&older, &old_window, start);
        let new_window = window("window-newer")?;
        registry.attach_window(&newer, new_window.clone());
        registry.detach_window(&newer, &new_window, start + DEFAULT_CLOSE_GRACE);
        assert_eq!(
            registry.expired(start + DEFAULT_CLOSE_GRACE),
            Some(older),
            "the expired Session is reported even when a fresh loss is present"
        );
        assert_eq!(
            registry.expired(start + DEFAULT_CLOSE_GRACE * 2),
            Some(newer.clone())
        );
        Ok(())
    }

    #[test]
    fn a_claim_releases_its_window_on_drop() -> Result {
        let one = session("ses_close_claim")?;
        let watch = Arc::new(WindowWatch::new());
        let start = tokio::time::Instant::now();
        let claim = watch.attach(&one);
        // While the claim is alive the Session is watched, so no grace runs.
        assert!(watch.next_deadline(start).is_none());
        drop(claim);
        // The claim records the drop's own reading, not `start`, so the grace
        // began no earlier than this test's baseline.
        let deadline = watch
            .next_deadline(start)
            .ok_or("dropping the claim starts the grace")?;
        assert!(
            deadline >= start + DEFAULT_CLOSE_GRACE,
            "the grace cannot end before it begins"
        );
        Ok(())
    }
}
