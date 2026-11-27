//! Request order follows assistant responses, not asynchronous commit order.
//! Raw rows retain their sequence numbers for compaction cuts and watermarks.

use std::collections::BTreeMap;

use grapt_provider::{CodexInputItem, MessageRole};
use grapt_session::history::AssistantMessage;
use grapt_types::{EventSeq, ToolCallId};

/// Originating content positions for rendered tool results.
pub trait CallPositions {
    /// Return the first retained call's durable sequence, if present.
    fn sequence(&self, call: &ToolCallId) -> Option<EventSeq>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Position {
    response: u64,
    result: bool,
    ordinal: u64,
    committed: u64,
}

/// Stable grouping of request items by their originating assistant response.
#[derive(Debug)]
pub struct ResponseOrder(BTreeMap<u64, Position>);

impl ResponseOrder {
    /// Associate rendered request rows with durable assistant content.
    pub fn prepare(
        rows: &[(u64, CodexInputItem)],
        messages: &[AssistantMessage],
        calls: &impl CallPositions,
    ) -> Result<Self, String> {
        let mut positions = BTreeMap::new();
        for (seq, item) in rows {
            if matches!(item, CodexInputItem::ImageUser { .. })
                || matches!(item, CodexInputItem::Message(message) if message.role != MessageRole::Assistant)
            {
                continue;
            }
            let result_call = match item {
                CodexInputItem::ToolOutput { call, .. }
                | CodexInputItem::ImageToolOutput { call, .. } => Some(call),
                _ => None,
            };
            let content_seq = match result_call {
                Some(call) => calls
                    .sequence(call)
                    .ok_or("tool result has no originating response content")?
                    .value(),
                None => *seq,
            };
            let (message, content) = messages
                .iter()
                .find_map(|message| {
                    message.content.as_ref()?.items().iter().find_map(|held| {
                        (held.committed.as_ref()?.seq == content_seq).then_some((message, held))
                    })
                })
                .ok_or("request item has no durable assistant response position")?;
            // A pre-visibility retry reuses the Step's message opening, but
            // consumes a later request (including its recovered cue and mail).
            // Group its content after that request's context, preserving the
            // prefix signed by the attempt that actually supplied the content.
            let response = if message.attempt == message.usage_attempt {
                message.opened_at_seq
            } else {
                message
                    .content
                    .as_ref()
                    .into_iter()
                    .flat_map(|content| content.items())
                    .filter_map(|held| held.committed.as_ref().map(|position| position.seq))
                    .min()
                    .ok_or("retried response has no committed content")?
            };
            positions.insert(
                *seq,
                Position {
                    response,
                    result: result_call.is_some(),
                    ordinal: content.item.ordinal().value(),
                    committed: *seq,
                },
            );
        }
        Ok(Self(positions))
    }

    /// Number of response boundary markers included in structural admission.
    pub fn boundaries(&self) -> usize {
        self.0
            .values()
            .filter(|position| !position.result)
            .map(|position| position.response)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// Keep each emitted item's originating durable position, including its
    /// response boundary and effort update, for request-local protection.
    pub fn emit_positioned(
        &self,
        mut rows: Vec<(u64, CodexInputItem)>,
        efforts: &BTreeMap<u64, grapt_provider::ReasoningEffort>,
    ) -> Vec<(u64, CodexInputItem)> {
        rows.sort_by_key(|(seq, _)| {
            self.0.get(seq).copied().unwrap_or(Position {
                response: *seq,
                result: false,
                ordinal: 0,
                committed: *seq,
            })
        });
        let mut input = Vec::with_capacity(rows.len() + self.boundaries());
        let mut previous = None;
        for (seq, item) in rows {
            let position = self.0.get(&seq);
            if let Some(position) = position.filter(|position| !position.result) {
                if previous != Some(position.response) {
                    input.push((seq, CodexInputItem::AssistantResponseStart));
                }
                previous = Some(position.response);
            } else {
                previous = None;
            }
            if let Some(effort) = efforts.get(&seq) {
                input.push((seq, CodexInputItem::EffortUpdate(*effort)));
            }
            input.push((seq, item));
        }
        input
    }
}
