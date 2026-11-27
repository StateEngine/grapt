//! One session's last-known quiescence state, and its single emission point. The
//! SESSION-QUIESCENCE ruling: `session.status` is the only quiescence edge the product
//! reports. This emitter sits beside the active-execution registries, updates its map at
//! the registry transitions, and publishes the edge through the existing hub funnel. The
//! map is a bounded last-known cache: a restart starts empty, and an empty cache is an
//! honest unknown, not idle. Publication of the return to idle reads the registry
//! transition itself (the owed set), never the cache: churn that evicts a cache entry
//! cannot swallow a legitimate settle. The live executor records hydration debt while
//! its registry lock is held, and settles under that lock. Against a concurrent
//! settle: a stale busy is omitted or a matching idle is published, and the
//! debt cannot outlive its entry.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use grapt_api::u3a::{
    DogfoodEvent, FiniteF64, LiveEvent, SessionStatusData, SessionStatusInfo, WireId,
};
use grapt_api::DogfoodEventHub;
use grapt_service_state::Snapshot;
use grapt_store::{OsRandomIds, SystemClock};
use grapt_types::{ClockAuthority, EventId, IdAuthority, SessionId};

mod feedback;
mod hydration;
mod journal;
pub(crate) use feedback::RetryFeedback;
mod lifecycle;
mod owner;
mod read;
pub(crate) use hydration::{install_hydration, install_volatile_hydration};
pub(crate) use lifecycle::project as lifecycle_event;
pub(crate) use owner::{ConfirmationError, StatusInputs};
pub(crate) use read::fields as state_fields;
mod retry;
pub(crate) use journal::{wire as state_wire, Presentation};

/// The ruling's last-known cap: 256 sessions, LRU.
const LAST_KNOWN_CAP: usize = 256;
/// A hydration snapshot is courtesy, never a replay: bound the frames one
/// new subscriber can receive from the cache.
const HYDRATION_CAP: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusKind {
    Idle,
    Busy,
}

#[derive(Debug)]
struct LastKnown {
    order: VecDeque<SessionId>,
    state: HashMap<SessionId, StatusKind>,
}

impl LastKnown {
    /// Record `session` as `kind`; the previous kind, if any. A new entry
    /// evicts the least-recently-used one at the cap; an update reorders.
    fn record(&mut self, session: SessionId, kind: StatusKind) -> Option<StatusKind> {
        let existing = self.state.contains_key(&session);
        if !existing && self.state.len() >= LAST_KNOWN_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.state.remove(&oldest);
            }
        }
        let previous = self.state.insert(session.clone(), kind);
        if existing {
            if let Some(position) = self.order.iter().position(|entry| *entry == session) {
                self.order.remove(position);
            }
        }
        self.order.push_back(session);
        previous
    }
}

pub(crate) struct SessionStatusEmitter {
    ids: Arc<OsRandomIds>,
    clock: Arc<SystemClock>,
    hub: Arc<DogfoodEventHub>,
    last_known: Mutex<LastKnown>,
    /// The sessions that owe a return to idle: a client was told them busy and the
    /// matching settle has not been recorded. The idle edge's publication reads THIS set,
    /// not last-known membership — the cache exists to hydrate newcomers, and a
    /// legitimate settle must not be swallowed by a cache eviction. Bounded by the live
    /// registry lifecycles: every insert is a busy a client was told — the published edge
    /// at a registry busy transition, or a hydration frame for a session the registry
    /// lists whose execution may abandon before prepare ever publishes one — and its
    /// remove the settle of that registry entry. In both production offers the record is
    /// made while the offering registry's lock is held, so a concurrent settle is ordered
    /// against it.
    owed: Mutex<HashSet<SessionId>>,
    journal: Option<Arc<Mutex<grapt_inbox::Inbox>>>,
}

impl core::fmt::Debug for SessionStatusEmitter {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SessionStatusEmitter")
            .finish_non_exhaustive()
    }
}

impl SessionStatusEmitter {
    /// An unavailable authoritative observation makes the streamed view stale.
    fn observation_unavailable(&self, session: &SessionId, error: &str) {
        self.hub.invalidate_session_because(
            session,
            &format!("session {session}: authoritative status unavailable: {error}"),
        );
    }

    pub(crate) fn journaled(
        ids: &Arc<OsRandomIds>,
        clock: &Arc<SystemClock>,
        hub: &Arc<DogfoodEventHub>,
        inbox: &Arc<Mutex<grapt_inbox::Inbox>>,
    ) -> Arc<Self> {
        Arc::new(
            Self::new(Arc::clone(ids), Arc::clone(clock), Arc::clone(hub))
                .with_journal(Arc::clone(inbox)),
        )
    }

    /// Record every Session still active at restart as disconnected, each
    /// after `first` succeeds for it. Skipped Sessions are logged and left as
    /// found.
    pub(super) fn recover(
        store: &mut grapt_store::EventStore,
        clock: &SystemClock,
        ids: &OsRandomIds,
        first: &mut dyn FnMut(&mut grapt_store::EventStore, &SessionId) -> Result<(), String>,
    ) {
        let recovery = match grapt_service_state::recover(
            store,
            &grapt_types::DeterministicAuthorities::new(clock, ids),
            crate::dogfood::MAX_ACTIVE_EXECUTIONS,
            first,
        ) {
            Ok(recovery) => recovery,
            Err(error) => {
                crate::server_log::warn(format_args!(
                    "Session status restart reconciliation could not list active sessions: {error}; unresolved owners remain unconfirmed."
                ));
                return;
            }
        };
        for warning in &recovery.warnings {
            crate::server_log::warn(format_args!(
                "Session status restart reconciliation skipped {warning}; it remains unconfirmed."
            ));
        }
        if recovery.warnings_omitted > 0 {
            crate::server_log::warn(format_args!(
                "Session status restart reconciliation skipped {} more sessions not listed.",
                recovery.warnings_omitted
            ));
        }
    }

    pub(crate) fn new(
        ids: Arc<OsRandomIds>,
        clock: Arc<SystemClock>,
        hub: Arc<DogfoodEventHub>,
    ) -> Self {
        Self {
            ids,
            clock,
            hub,
            last_known: Mutex::new(LastKnown {
                order: VecDeque::new(),
                state: HashMap::new(),
            }),
            owed: Mutex::new(HashSet::new()),
            journal: None,
        }
    }

    /// The session became, or stays, active. Always publishes the edge: the
    /// registry listed it at the moment the caller records this.
    pub(crate) fn busy(&self, session: &SessionId) {
        self.lock_owed().insert(session.clone());
        self.record(session, StatusKind::Busy);
        self.publish(session, StatusKind::Busy);
    }

    /// The session's active entry left the registry. Publishes the return to idle only
    /// when the session owes one: a session that was never told busy must not be
    /// fabricated into an idle edge, and one that already settled must not be notified
    /// twice. The decision reads the owed set, not the cache, so a churn that evicted the
    /// cache entry still reaches the subscriber told the busy, by edge or by hydration.
    /// Test composite only: both production settles run `settle_decision` under their
    /// registry lock and `publish_idle` after it, so the decision is ordered against the
    /// hydration offer's debt record.
    #[cfg(test)]
    pub(crate) fn idle(&self, session: &SessionId) {
        if self.settle_decision(session) {
            self.publish_idle(session);
        }
    }

    /// The settle decision, made while the caller holds its registry
    /// lock: remove the debt and record the last-known idle, returning
    /// whether the session owed the return. Ordering the decision under
    /// the caller's registry lock is what makes it atomic against the
    /// hydration offer's debt record: the offer either listed the session
    /// and recorded the debt before this decision, which then removes it
    /// and the caller publishes the matching idle, or this decision
    /// removed the entry first, so the offer never lists the session and
    /// records no debt.
    pub(crate) fn settle_decision(&self, session: &SessionId) -> bool {
        let was_busy = self.lock_owed().remove(session);
        self.record(session, StatusKind::Idle);
        was_busy
    }

    /// Publish the return to idle, after the caller has dropped its
    /// registry lock: the hub's subscriber lock is never taken under a
    /// registry lock, so the settle cannot deadlock a hydrator running on
    /// a subscriber thread.
    pub(crate) fn publish_idle(&self, session: &SessionId) {
        self.publish(session, StatusKind::Idle);
    }

    /// The snapshot a new subscriber receives after its connection frame. The registry
    /// ALWAYS wins: an active session is reported busy, and it can never be reported
    /// idle. Then the last-known idle entries, in LRU order, that the registry does not
    /// list. A last-known BUSY entry the registry does not list is reported nothing:
    /// absent is not busy. Every busy this snapshot tells is recorded as owed: the
    /// matching settle must return it to idle even when the execution abandons before
    /// prepare ever publishes a busy edge. The live executor runs `offer_active` under
    /// its registry lock, ordering the debt record against a concurrent settle. This
    /// composite is the direct-call shape only: it trusts its `active` listing as given.
    /// Test composite only: no production path passes a listing it does not own under a
    /// registry lock.
    #[cfg(test)]
    pub(crate) fn hydrate(&self, active: &[SessionId]) -> Vec<DogfoodEvent> {
        let mut active = active.to_vec();
        active.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        active.dedup();
        let mut frames = Vec::with_capacity(HYDRATION_CAP);
        for session in &active {
            if !self.offer_active(session, &mut frames) {
                break;
            }
        }
        self.finish_hydration(&active, &mut frames);
        frames
    }

    /// Offer one listed session's busy to the snapshot, recording its debt
    /// in the same step. A caller that holds its registry lock across the
    /// offers (the production offer) gets the ordering guarantee against a
    /// concurrent settle: the settle either already removed the entry, so
    /// the session is not listed and no stale busy is queued with no debt,
    /// or it lands after the record, so its decision removes the debt and
    /// publishes the matching idle. `false` when the snapshot is at its
    /// cap.
    pub(crate) fn offer_active(&self, session: &SessionId, frames: &mut Vec<DogfoodEvent>) -> bool {
        if frames.len() >= HYDRATION_CAP {
            return false;
        }
        if let Some(frame) = self.build(session, StatusKind::Busy) {
            // The snapshot's subscriber is told this session is busy; the entry's settle
            // must owe the return to idle even when the execution abandons before prepare
            // ever publishes a busy edge.
            self.lock_owed().insert(session.clone());
            frames.push(frame);
        }
        true
    }

    /// The snapshot's idle half: the last-known idle entries, in LRU
    /// order, that the caller's listing does not cover, within the
    /// remaining cap.
    pub(crate) fn finish_hydration(&self, listed: &[SessionId], frames: &mut Vec<DogfoodEvent>) {
        let snapshot: Vec<(SessionId, StatusKind)> = {
            let known = self.lock_last_known();
            known
                .order
                .iter()
                .filter_map(|session| {
                    known
                        .state
                        .get(session)
                        .map(|kind| (session.clone(), *kind))
                })
                .collect()
        };
        for (session, kind) in snapshot {
            if frames.len() >= HYDRATION_CAP {
                break;
            }
            if listed.contains(&session) {
                continue;
            }
            if kind == StatusKind::Idle {
                if let Some(frame) = self.build(&session, StatusKind::Idle) {
                    frames.push(frame);
                }
            }
        }
    }

    fn record(&self, session: &SessionId, kind: StatusKind) -> Option<StatusKind> {
        self.lock_last_known().record(session.clone(), kind)
    }

    fn lock_last_known(&self) -> MutexGuard<'_, LastKnown> {
        match self.last_known.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn lock_owed(&self) -> MutexGuard<'_, HashSet<SessionId>> {
        match self.owed.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn build(&self, session: &SessionId, kind: StatusKind) -> Option<DogfoodEvent> {
        let status = match kind {
            StatusKind::Idle => SessionStatusInfo::idle(),
            StatusKind::Busy => SessionStatusInfo::busy(),
        };
        self.build_info(session, status)
    }

    fn publish_snapshot(
        &self,
        session: &SessionId,
        snapshot: Snapshot,
        present: impl Fn(&Snapshot) -> Result<Presentation, String>,
    ) -> Result<(), String> {
        let presentation = present(&snapshot)?;
        let data = SessionStatusData::new(
            WireId::parse(session.as_str().to_owned()).ok_or("session status identity refused")?,
            presentation.status.clone(),
        )
        .with_phase(presentation.phase)
        .with_state(state_wire(snapshot.clone())?);
        let id = format!("evt_{}", self.ids.mint().as_str());
        let event = DogfoodEvent::SessionStatus(LiveEvent::new(
            WireId::parse(id).ok_or("session status event identity refused")?,
            FiniteF64::try_new(self.clock.now().as_millis() as f64)?,
            data,
        ));
        let Some(journal) = &self.journal else {
            return Ok(());
        };
        let inbox = match journal.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Serialize the final revision check and live offer with journal writes.
        if grapt_service_state::read(inbox.store().connection(), session)? != snapshot {
            return Ok(());
        }
        self.hub
            .publish_if(&event, || {
                present(&snapshot).is_ok_and(|current| current == presentation)
            })
            .map_err(|error| error.to_string())?;
        drop(inbox);
        Ok(())
    }

    /// Publish one edge. A dropped listener or an unbuildable frame can
    /// never fail the transition: the registry move stands, and clients
    /// that miss the edge re-read it on their next hydration.
    fn publish(&self, session: &SessionId, kind: StatusKind) {
        let Some(event) = self.build(session, kind) else {
            self.hub.invalidate_session_because(
                session,
                &format!("session {session}: the status edge could not be built"),
            );
            return;
        };
        if let Err(error) = self.hub.publish(&event) {
            self.hub.invalidate_session_because(
                session,
                &format!("session {session}: the status edge could not be published: {error}"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> Arc<OsRandomIds> {
        Arc::new(OsRandomIds::seed().unwrap_or_else(|error| panic!("fixture ids: {error}")))
    }

    fn hub() -> Arc<DogfoodEventHub> {
        let limits = grapt_api::u3a::SseLimits::try_new(grapt_api::u3a::SseLimitValues {
            subscribers: 1,
            pending_events: 1,
            event_bytes: grapt_api::u3a::MAX_SSE_EVENT_BYTES,
            frame_bytes: grapt_api::u3a::MAX_SSE_FRAME_BYTES,
        })
        .unwrap_or_else(|error| panic!("fixture limits: {error}"));
        let id = WireId::<EventId>::parse("evt_status_fixture".to_owned())
            .unwrap_or_else(|| panic!("fixture event id"));
        let connected = grapt_api::u3a::ServerConnectedEvent::new(id);
        Arc::new(
            DogfoodEventHub::try_new(limits, connected)
                .unwrap_or_else(|error| panic!("fixture hub: {error}")),
        )
    }

    fn emitter() -> Arc<SessionStatusEmitter> {
        Arc::new(SessionStatusEmitter::new(
            ids(),
            Arc::new(SystemClock),
            hub(),
        ))
    }

    fn session(raw: &str) -> SessionId {
        raw.parse()
            .unwrap_or_else(|error| panic!("fixture session id: {error:?}"))
    }

    #[test]
    fn local_capacity_status_names_waiting_and_elapsed_time_without_provider_blame(
    ) -> Result<(), String> {
        let emitter = emitter();
        let provider = grapt_types::ProviderId::new("fixture").ok_or("provider")?;
        for elapsed in [0, 37, 3601] {
            let status = emitter
                .waiting_info(
                    &provider,
                    grapt_runtime::step::RetryNotice {
                        attempt: 1,
                        after: std::time::Duration::from_secs(2),
                        class: None,
                        preparation_stage: None,
                        capacity_wait: Some(std::time::Duration::from_secs(elapsed)),
                    },
                )
                .ok_or("capacity waiting status absent")?;
            let SessionStatusInfo::Retry {
                message, action, ..
            } = status
            else {
                return Err("capacity wait was shown as idle or ordinary work".into());
            };
            let lower = message.to_lowercase();
            assert!(
                lower.contains("waiting") && lower.contains("capacity"),
                "{message}"
            );
            assert!(
                message.contains(&elapsed.to_string()),
                "elapsed time missing: {message}"
            );
            assert!(
                !lower.contains("provider") && !lower.contains("fail"),
                "{message}"
            );
            assert!(
                !message.contains('\n'),
                "capacity waiting must be a plain line"
            );
            assert!(
                action.is_some(),
                "capacity waiting lost its available action"
            );
        }
        Ok(())
    }

    #[test]
    fn the_last_known_map_is_lru_bounded_at_the_ruling_cap() {
        let mut known = LastKnown {
            order: VecDeque::new(),
            state: HashMap::new(),
        };
        let first = session("ses_first");
        assert_eq!(known.record(first.clone(), StatusKind::Idle), None);
        for index in 0..(LAST_KNOWN_CAP - 1) {
            known.record(session(&format!("ses_fill{index:04}")), StatusKind::Busy);
        }
        assert_eq!(known.state.len(), LAST_KNOWN_CAP);
        // A new entry evicts the untouched oldest.
        known.record(session("ses_newest"), StatusKind::Idle);
        assert_eq!(known.state.len(), LAST_KNOWN_CAP);
        assert!(
            !known.state.contains_key(&first),
            "the untouched oldest entry survived"
        );
        // Re-add it: its slot evicts the now-oldest filler.
        known.record(first.clone(), StatusKind::Busy);
        assert!(
            !known.state.contains_key(&session("ses_fill0000")),
            "the oldest filler was not evicted"
        );
        // Touch it again, then add one more: the next eviction must skip it.
        known.record(first.clone(), StatusKind::Busy);
        known.record(session("ses_newest2"), StatusKind::Idle);
        assert!(
            known.state.contains_key(&first),
            "the touched entry was evicted"
        );
        assert!(
            !known.state.contains_key(&session("ses_fill0001")),
            "the LRU order was not honored"
        );
    }

    #[test]
    fn hydration_never_reports_busy_for_a_session_the_registry_dropped() {
        let emitter = emitter();
        let dropped = session("ses_dropped");
        let settled = session("ses_settled");
        let active = session("ses_active");
        // The registry dropped `dropped` while it was still busy: the entry
        // left without a settle, so its last-known kind stays Busy.
        emitter.busy(&dropped);
        emitter.busy(&settled);
        emitter.idle(&settled);
        emitter.busy(&active);
        let frames = emitter.hydrate(std::slice::from_ref(&active));
        let rendered: Vec<String> = frames
            .iter()
            .map(|frame| {
                serde_json::to_string(frame)
                    .unwrap_or_else(|error| panic!("encode failed: {error}"))
            })
            .collect();
        assert_eq!(
            rendered.len(),
            2,
            "expected active-busy then settled-idle: {rendered:?}"
        );
        assert!(rendered[0].contains(r#""status":{"type":"busy"}"#));
        assert!(rendered[0].contains("ses_active"));
        assert!(rendered[1].contains(r#""status":{"type":"idle"}"#));
        assert!(rendered[1].contains("ses_settled"));
        assert!(
            !rendered.iter().any(|frame| frame.contains("ses_dropped")),
            "the dropped session leaked a frame: {rendered:?}"
        );
    }

    /// SQ-H2: a legitimate settle
    /// whose session the 256-entry cache evicted must still reach the
    /// subscriber that saw its busy edge. Publication reads the registry
    /// transition, not the cache membership.
    #[test]
    fn an_evicted_sessions_settle_still_reaches_its_live_subscriber() {
        let limits = grapt_api::u3a::SseLimits::try_new(grapt_api::u3a::SseLimitValues {
            subscribers: 4,
            pending_events: 8,
            event_bytes: grapt_api::u3a::MAX_SSE_EVENT_BYTES,
            frame_bytes: grapt_api::u3a::MAX_SSE_FRAME_BYTES,
        })
        .unwrap_or_else(|error| panic!("fixture limits: {error}"));
        let id = WireId::<EventId>::parse("evt_evicted_settle".to_owned())
            .unwrap_or_else(|| panic!("fixture event id"));
        let connected = grapt_api::u3a::ServerConnectedEvent::new(id);
        let hub = Arc::new(
            DogfoodEventHub::try_new(limits, connected)
                .unwrap_or_else(|error| panic!("fixture hub: {error}")),
        );
        let emitter = Arc::new(SessionStatusEmitter::new(
            ids(),
            Arc::new(SystemClock),
            Arc::clone(&hub),
        ));
        let active = session("ses_evicted");
        emitter.busy(&active);
        // Churn the cache past its cap: the active session's entry evicts.
        for index in 0..LAST_KNOWN_CAP {
            let other = session(&format!("ses_churn{index:04}"));
            emitter.busy(&other);
            emitter.idle(&other);
        }
        let subscription = hub
            .subscribe_host()
            .unwrap_or_else(|error| panic!("fixture subscription: {error}"));
        let subscription = Arc::new(subscription);
        // At most one frame crosses: the reader breaks after the first
        // status edge.
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let reader = {
            let sub = Arc::clone(&subscription);
            std::thread::spawn(move || {
                // The subscriber's first frame is the hub's own
                // `server.connected`; keep only the status edges.
                for _ in 0..8 {
                    match sub.next_frame() {
                        Ok(Some(frame)) => {
                            let text = String::from_utf8_lossy(&frame).into_owned();
                            if text.contains(r#""type":"session.status""#) {
                                let _ = tx.send(text);
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            })
        };
        emitter.idle(&active);
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(frame) => assert!(
                frame.contains(r#""type":"session.status""#)
                    && frame.contains(r#""idle""#)
                    && frame.contains("ses_evicted"),
                "the evicted session's settle must publish its idle edge: {frame}"
            ),
            Err(error) => {
                subscription.cancel();
                let _ = reader.join();
                panic!("no idle edge reached the subscriber after the cache churn: {error:?}");
            }
        }
        let _ = reader.join();
    }
}
