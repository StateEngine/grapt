//! Adapt retained application calls to context-owned response ordering.

pub(super) use grapt_context::response_order::ResponseOrder;

pub(super) struct Calls<'a>(pub(super) &'a [super::StoredCall]);

impl grapt_context::response_order::CallPositions for Calls<'_> {
    fn sequence(&self, call: &grapt_types::ToolCallId) -> Option<grapt_types::EventSeq> {
        self.0
            .iter()
            .find(|held| held.call == *call)
            .map(|held| grapt_types::EventSeq::new(held.seq))
    }
}
