//! The server's order of two facts about one session's activity.
use super::*;
use std::cmp::Ordering;

/// Where a fact sits in the server's order. Both parts come from the frame
/// that carried the fact, never from a client clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FactStamp {
    /// The durable event seq, when the frame carried a `durable` block.
    pub seq: Option<u64>,
    /// The server's `created` stamp, when the frame carried one.
    pub created: Option<u64>,
}

impl FactStamp {
    pub(crate) fn of(envelope: &EventEnvelope) -> Self {
        Self {
            seq: envelope.durable.as_ref().map(|durable| durable.seq),
            created: envelope.created,
        }
    }

    /// Durable seqs order two facts that both carry one. Wall-clock stamps
    /// can tie or step, so they order only facts without a common seq.
    /// `None` when the facts share neither: the server never ordered them.
    pub fn order(&self, other: &Self) -> Option<Ordering> {
        match (self.seq, other.seq) {
            (Some(seq), Some(other)) => Some(seq.cmp(&other)),
            _ => self
                .created
                .zip(other.created)
                .map(|(created, other)| created.cmp(&other)),
        }
    }
}

/// The one rule for whether a fact replaces the one held, each given as
/// (running, stamp): the server ordered it later, or at the same position it
/// is the settled one. `None` when the server never ordered the two, so the
/// caller keeps its previous precedence.
pub fn supersedes(fact: (bool, FactStamp), held: (bool, FactStamp)) -> Option<bool> {
    Some(match fact.1.order(&held.1)? {
        Ordering::Greater => true,
        Ordering::Equal => !fact.0,
        Ordering::Less => false,
    })
}

impl SessionView {
    /// Record a quiescence fact with the frame's order.
    pub(super) fn set_idle(&mut self, idle: bool, envelope: &EventEnvelope) {
        self.idle = Some(idle);
        self.idle_created = envelope.created;
        self.idle_seq = FactStamp::of(envelope).seq;
    }

    /// The order of the frame that set `idle`, the running edge included.
    pub fn idle_stamp(&self) -> FactStamp {
        FactStamp {
            seq: self.idle_seq,
            created: self.idle_created,
        }
    }

    /// The order of the live terminal this view applied. A durable rebuild
    /// carries neither part.
    pub fn terminal_stamp(&self) -> FactStamp {
        FactStamp {
            seq: self.terminal_seq,
            created: self.terminal_created,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_cut_and_restore_keep_the_idle_frames_order() -> Result<(), Box<dyn std::error::Error>>
    {
        let session = crate::wire::session_id("ses_activity_cut")?;
        let mut view = SessionView::new(&session);
        let envelope = EventEnvelope::decode(
            &serde_json::json!({
                "id":"evt_activity_cut","type":"session.execution.started","created":100,
                "durable":{"aggregateID":session.spelled(),"seq":10,"version":1},
                "data":{"sessionID":session.spelled(),"executionID":"exe_activity"}
            })
            .to_string(),
        )?;
        view.apply(&envelope, crate::Bounds::PRODUCTION)?;
        let before = view.idle_stamp();
        let held = view.suspend_live_quiescence();
        assert_eq!(view.idle_stamp(), FactStamp::default());
        view.restore_live_quiescence(held);
        assert_eq!(view.idle_stamp(), before);
        Ok(())
    }
}
