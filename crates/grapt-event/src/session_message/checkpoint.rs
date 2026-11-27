//! `session.content.checkpointed.1`: the text an open text block has streamed
//! so far (CLAIM-2).
//!
//! A text block's bytes are held in memory until the block closes, and only
//! the close commits a [`super::ContentAppendedFact`]. A process that dies
//! mid-stream would lose text the user already saw. The owner therefore
//! commits this fact at a bounded cadence while the block is open. Each one is
//! CUMULATIVE (the block's whole text so far), so the newest checkpoint alone
//! says what was streamed, and recovery can promote it without capturing
//! anything new.
//!
//! A checkpoint is never a content item by itself. A block that closes writes
//! its own append and its checkpoints are superseded; only process-loss
//! recovery of the attempt that wrote them may promote the newest one. Like
//! every session-message payload it carries an [`ArtifactRef`], never inline
//! bytes.

use grapt_types::{ArtifactRef, MessageId, PhysicalAttemptId};
use serde_json::{Map, Value};

use super::{
    decode_reference, encode_reference, invalid, object, reject_unknown, required, required_str,
    required_u64, PayloadError,
};

const CHECKPOINTED: &str = "session.content.checkpointed.1";

/// The text one open text block has streamed so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentCheckpointedFact {
    attempt: PhysicalAttemptId,
    message: MessageId,
    block: u32,
    text: ArtifactRef,
}

impl ContentCheckpointedFact {
    pub fn new(
        attempt: PhysicalAttemptId,
        message: MessageId,
        block: u32,
        text: ArtifactRef,
    ) -> Self {
        Self {
            attempt,
            message,
            block,
            text,
        }
    }
    /// The physical attempt that streamed it. Recovery promotes only the
    /// checkpoints of the attempt it settles, so text a retried attempt
    /// discarded is never brought back.
    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }
    pub fn message(&self) -> &MessageId {
        &self.message
    }
    /// The block ordinal, scoped to the attempt, as in its opening.
    pub fn block(&self) -> u32 {
        self.block
    }
    /// The captured text so far: every byte the block held, in order.
    pub fn text(&self) -> &ArtifactRef {
        &self.text
    }
}

pub fn encode_checkpointed(fact: &ContentCheckpointedFact) -> Value {
    let mut root = Map::new();
    root.insert("attempt".into(), Value::from(fact.attempt.as_str()));
    root.insert("message".into(), Value::from(fact.message.as_str()));
    root.insert("block".into(), Value::from(fact.block));
    root.insert("text".into(), encode_reference(&fact.text));
    Value::Object(root)
}

pub fn decode_checkpointed(value: &Value) -> Result<ContentCheckpointedFact, PayloadError> {
    let root = object(value, CHECKPOINTED)?;
    let attempt = PhysicalAttemptId::new(required_str(root, CHECKPOINTED, "attempt")?)
        .ok_or_else(|| invalid(CHECKPOINTED, "attempt", "a non-empty PhysicalAttemptId"))?;
    let message = required_str(root, CHECKPOINTED, "message")?
        .parse()
        .map_err(|_| invalid(CHECKPOINTED, "message", "a validated msg_* identity"))?;
    let block = u32::try_from(required_u64(root, CHECKPOINTED, "block")?)
        .map_err(|_| invalid(CHECKPOINTED, "block", "a bounded block ordinal"))?;
    let text = decode_reference(required(root, CHECKPOINTED, "text")?)?;
    reject_unknown(root, CHECKPOINTED, &["attempt", "message", "block", "text"])?;
    Ok(ContentCheckpointedFact::new(attempt, message, block, text))
}
