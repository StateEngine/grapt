//! Mail status for a Session bound through a linked StateEngine runtime, where
//! the native mail owner does not run. Say plainly that mail is stopped, rather
//! than showing an empty inbox or retrying a door that is not there.
use super::*;
use grapt_api::{
    u3a::{DogfoodEvent, FiniteF64, LiveEvent, MailAttentionData, WireId},
    DogfoodEventHub,
};
use grapt_types::{ClockAuthority, EventId, IdAuthority, LocationRef};
use std::collections::BTreeMap;

const LINKED_CAP: usize = 32;

// Constructed by the linked-runtime seat (StateEngine W3) once it is wired.
#[cfg_attr(not(test), allow(dead_code))]
pub(in crate::dogfood) struct LinkedMailStatus {
    ids: Arc<dyn IdAuthority + Send + Sync>,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
    hub: Arc<DogfoodEventHub>,
    sessions: Mutex<BTreeMap<SessionId, LocationRef>>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl LinkedMailStatus {
    pub(in crate::dogfood) fn new(
        ids: Arc<dyn IdAuthority + Send + Sync>,
        clock: Arc<dyn ClockAuthority + Send + Sync>,
        hub: Arc<DogfoodEventHub>,
    ) -> Self {
        Self {
            ids,
            clock,
            hub,
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    /// Publish Stopped once for a bound Session whose mail this product cannot
    /// observe, and keep it for clients that connect later.
    pub(in crate::dogfood) fn set_unsupported(&self, session: &SessionId, location: LocationRef) {
        let inserted = match self.sessions.lock() {
            Ok(mut sessions) if sessions.len() < LINKED_CAP || sessions.contains_key(session) => {
                sessions.insert(session.clone(), location.clone()).is_none()
            }
            _ => false,
        };
        if !inserted {
            return;
        }
        crate::diagnostics::report(format!(
            "native mail stopped session={session} cause=linked-runtime-mail-unsupported"
        ));
        match self.frame(session, location) {
            Some(frame) if self.hub.publish(&frame).is_ok() => {}
            _ => self
                .hub
                .invalidate_because("linked mail status could not be published"),
        }
    }

    /// Reconnect frames, read from this small map only.
    pub(in crate::dogfood) fn hydrate(&self) -> Vec<DogfoodEvent> {
        let Ok(sessions) = self.sessions.lock() else {
            return Vec::new();
        };
        sessions
            .iter()
            .take(LINKED_CAP)
            .filter_map(|(session, location)| self.frame(session, location.clone()))
            .collect()
    }

    fn frame(&self, session: &SessionId, location: LocationRef) -> Option<DogfoodEvent> {
        let id = WireId::<EventId>::parse(format!("evt_{}", self.ids.mint().as_str()))?;
        let wire = WireId::parse(session.as_str().to_owned())?;
        let created = FiniteF64::try_new(self.clock.now().as_millis() as f64).ok()?;
        Some(DogfoodEvent::MailAttention(LiveEvent::new(
            id,
            created,
            MailAttentionData::new(wire, MailAttentionInfo::Stopped, location),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dogfood::native_mail::custody::tests::Clock;
    use grapt_api::u3a::{ServerConnectedEvent, SseLimitValues, SseLimits};

    #[test]
    fn unsupported_linked_mail_shows_stopped_once_and_hydrates(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let limits = SseLimits::try_new(SseLimitValues {
            subscribers: 2,
            pending_events: 16,
            event_bytes: grapt_api::u3a::MAX_SSE_EVENT_BYTES,
            frame_bytes: grapt_api::u3a::MAX_SSE_FRAME_BYTES,
        })?;
        let connected =
            ServerConnectedEvent::new(WireId::parse("evt_linked_mail".into()).ok_or("event")?);
        let hub = Arc::new(DogfoodEventHub::try_new(limits, connected)?);
        let context = crate::dogfood::native_mail::tests::context(None, false)?;
        let status = LinkedMailStatus::new(context.ids.clone(), Arc::new(Clock), hub);
        let session: SessionId = "ses_linked_mail".parse()?;
        let location = context.location.clone();
        status.set_unsupported(&session, location.clone());
        status.set_unsupported(&session, location);
        let frames = status.hydrate();
        assert_eq!(frames.len(), 1, "a repeated bind published another status");
        let frame = serde_json::to_value(&frames[0])?;
        assert_eq!(frame["data"]["status"]["type"], "stopped");
        Ok(())
    }
}
