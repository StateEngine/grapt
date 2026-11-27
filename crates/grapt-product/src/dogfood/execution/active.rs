mod shutdown;
mod state;
mod status;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use grapt_api::u3a::DogfoodEvent;
use grapt_types::{ExecutionId, SessionId};

#[derive(Debug, Default)]
pub(in crate::dogfood) struct LiveActiveExecutions {
    sessions: Mutex<HashMap<SessionId, ActiveEntry>>,
    /// Sessions whose acknowledged execution is still preparing (MCP startup)
    /// and has no drain yet, each with the signal an interrupt trips.
    preparing: Mutex<HashMap<SessionId, Preparing>>,
    /// The quiescence emitter, present in a live application. `None` keeps the standalone
    /// registries in tests exactly as they were: no edge, no map.
    pub(super) status: Option<Arc<super::super::session_status::SessionStatusEmitter>>,
}

#[derive(Debug, Clone)]
struct ActiveEntry {
    execution: ExecutionId,
    retry: Option<grapt_api::u3a::SessionStatusInfo>,
    compacting: bool,
    state_available: bool,
    human_input: Option<Arc<crate::dogfood::human_input::Waits>>,
    shutdown: Option<super::super::lifecycle::StopReason>,
    cancel: tokio::sync::watch::Sender<bool>,
}

/// One Session's preparation: the signal an interrupt trips, and whether its
/// drain has taken the signal over.
#[derive(Debug)]
struct Preparing {
    cancel: tokio::sync::watch::Sender<bool>,
    handed_off: bool,
}

/// One Session's preparation window; ends when dropped.
pub(super) struct PreparingGuard<'a> {
    active: &'a LiveActiveExecutions,
    session: SessionId,
}

impl Drop for PreparingGuard<'_> {
    fn drop(&mut self) {
        self.active.lock_preparing().remove(&self.session);
    }
}

pub(super) struct LiveActiveGuard<'a> {
    active: &'a LiveActiveExecutions,
    session: SessionId,
    execution: ExecutionId,
}

impl Drop for LiveActiveGuard<'_> {
    /// Removes only its own execution: a successor that registered after this
    /// one settled keeps its entry.
    fn drop(&mut self) {
        self.active.release_state(&self.session, &self.execution);
    }
}

impl LiveActiveExecutions {
    pub(super) fn with_status(
        emitter: Arc<super::super::session_status::SessionStatusEmitter>,
    ) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            preparing: Mutex::new(HashMap::new()),
            status: Some(emitter),
        }
    }

    pub(super) fn register(
        &self,
        session: SessionId,
        execution: ExecutionId,
        limit: usize,
    ) -> Result<LiveActiveGuard<'_>, String> {
        let mut active = self.lock();
        if active.contains_key(&session) {
            return Err(format!(
                "session {} already has a projected execution",
                session.as_str()
            ));
        }
        if active.len() >= limit {
            return Err("the active execution bound is exhausted".to_owned());
        }
        active.insert(
            session.clone(),
            ActiveEntry {
                execution: execution.clone(),
                retry: None,
                compacting: false,
                state_available: true,
                human_input: None,
                shutdown: None,
                cancel: tokio::sync::watch::channel(false).0,
            },
        );
        drop(active);
        Ok(LiveActiveGuard {
            active: self,
            session,
            execution,
        })
    }

    /// `execution` has a durable terminal: it is no longer active to clients,
    /// before its terminal is published. Only that
    /// execution's entry goes; a successor's stays.
    pub(super) fn settle(&self, session: &SessionId, execution: &ExecutionId) {
        let owe_idle = {
            let mut active = self.lock();
            if active.get(session).map(|entry| &entry.execution) == Some(execution) {
                active.remove(session);
                if let Some(emitter) = &self.status {
                    emitter.settle_decision(session)
                } else {
                    false
                }
            } else {
                false
            }
        };
        if owe_idle {
            if let Some(emitter) = &self.status {
                emitter.publish_idle(session);
            }
        }
    }

    /// Mark `session` as preparing: it is active to clients from here, and
    /// an interrupt reaches it through the returned signal.
    pub(super) fn prepare(
        &self,
        session: SessionId,
    ) -> (PreparingGuard<'_>, tokio::sync::watch::Receiver<bool>) {
        let (cancel, cancelled) = self.lock().get(&session).map_or_else(
            || tokio::sync::watch::channel(false),
            |entry| (entry.cancel.clone(), entry.cancel.subscribe()),
        );
        self.lock_preparing().insert(
            session.clone(),
            Preparing {
                cancel,
                handed_off: false,
            },
        );
        // The session is active to clients from here: its quiescence edge is
        // busy, and the prepare-failure paths owe the return to idle through
        // the settle this registry performs when the entry leaves.
        if let Some(emitter) = &self.status {
            emitter.busy(&session);
        }
        let execution = self
            .lock()
            .get(&session)
            .map(|entry| entry.execution.clone());
        if let Some(execution) = execution {
            self.record_state(&session, &execution, grapt_service_state::Change::Start);
        }
        (
            PreparingGuard {
                active: self,
                session,
            },
            cancelled,
        )
    }

    /// Interrupt a preparing Session; `false` when it is not preparing.
    /// After the hand-off the drain's own slot receives interrupts instead.
    pub(super) fn cancel_preparing(&self, session: &SessionId) -> bool {
        self.lock_preparing().get(session).is_some_and(|preparing| {
            !preparing.handed_off && {
                preparing.cancel.send_replace(true);
                true
            }
        })
    }

    /// Hand `session`'s preparation to the drain it now owns: whether it was
    /// interrupted. Called under the coordinator's slot lock as the drain is
    /// claimed, so every interrupt lands either here or on the drain's slot.
    /// The Session stays projected as active until its run ends.
    pub(super) fn hand_off(&self, session: &SessionId) -> bool {
        self.lock_preparing()
            .get_mut(session)
            .is_some_and(|preparing| {
                preparing.handed_off = true;
                *preparing.cancel.borrow()
            })
    }

    /// The executions clients see as active: those the coordinator drains,
    /// those still preparing before their drain exists, and those in an active
    /// retry loop while waiting to re-enter execution.
    pub(super) fn projected(&self, running: Vec<SessionId>) -> Vec<(SessionId, ExecutionId)> {
        let active = self.lock();
        let preparing: Vec<SessionId> = self.lock_preparing().keys().cloned().collect();
        let mut sessions = running;
        for session in preparing {
            if !sessions.contains(&session) {
                sessions.push(session);
            }
        }
        let mut retrying: Vec<SessionId> = active
            .iter()
            .filter(|(_, entry)| entry.retry.is_some())
            .map(|(session, _)| session.clone())
            .collect();
        retrying.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        for session in retrying {
            if !sessions.contains(&session) {
                sessions.push(session);
            }
        }
        sessions
            .into_iter()
            .filter_map(|session| {
                active
                    .get(&session)
                    .cloned()
                    .map(|entry| (session, entry.execution))
            })
            .collect()
    }

    pub(super) fn validate(
        &self,
        session: &SessionId,
        expected: Option<&ExecutionId>,
    ) -> Result<(), String> {
        if expected.is_some_and(|execution| {
            self.lock().get(session).map(|entry| &entry.execution) != Some(execution)
        }) {
            return Err("the active execution identity changed".into());
        }
        Ok(())
    }

    /// Identity checking and signalling share one lock, but no store or hub
    /// lock: the receiver belongs to this execution even after it settles.
    pub(super) fn cancel_execution(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
    ) -> Result<(), String> {
        self.cancel_target(session, Some(execution)).map(|_| ())
    }

    /// Capture exactly the execution signalled while its registry lock is held.
    pub(super) fn cancel_target(
        &self,
        session: &SessionId,
        expected: Option<&ExecutionId>,
    ) -> Result<Option<ExecutionId>, String> {
        let active = self.lock();
        let entry = active.get(session);
        if expected.is_some_and(|expected| entry.map(|entry| &entry.execution) != Some(expected)) {
            return Err("the active execution identity changed".into());
        }
        Ok(entry.map(|entry| {
            entry.cancel.send_replace(true);
            entry.execution.clone()
        }))
    }

    /// The sessions the status registry lists as active: the registered
    /// executions and the ones still preparing before their drain exists,
    /// in a deterministic order.
    ///
    /// A preparation whose drain registered and then settled is residue the
    /// run's guard still carries: it must not revive a settled execution for
    /// hydration while the ordinary projection has already gone empty
    ///. The authoritative
    /// lifecycle is the registry map — a handed-off preparation adds nothing
    /// of its own once its entry settled, and the preparing-before-drain
    /// coverage and successor protection stand as before.
    fn listed_sessions(
        active: &HashMap<SessionId, ActiveEntry>,
        preparing: &HashMap<SessionId, Preparing>,
    ) -> Vec<SessionId> {
        let mut sessions: Vec<SessionId> = active.keys().cloned().collect();
        for (session, entry) in preparing.iter() {
            let preparing_before_drain = !entry.handed_off && !active.contains_key(session);
            if preparing_before_drain && !sessions.contains(session) {
                sessions.push(session.clone());
            }
        }
        sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        sessions
    }

    /// The production hydration offer: list the sessions the registry
    /// shows active and offer each one's busy to the snapshot, while the
    /// registry lock is held across the capture and the offers. A
    /// concurrent settle is ordered against the offer's debt record: it
    /// has either already removed the session, so the session is not
    /// listed and no stale busy is queued with no debt, or it lands after
    /// the record, so its decision removes the debt and publishes the
    /// matching idle. A stale busy is never queued and a debt is never
    /// orphaned. The
    /// listing is sorted so the snapshot's busy frames keep their
    /// deterministic order. Returns the listed sessions so the caller can
    /// run the idle half, which skips exactly those.
    fn lock_preparing(&self) -> MutexGuard<'_, HashMap<SessionId, Preparing>> {
        match self.preparing.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<SessionId, ActiveEntry>> {
        match self.sessions.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_api::u3a::{
        DogfoodEventHub, SseLimitValues, SseLimits, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES,
    };
    use grapt_store::{OsRandomIds, SystemClock};
    use grapt_types::EventId;

    pub(super) fn session() -> SessionId {
        "ses_handoff"
            .parse()
            .unwrap_or_else(|error| panic!("fixture session id: {error:?}"))
    }

    pub(super) fn execution(id: &str) -> ExecutionId {
        ExecutionId::new(id).unwrap_or_else(|| panic!("fixture id"))
    }

    pub(super) fn hub() -> Arc<DogfoodEventHub> {
        let limits = SseLimits::try_new(SseLimitValues {
            subscribers: 4,
            pending_events: 8,
            event_bytes: MAX_SSE_EVENT_BYTES,
            frame_bytes: MAX_SSE_FRAME_BYTES,
        })
        .unwrap_or_else(|error| panic!("fixture limits: {error}"));
        let id =
            grapt_api::u3a::WireId::<EventId>::parse("evt_execution_active_fixture".to_owned())
                .unwrap_or_else(|| panic!("fixture event id"));
        let connected = grapt_api::u3a::ServerConnectedEvent::new(id);
        Arc::new(
            DogfoodEventHub::try_new(limits, connected)
                .unwrap_or_else(|error| panic!("fixture hub: {error}")),
        )
    }

    /// A settled execution's late guard drop must not remove a successor that registered
    /// after it settled.
    #[test]
    fn a_settled_executions_late_drop_keeps_its_successor() {
        let active = LiveActiveExecutions::default();
        let first = active
            .register(session(), execution("exe_first"), 4)
            .unwrap_or_else(|error| panic!("{error}"));
        active.settle(&session(), &execution("exe_first"));
        assert!(
            active.projected(vec![session()]).is_empty(),
            "the settled execution is still active"
        );
        let _second = active
            .register(session(), execution("exe_second"), 4)
            .unwrap_or_else(|error| panic!("{error}"));
        drop(first);
        assert_eq!(
            active.projected(vec![session()]),
            vec![(session(), execution("exe_second"))],
            "the first execution's late drop removed its successor"
        );
    }

    #[test]
    fn an_unsettled_guard_still_removes_its_own_entry() {
        let active = LiveActiveExecutions::default();
        let guard = active
            .register(session(), execution("exe_only"), 4)
            .unwrap_or_else(|error| panic!("{error}"));
        drop(guard);
        assert!(active.projected(vec![session()]).is_empty());
    }

    /// An interrupt naming the settled execution, arriving after it settled and before
    /// its run returns, is refused: it reaches neither the successor nor the settled
    /// execution.
    #[test]
    fn an_interrupt_for_a_settled_execution_is_a_no_op() {
        let active = LiveActiveExecutions::default();
        let _first = active
            .register(session(), execution("exe_first"), 4)
            .unwrap_or_else(|error| panic!("{error}"));
        active.settle(&session(), &execution("exe_first"));
        assert!(active
            .validate(&session(), Some(&execution("exe_first")))
            .is_err());
        let _second = active
            .register(session(), execution("exe_second"), 4)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            active
                .validate(&session(), Some(&execution("exe_first")))
                .is_err(),
            "an interrupt for the settled execution would reach its successor"
        );
        assert!(active
            .validate(&session(), Some(&execution("exe_second")))
            .is_ok());
        assert_eq!(
            active.projected(vec![session()]),
            vec![(session(), execution("exe_second"))]
        );
    }

    #[test]
    fn an_interrupt_before_the_hand_off_travels_with_it() {
        let active = LiveActiveExecutions::default();
        let (_preparing, cancelled) = active.prepare(session());
        assert!(active.cancel_preparing(&session()));
        assert!(*cancelled.borrow());
        assert!(
            active.hand_off(&session()),
            "the drain must start interrupted"
        );
    }

    #[test]
    fn after_the_hand_off_an_interrupt_is_the_drains() {
        let active = LiveActiveExecutions::default();
        let execution = ExecutionId::new("exe_handoff").unwrap_or_else(|| panic!("fixture id"));
        let _running = active
            .register(session(), execution.clone(), 1)
            .unwrap_or_else(|error| panic!("{error}"));
        let (preparing, _cancelled) = active.prepare(session());
        assert!(!active.hand_off(&session()));
        // The preparation no longer claims it, so the caller asks the
        // coordinator, whose slot is active from the same instant.
        assert!(!active.cancel_preparing(&session()));
        // And clients still see the turn until its run ends.
        assert_eq!(active.projected(Vec::new()), vec![(session(), execution)]);
        drop(preparing);
        assert!(active.projected(Vec::new()).is_empty());
    }

    /// SQ-H1: register, prepare,
    /// hand off, and settle the execution while the preparation guard still
    /// lives. The ordinary projection is empty, and the status hydration
    /// must agree: the settled session hydrates no busy.
    #[test]
    fn a_settled_handoff_does_not_hydrate_busy_while_the_preparation_guard_lives() {
        let hub = hub();
        let emitter = Arc::new(
            super::super::super::session_status::SessionStatusEmitter::new(
                Arc::new(
                    OsRandomIds::seed().unwrap_or_else(|error| panic!("fixture ids: {error}")),
                ),
                Arc::new(SystemClock),
                hub,
            ),
        );
        let active = LiveActiveExecutions::with_status(Arc::clone(&emitter));
        let session = session();
        let execution = execution("exe_settled_handoff");
        let guard = active
            .register(session.clone(), execution.clone(), 4)
            .unwrap_or_else(|error| panic!("{error}"));
        let (preparing, _cancelled) = active.prepare(session.clone());
        // The drain was claimed without an interrupt: `hand_off` reports
        // the preparation's cancel flag, which nothing here ever sets.
        assert!(
            !active.hand_off(&session),
            "the preparation was not interrupted"
        );
        active.settle(&session, &execution);
        assert!(
            active.projected(Vec::new()).is_empty(),
            "the settled execution is no longer active"
        );
        let mut frames = Vec::new();
        let listed = active.offer_status_hydration(&emitter, &mut frames);
        assert!(
            listed.is_empty(),
            "the handed-off preparation residue must not list the settled session"
        );
        // The settled session's last-known-idle fact may hydrate (it is
        // honest); what the residue must not revive is the busy report.
        emitter.finish_hydration(&listed, &mut frames);
        let rendered: Vec<String> = frames
            .iter()
            .map(|frame| {
                serde_json::to_string(frame)
                    .unwrap_or_else(|error| panic!("render hydration frame: {error}"))
            })
            .collect();
        assert!(
            !rendered.iter().any(|frame| {
                frame.contains("ses_handoff") && frame.contains(r#""status":{"type":"busy"}"#)
            }),
            "hydration must not report the settled session busy: {rendered:?}"
        );
        drop(preparing);
        drop(guard);
    }

    /// The production preparation path, end to end at the registry: the
    /// busy edge the prepare publishes, and the idle edge the guard drop
    /// owes when the execution is abandoned before its drain. Q4 interrupts
    /// a started execution; this case covers abandonment before drain.
    #[test]
    fn the_production_prepare_path_publishes_busy_and_the_guard_drop_owes_idle() {
        let hub = hub();
        let emitter = Arc::new(
            super::super::super::session_status::SessionStatusEmitter::new(
                Arc::new(
                    OsRandomIds::seed().unwrap_or_else(|error| panic!("fixture ids: {error}")),
                ),
                Arc::new(SystemClock),
                Arc::clone(&hub),
            ),
        );
        let active = Arc::new(LiveActiveExecutions::with_status(Arc::clone(&emitter)));
        let session = session();
        let subscription = hub
            .subscribe_host()
            .unwrap_or_else(|error| panic!("fixture subscription: {error}"));
        let subscription = Arc::new(subscription);
        // One result crosses the channel: the reader's single Vec send
        //.
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let reader = {
            let sub = Arc::clone(&subscription);
            std::thread::spawn(move || {
                // The subscriber's first frame is the hub's own
                // `server.connected`; keep only the status edges, and stop
                // once the witness holds both it is owed.
                let mut seen = Vec::new();
                for _ in 0..8 {
                    match sub.next_frame() {
                        Ok(Some(frame)) => {
                            let text = String::from_utf8_lossy(&frame).into_owned();
                            if text.contains(r#""type":"session.status""#) {
                                seen.push(text);
                                if seen.len() == 2 {
                                    break;
                                }
                            }
                        }
                        _ => break,
                    }
                }
                let _ = tx.send(seen);
            })
        };
        {
            let guard = active
                .register(session.clone(), execution("exe_prepare_path"), 4)
                .unwrap_or_else(|error| panic!("{error}"));
            let (preparing, _cancelled) = active.prepare(session.clone());
            // The execution is abandoned: the run ends without a drain, the
            // preparation guard drops, then the registered guard settles.
            drop(preparing);
            drop(guard);
        }
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(frames) => {
                assert!(
                    frames.len() == 2,
                    "busy then idle must both reach the hub: {frames:?}"
                );
                assert!(
                    frames[0].contains(r#""busy""#),
                    "the first edge must be the prepare's busy: {frames:?}"
                );
                assert!(
                    frames[1].contains(r#""idle""#),
                    "the abandoned preparation must settle the session to idle: {frames:?}"
                );
            }
            Err(error) => {
                subscription.cancel();
                let _ = reader.join();
                panic!("the prepare path's edges did not reach the hub: {error:?}");
            }
        }
        let _ = reader.join();
    }

    /// The mirror: a registered execution that never prepared (the
    /// production refusal paths between register and prepare) owes no busy
    /// and therefore no idle edge either — settling the never-busy is
    /// silence, not a fabricated quiescence.
    #[test]
    fn a_never_prepared_settle_publishes_no_idle_edge() {
        let hub = hub();
        let emitter = Arc::new(
            super::super::super::session_status::SessionStatusEmitter::new(
                Arc::new(
                    OsRandomIds::seed().unwrap_or_else(|error| panic!("fixture ids: {error}")),
                ),
                Arc::new(SystemClock),
                Arc::clone(&hub),
            ),
        );
        let active = Arc::new(LiveActiveExecutions::with_status(Arc::clone(&emitter)));
        let session = session();
        let subscription = hub
            .subscribe_host()
            .unwrap_or_else(|error| panic!("fixture subscription: {error}"));
        let subscription = Arc::new(subscription);
        // At most the eight frames the reader loop is owed cross the
        // channel.
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let reader = {
            let sub = Arc::clone(&subscription);
            std::thread::spawn(move || {
                // Forward every frame the subscriber is owed, in order.
                for _ in 0..8 {
                    match sub.next_frame() {
                        Ok(Some(frame)) => {
                            let text = String::from_utf8_lossy(&frame).into_owned();
                            if tx.send(text).is_err() {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            })
        };
        // The connected frame is queued at subscription: the witness starts
        // on a clean slate.
        let first = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|error| panic!("no connected frame: {error:?}"));
        assert!(
            !first.contains(r#""type":"session.status""#),
            "the clean slate must hold only the hub's own frame: {first:?}"
        );
        let guard = active
            .register(session.clone(), execution("exe_never_prepared"), 4)
            .unwrap_or_else(|error| panic!("{error}"));
        // The refusal paths between register and prepare end the execution
        // without a busy edge; the settle must stay silent.
        drop(guard);
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            other => {
                subscription.cancel();
                let _ = reader.join();
                panic!("a never-busy settle published an edge: {other:?}");
            }
        }
        subscription.cancel();
        let _ = reader.join();
    }

    /// SQ-H2 hydration remainder:
    /// the mirror of the never-prepared silence with a live client
    /// composed in. A subscriber that hydrates while an execution is
    /// registered but not yet prepared is told busy; the refusal paths
    /// between register and prepare then abandon it, and the session must
    /// return to idle — the registry goes empty, and the client told busy
    /// cannot keep it.
    #[test]
    fn a_hydrated_busy_is_settled_to_idle_when_abandoned_before_prepare() {
        let hub = hub();
        let emitter = Arc::new(
            super::super::super::session_status::SessionStatusEmitter::new(
                Arc::new(
                    OsRandomIds::seed().unwrap_or_else(|error| panic!("fixture ids: {error}")),
                ),
                Arc::new(SystemClock),
                Arc::clone(&hub),
            ),
        );
        let active = Arc::new(LiveActiveExecutions::with_status(Arc::clone(&emitter)));
        install_production_hydrator(&hub, &active, &emitter);
        let session = session();
        let guard = active
            .register(session.clone(), execution("exe_hydration_remainder"), 4)
            .unwrap_or_else(|error| panic!("{error}"));
        let subscription = hub
            .subscribe_host()
            .unwrap_or_else(|error| panic!("fixture subscription: {error}"));
        let subscription = Arc::new(subscription);
        // The reader is owed one triple at most across the channel.
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let reader = {
            let sub = Arc::clone(&subscription);
            std::thread::spawn(move || {
                // The first frame is the hub's own `server.connected`, the
                // second this subscriber's busy hydration; the third is the
                // edge owed once the execution is abandoned.
                let mut seen = Vec::new();
                for _ in 0..8 {
                    match sub.next_frame() {
                        Ok(Some(frame)) => {
                            let text = String::from_utf8_lossy(&frame).into_owned();
                            seen.push(text);
                            if seen.len() == 3 {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                let _ = tx.send(seen);
            })
        };
        // The refusal paths between register and prepare end the execution
        // without a busy edge; the subscriber told busy is owed the idle.
        drop(guard);
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(frames) => {
                assert!(
                    !frames
                        .first()
                        .is_some_and(|frame| frame.contains(r#""type":"session.status""#)),
                    "the first frame must be the hub's own connection: {frames:?}"
                );
                assert!(
                    frames.get(1).is_some_and(|frame| {
                        frame.contains(r#""type":"session.status""#)
                            && frame.contains(r#""busy""#)
                            && frame.contains("ses_handoff")
                    }),
                    "the hydration must tell this subscriber the session is busy: {frames:?}"
                );
                assert!(
                    frames.get(2).is_some_and(|frame| {
                        frame.contains(r#""type":"session.status""#)
                            && frame.contains(r#""idle""#)
                            && frame.contains("ses_handoff")
                    }),
                    "the abandoned execution must return the hydrated session to idle: {frames:?}"
                );
            }
            Err(error) => {
                subscription.cancel();
                let _ = reader.join();
                panic!("the hydrated busy was never settled to idle: {error:?}");
            }
        }
        let _ = reader.join();
        assert!(
            active.projected(Vec::new()).is_empty(),
            "the abandoned execution must leave no active session"
        );
    }

    /// Install the exact production hydration supplier: the offer runs
    /// under the registry lock, then the idle half, on the frames vector
    /// the closure owns.
    fn install_production_hydrator(
        hub: &Arc<DogfoodEventHub>,
        active: &Arc<LiveActiveExecutions>,
        emitter: &Arc<super::super::super::session_status::SessionStatusEmitter>,
    ) {
        let hydrate_active = Arc::clone(active);
        let hydrate_status = Arc::clone(emitter);
        assert!(
            hub.install_status_hydrator(Box::new(move || {
                let mut frames = Vec::new();
                let listed = hydrate_active.offer_status_hydration(&hydrate_status, &mut frames);
                hydrate_status.finish_hydration(&listed, &mut frames);
                frames
            })),
            "the production hydration supplier must install once"
        );
    }

    /// SQ-H2 concurrent remainder:
    /// the settle the production hydrator's two-call closure could once
    /// land between the snapshot's capture and the hydration's completion.
    /// The offer now records its debt while the registry lock is held, so
    /// the settle is ordered against it in one of two ways, and both are
    /// exercised here without a pause. First, the settle lands before the
    /// hydrator's offer acquires the lock: the session is listed at
    /// snapshot capture, gone by completion, and the stale busy is omitted
    /// with no debt left behind. Second, the offer lands first: the debt
    /// is recorded under the lock, and the settle's decision removes it
    /// and publishes the matching idle instead of stranding the hydrated
    /// busy.
    #[test]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn a_settle_between_snapshot_capture_and_hydration_omits_the_stale_busy() {
        let hub = hub();
        let emitter = Arc::new(
            super::super::super::session_status::SessionStatusEmitter::new(
                Arc::new(
                    OsRandomIds::seed().unwrap_or_else(|error| panic!("fixture ids: {error}")),
                ),
                Arc::new(SystemClock),
                Arc::clone(&hub),
            ),
        );
        let active = Arc::new(LiveActiveExecutions::with_status(Arc::clone(&emitter)));
        install_production_hydrator(&hub, &active, &emitter);
        let session = session();
        // Snapshot capture: the registry lists the session to a capture
        // taken now.
        let guard = active
            .register(session.clone(), execution("exe_hydration_race"), 4)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            active.projected(vec![session.clone()]),
            vec![(session.clone(), execution("exe_hydration_race"))],
            "the capture must list the registered session"
        );
        // The matching pre-prepare abandon settles before the hydrator's
        // offer acquires the registry lock: the settle is silent — no debt
        // yet — and last-known records the idle.
        drop(guard);
        let subscription = hub
            .subscribe_host()
            .unwrap_or_else(|error| panic!("fixture subscription: {error}"));
        let subscription = Arc::new(subscription);
        // The reader collects until the settle broadcast of the second phase arrives: at
        // most two session-status idles cross.
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let reader = {
            let sub = Arc::clone(&subscription);
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                let mut status_idles = 0usize;
                for _ in 0..8 {
                    match sub.next_frame() {
                        Ok(Some(frame)) => {
                            let text = String::from_utf8_lossy(&frame).into_owned();
                            if text.contains(r#""type":"session.status""#)
                                && text.contains("ses_handoff")
                                && text.contains(r#""idle""#)
                            {
                                status_idles += 1;
                            }
                            seen.push(text);
                            if status_idles == 2 {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                let _ = tx.send(seen);
            })
        };
        // Second phase: the offer acquires the lock first, so the debt is
        // recorded under it and the settle's decision publishes the
        // matching idle.
        let guard = active
            .register(session.clone(), execution("exe_hydration_race_2"), 4)
            .unwrap_or_else(|error| panic!("{error}"));
        let subscription_two = hub
            .subscribe_host()
            .unwrap_or_else(|error| panic!("fixture subscription: {error}"));
        let subscription_two = Arc::new(subscription_two);
        let (tx_two, rx_two) = std::sync::mpsc::sync_channel(1);
        let reader_two = {
            let sub = Arc::clone(&subscription_two);
            std::thread::spawn(move || {
                // The first frame is the hub's own `server.connected`, the
                // second this subscriber's busy hydration; the third is
                // the edge owed once the execution is abandoned.
                let mut seen = Vec::new();
                for _ in 0..8 {
                    match sub.next_frame() {
                        Ok(Some(frame)) => {
                            let text = String::from_utf8_lossy(&frame).into_owned();
                            seen.push(text);
                            if seen.len() == 3 {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                let _ = tx_two.send(seen);
            })
        };
        drop(guard);
        match rx_two.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(frames) => {
                assert!(
                    !frames
                        .first()
                        .is_some_and(|frame| frame.contains(r#""type":"session.status""#)),
                    "the first frame must be the hub's own connection: {frames:?}"
                );
                assert!(
                    frames.get(1).is_some_and(|frame| {
                        frame.contains(r#""type":"session.status""#)
                            && frame.contains(r#""busy""#)
                            && frame.contains("ses_handoff")
                    }),
                    "the hydration must tell this subscriber the session is busy: {frames:?}"
                );
                assert!(
                    frames.get(2).is_some_and(|frame| {
                        frame.contains(r#""type":"session.status""#)
                            && frame.contains(r#""idle""#)
                            && frame.contains("ses_handoff")
                    }),
                    "the settle after the offered debt must publish the matching idle: {frames:?}"
                );
            }
            Err(error) => {
                subscription_two.cancel();
                let _ = reader_two.join();
                panic!("the hydrated busy was never settled to idle: {error:?}");
            }
        }
        let _ = reader_two.join();
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(frames) => {
                assert!(
                    !frames.iter().any(|frame| {
                        frame.contains(r#""type":"session.status""#)
                            && frame.contains("ses_handoff")
                            && frame.contains(r#""busy""#)
                    }),
                    "a stale busy reached a subscriber whose settle already ran: {frames:?}"
                );
                assert_eq!(
                    frames
                        .iter()
                        .filter(|frame| {
                            frame.contains(r#""type":"session.status""#)
                                && frame.contains("ses_handoff")
                                && frame.contains(r#""idle""#)
                        })
                        .count(),
                    2,
                    "the first subscriber owes the cache idle and the settle broadcast idle: {frames:?}"
                );
            }
            Err(error) => {
                subscription.cancel();
                let _ = reader.join();
                panic!("the quiescence edge never reached the first subscriber: {error:?}");
            }
        }
        let _ = reader.join();
        assert!(
            active.projected(Vec::new()).is_empty(),
            "the abandoned executions must leave no active session"
        );
    }
}

#[cfg(test)]
#[path = "retry_feedback_tests.rs"]
mod retry_feedback_tests;
