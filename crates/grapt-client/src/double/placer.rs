//! Positions for frames a witness writes by hand. A [`Placer`] is the stream
//! side of a projection witness: it places each live delta at its block's
//! byte offset and ends each block with its whole text, as the product's
//! positioned frames do. Frames that already carry their position, and
//! malformed ones, pass unchanged so their refusal stays the witness.

use crate::contract::{EventEnvelope, EventMember};
use std::collections::BTreeMap;

/// One live block's whole identity: its session, assistant message, kind
/// (reasoning or not) and ordinal. Message ids repeat across sessions, and
/// each kind numbers its own blocks.
type Block = (String, String, bool, u64);

/// The text each live block has been sent, while it is live.
#[derive(Debug, Default)]
pub struct Placer {
    blocks: BTreeMap<Block, String>,
}

impl Placer {
    /// Place one frame: a delta gains `from`, an end gains `text`.
    pub fn place(&mut self, mut envelope: EventEnvelope) -> EventEnvelope {
        self.place_data(envelope.member, &mut envelope.data);
        envelope
    }

    /// Place one frame's data by its member.
    pub(super) fn place_data(&mut self, member: Option<EventMember>, data: &mut serde_json::Value) {
        let (reasoning, phase) = match member {
            Some(EventMember::TextStarted) => (false, Phase::Start),
            Some(EventMember::TextDelta) => (false, Phase::Append),
            Some(EventMember::TextEnded) => (false, Phase::End),
            Some(EventMember::ReasoningStarted) => (true, Phase::Start),
            Some(EventMember::ReasoningDelta) => (true, Phase::Append),
            Some(EventMember::ReasoningEnded) => (true, Phase::End),
            _ => return,
        };
        let (Some(message), Some(ordinal)) = (
            data.get("assistantMessageID")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            data.get("ordinal").and_then(serde_json::Value::as_u64),
        ) else {
            return;
        };
        // A witness's frame may name no session; it is then its own stream's.
        let session = data
            .get("sessionID")
            .and_then(serde_json::Value::as_str)
            .map_or_else(String::new, str::to_owned);
        let key = (session, message, reasoning, ordinal);
        if phase == Phase::End {
            // The block ends: it carries its whole text and is live no more.
            let sent = self.blocks.remove(&key).unwrap_or_default();
            if data.get("text").is_none() {
                data["text"] = serde_json::json!(sent);
            }
            return;
        }
        let sent = self.blocks.entry(key).or_default();
        match phase {
            Phase::Start => sent.clear(),
            Phase::Append => {
                let delta = data
                    .get("delta")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                if let Some(delta) = delta {
                    if data.get("from").is_none() {
                        data["from"] = serde_json::json!(sent.len());
                    }
                    sent.push_str(&delta);
                }
            }
            Phase::End => {}
        }
    }
}

#[derive(PartialEq, Eq)]
enum Phase {
    Start,
    Append,
    End,
}
