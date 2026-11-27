//! What the close grace needs to know about a Session's launch, held in memory.
//!
//! The deadline loop that stops unwatched turns must never take a store read:
//! a blocked or slow store would stall the loop that every other Session's
//! grace is measured against, which is the failure the stall-aware grace exists
//! to survive. So the one durable fact the stop decision needs — whether a
//! Session was launched to run with no window — is remembered here, at the
//! points that already hold a fresh [`SessionRecord`](grapt_session::SessionRecord).
//!
//! A Session this cache has never seen is NOT assumed headless. Stopping an
//! unwatched turn is today's behaviour and the safe answer for an interactive
//! launch; skipping a stop that should have happened is the worse mistake,
//! because it spends tokens for a window nobody is watching.

use std::collections::BTreeMap;
use std::sync::Mutex;

use grapt_types::{SessionId, SessionLaunch};

/// The launch modes this process has observed, keyed by Session.
///
/// Bounded in spirit by the number of Sessions this process has opened; a stale
/// entry is not a correctness problem because the mode of a Session never
/// changes after its launch.
#[derive(Debug, Default)]
pub(crate) struct Launches {
    modes: Mutex<BTreeMap<SessionId, SessionLaunch>>,
}

impl Launches {
    /// Remember how `session` was launched.
    pub(crate) fn remember(&self, session: &SessionId, launch: SessionLaunch) {
        let mut modes = match self.modes.lock() {
            Ok(held) => held,
            // A poisoned lock means a sibling panicked while holding it. The
            // map it guards is a cache of immutable facts, so recovering it is
            // correct: refusing every later stop decision would be a far worse
            // outcome than reading a map one panic left behind.
            Err(poisoned) => poisoned.into_inner(),
        };
        modes.insert(session.clone(), launch);
    }

    /// Whether `session` was recorded as a headless launch.
    ///
    /// `false` for a Session this process never observed: absence is not
    /// consent to run unwatched, so the caller keeps the ordinary stop.
    pub(crate) fn is_headless(&self, session: &SessionId) -> bool {
        let modes = match self.modes.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        modes
            .get(session)
            .is_some_and(|launch| launch.is_headless())
    }

    /// Decide the mode for a Session that carried no launch fact, once.
    ///
    /// An older client sends no launch header, so nothing durable says how it
    /// started. Its first prompt is the evidence we have: a launcher that names
    /// no window is running without one, which is the headless pattern. The
    /// decision is made ONCE per Session — a later prompt during a brief window
    /// gap must not flip a Session that started with a window into headless,
    /// which would make it unstoppable.
    pub(crate) fn decide_without_fact(&self, session: &SessionId, has_window: bool) {
        let mut modes = match self.modes.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        modes.entry(session.clone()).or_insert_with(|| {
            if has_window {
                SessionLaunch::Interactive
            } else {
                SessionLaunch::Headless
            }
        });
    }
}
