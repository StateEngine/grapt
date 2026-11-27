//! Live blocks a snapshot may show. A publish takes the subscriber lock, then
//! the registry's (`grapt-stream`), and holds both from before its frame's
//! offer through the change: one linearization point. Readers and discards
//! take only the registry's lock, never one a writer holds across a commit.
use super::*;
use grapt_stream::provisional::{ProvisionalChange, ProvisionalRegistry, ProvisionalText};
use grapt_types::SessionId;

impl DogfoodEventHub {
    /// Offer a live block frame, then apply its change: subscriber lock, then
    /// this registry's, held from before the offer (which carries a delta's
    /// offset) through the change. Never the reverse order.
    pub fn publish_provisional(
        &self,
        event: &DogfoodEvent,
        change: ProvisionalChange<'_>,
    ) -> Result<PublishReceipt, SseError> {
        let max_text = self.limits.event_bytes();
        self.publish_scoped(
            event,
            None,
            || true,
            || {
                let registry = self.registry();
                let from = registry.offset(change);
                (registry, from)
            },
            |registry| {
                registry.apply(change, max_text);
            },
        )
    }

    /// The registry's own lock; a poisoned one still holds whole blocks.
    fn registry(&self) -> std::sync::MutexGuard<'_, ProvisionalRegistry> {
        self.provisional
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Apply a change that travels with no frame (a refusal or a discard).
    pub fn change_provisional(&self, change: ProvisionalChange<'_>) -> Result<(), SseError> {
        let max_text = self.limits.event_bytes();
        self.registry().apply(change, max_text);
        Ok(())
    }

    /// The session's live blocks; incomplete while any is omitted or text-less.
    pub fn provisional(&self, session: &SessionId) -> Result<Vec<ProvisionalText>, SseError> {
        self.registry().live(session)
    }
}
