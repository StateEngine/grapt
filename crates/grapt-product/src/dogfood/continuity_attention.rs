//! Each Session's StateEngine continuity level. It is published as the whole
//! state whenever it changes and offered to every new subscriber while it is
//! degraded, so a window that missed a change still gets the truth.
use grapt_api::{
    u3a::{
        ContinuityAttentionData, ContinuityAttentionInfo, ContinuityDegradedReason, DogfoodEvent,
        FiniteF64, LiveEvent, WireId,
    },
    DogfoodEventHub,
};
use grapt_session::execution::ContinuityFailure as E;
use grapt_types::{ClockAuthority, EventId, IdAuthority, LocationRef, SessionId};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Degraded Sessions held at once; one per capability Session at most.
const HELD: usize = 128;

impl std::fmt::Debug for ContinuityAttention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContinuityAttention")
            .finish_non_exhaustive()
    }
}

/// Only degraded Sessions are held: a healthy or never-bound Session needs no
/// offer, and reads as unknown until a frame says otherwise.
pub(in crate::dogfood) struct ContinuityAttention {
    levels: Mutex<BTreeMap<SessionId, (ContinuityAttentionInfo, LocationRef)>>,
    ids: Arc<dyn IdAuthority + Send + Sync>,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
    hub: Arc<DogfoodEventHub>,
}

impl ContinuityAttention {
    pub(in crate::dogfood) fn new(
        ids: Arc<dyn IdAuthority + Send + Sync>,
        clock: Arc<dyn ClockAuthority + Send + Sync>,
        hub: Arc<DogfoodEventHub>,
    ) -> Arc<Self> {
        Arc::new(Self {
            levels: Mutex::new(BTreeMap::new()),
            ids,
            clock,
            hub,
        })
    }

    /// Record this Session's level, publishing it only when it changes;
    /// returns whether it changed. Recovery publishes `ok` only for a Session
    /// that was degraded. The map is updated before the publish, outside the
    /// hub lock, so two concurrent executions of one Session could publish out
    /// of order; hydration always reads the map, and the next change heals it.
    pub(in crate::dogfood) fn set(
        &self,
        session: &SessionId,
        location: &LocationRef,
        status: ContinuityAttentionInfo,
    ) -> bool {
        let changed = self.levels.lock().map_or(true, |mut levels| {
            if status == ContinuityAttentionInfo::Ok {
                return levels.remove(session).is_some();
            }
            if !levels.contains_key(session) && levels.len() >= HELD {
                return false;
            }
            levels
                .insert(session.clone(), (status, location.clone()))
                .map(|(previous, _)| previous)
                != Some(status)
        });
        if changed {
            self.publish(session, status, location.clone());
        }
        changed
    }

    /// A capability was put: a Session waiting for one is no longer degraded.
    /// Any other outage stands until a refresh says otherwise.
    pub(in crate::dogfood) fn clear(&self, session: &SessionId) {
        let rebind = ContinuityAttentionInfo::Degraded {
            reason: ContinuityDegradedReason::Capability,
        };
        let removed = self.levels.lock().ok().and_then(|mut levels| {
            if levels.get(session).map(|(status, _)| *status) == Some(rebind) {
                levels.remove(session)
            } else {
                None
            }
        });
        if let Some((_, location)) = removed {
            self.publish(session, ContinuityAttentionInfo::Ok, location);
        }
    }

    /// After a start, mark every Session that needs continuity but holds no
    /// capability as needing a rebind, so a window sees it before any turn.
    pub(in crate::dogfood) fn seed_rebind(
        &self,
        inbox: &Mutex<grapt_inbox::Inbox>,
        custody: &Mutex<super::context::SessionCapabilities>,
    ) {
        // Collected under inbox then custody, the service's order, and
        // published only after both are released.
        let unbound: Vec<(SessionId, LocationRef)> = {
            let (Ok(inbox), Ok(custody)) = (inbox.lock(), custody.lock()) else {
                crate::diagnostics::report(
                    "stateengine-continuity rebind seeding skipped: a lock is poisoned".to_owned(),
                );
                return;
            };
            let sessions = match inbox.continuity_bound_sessions(HELD) {
                Ok(sessions) => sessions,
                Err(error) => {
                    crate::diagnostics::report(format!(
                        "stateengine-continuity rebind seeding skipped: {error}"
                    ));
                    return;
                }
            };
            let store = grapt_session::SessionStore::over(inbox.store());
            sessions
                .into_iter()
                .filter(|session| !custody.values.contains_key(session))
                .filter_map(|session| {
                    let placement = store.record(&session).ok()?.placement;
                    Some((session, placement))
                })
                .collect()
        };
        let rebind = ContinuityAttentionInfo::Degraded {
            reason: ContinuityDegradedReason::Capability,
        };
        for (session, location) in unbound {
            self.set(&session, &location, rebind);
        }
    }

    fn publish(&self, session: &SessionId, status: ContinuityAttentionInfo, location: LocationRef) {
        let published = self
            .frame(session, status, location)
            .is_some_and(|frame| self.hub.publish(&frame).is_ok());
        if !published {
            crate::diagnostics::report(format!(
                "stateengine-continuity attention unpublished session={session}"
            ));
        }
    }

    /// The degraded Sessions' levels, for a new or reconnecting subscriber;
    /// the hub keeps only those in the subscriber's locations. A Session with
    /// no frame reads as unknown, never as healthy.
    pub(in crate::dogfood) fn hydrate(&self) -> Vec<DogfoodEvent> {
        let Ok(levels) = self.levels.lock() else {
            return Vec::new();
        };
        levels
            .iter()
            .filter_map(|(session, (status, location))| {
                self.frame(session, *status, location.clone())
            })
            .collect()
    }

    fn frame(
        &self,
        session: &SessionId,
        status: ContinuityAttentionInfo,
        location: LocationRef,
    ) -> Option<DogfoodEvent> {
        let id = WireId::<EventId>::parse(format!("evt_{}", self.ids.mint().as_str()))?;
        let session = WireId::parse(session.as_str().to_owned())?;
        let created = FiniteF64::try_new(self.clock.now().as_millis() as f64).ok()?;
        Some(DogfoodEvent::ContinuityAttention(LiveEvent::new(
            id,
            created,
            ContinuityAttentionData::new(session, status, location),
        )))
    }
}

/// The level an outage degrades continuity to; None for a failure that
/// refuses the turn instead.
pub(super) fn degraded(failure: E) -> Option<ContinuityAttentionInfo> {
    let reason = match failure {
        E::Transport => ContinuityDegradedReason::Transport,
        E::Capability => ContinuityDegradedReason::Capability,
        E::ServerUnavailable(_) => ContinuityDegradedReason::ServerUnavailable,
        E::Incomplete => ContinuityDegradedReason::Incomplete,
        E::Rebound => ContinuityDegradedReason::Rebound,
        E::TimedOut { .. } => ContinuityDegradedReason::TimedOut,
        _ => return None,
    };
    Some(ContinuityAttentionInfo::Degraded { reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_api::u3a::{ServerConnectedEvent, SseLimitValues, SseLimits};
    use grapt_types::{IdToken, Timestamp};

    #[derive(Debug)]
    struct Ids;
    impl IdAuthority for Ids {
        fn mint(&self) -> IdToken {
            IdToken::new("0000000000000001").unwrap_or_else(|| panic!("id"))
        }
    }
    #[derive(Debug)]
    struct Clock;
    impl ClockAuthority for Clock {
        fn now(&self) -> Timestamp {
            Timestamp::from_millis(1760000000123)
        }
    }

    #[test]
    fn a_level_publishes_on_change_and_only_degraded_sessions_hydrate(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let limits = SseLimits::try_new(SseLimitValues {
            subscribers: 2,
            pending_events: 16,
            event_bytes: grapt_api::u3a::MAX_SSE_EVENT_BYTES,
            frame_bytes: grapt_api::u3a::MAX_SSE_FRAME_BYTES,
        })?;
        let connected =
            ServerConnectedEvent::new(WireId::parse("evt_continuity".into()).ok_or("event")?);
        let hub = Arc::new(DogfoodEventHub::try_new(limits, connected)?);
        let attention = ContinuityAttention::new(Arc::new(Ids), Arc::new(Clock), hub);
        let location = LocationRef::new("/fixture", None)?;
        let session: SessionId = "ses_000000000001".parse()?;
        let down = degraded(E::Capability).ok_or("an outage degrades")?;
        assert!(
            !attention.set(&session, &location, ContinuityAttentionInfo::Ok),
            "a Session that was never degraded publishes nothing"
        );
        assert!(
            attention.hydrate().is_empty(),
            "a healthy Session needs no offer"
        );
        assert!(attention.set(&session, &location, down));
        assert!(!attention.set(&session, &location, down));
        let offered = attention.hydrate();
        assert_eq!(offered.len(), 1);
        let frame = serde_json::to_value(&offered[0])?;
        assert_eq!(frame["type"], "session.continuity.attention");
        assert_eq!(
            frame["data"]["status"],
            serde_json::json!({"type": "degraded", "reason": "capability"})
        );
        for index in 2..=200 {
            let other: SessionId = format!("ses_{index:012}").parse()?;
            attention.set(&other, &location, down);
        }
        assert_eq!(attention.hydrate().len(), HELD, "the map is bounded");
        // A put clears a Session waiting for a capability, and no other outage.
        let other: SessionId = "ses_000000000002".parse()?;
        let offline = degraded(E::Transport).ok_or("an outage degrades")?;
        attention.set(&other, &location, offline);
        attention.clear(&other);
        attention.clear(&session);
        let held = attention.hydrate();
        assert!(held.iter().all(|frame| {
            serde_json::to_value(frame)
                .is_ok_and(|frame| frame["data"]["sessionID"] != session.as_str())
        }));
        assert!(held.iter().any(|frame| {
            serde_json::to_value(frame)
                .is_ok_and(|frame| frame["data"]["sessionID"] == other.as_str())
        }));
        assert!(!attention.set(&session, &location, ContinuityAttentionInfo::Ok));
        assert_eq!(
            attention.hydrate().len(),
            HELD - 1,
            "recovery leaves the map"
        );
        assert!(
            degraded(E::Principal).is_none(),
            "a boundary never degrades"
        );
        Ok(())
    }
}
