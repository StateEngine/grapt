// SPDX-License-Identifier: MIT OR Apache-2.0
//! `session.publish_turn`: one completed turn of a conversation a harness runs
//! itself, with its token usage, diff counts and an erasable summary.
use crate::Sha256Hex;
use serde::{Deserialize, Serialize};

/// The host capability that offers [`PublishTurn`].
pub const PUBLISH_TURN_CAPABILITY: &str = "session.publish_turn";
pub(crate) const PUBLISH_TURN_OPERATION: &str = "session.publish_turn";
pub(crate) const RECEIPT_SCHEMA: &str = "se.publish-turn-receipt/v1";

const MAX_KEY_BYTES: usize = 128;
const MAX_OCCURRED_AT_US: i64 = 253_402_300_799_999_999;
const MAX_PATHS: usize = 256;
const MAX_NESTING_DEPTH: u32 = 64;
const MAX_PROSE_BYTES: usize = 1_048_576;
const MAX_DECISION_AND_PATHS_BYTES: usize = 65_536;

/// Who spoke the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    User,
    Assistant,
    Tool,
}

/// The provider and model that served the turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TurnModel {
    pub provider: String,
    pub model: String,
}

/// Token counts the harness reported; `cost_usd` only when it reported one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// What the turn changed in the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TurnDiff {
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
    pub paths: Vec<String>,
}

/// A decision the turn made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TurnDecision {
    pub title: String,
    pub rationale: String,
    pub scope: String,
}

/// A nested agent's turn, under the StateEngine session it ran in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TurnNesting {
    pub parent_session_id: String,
    pub nested_agent_id: String,
    pub depth: u32,
}

/// One completed turn. The same `idempotency_key` goes in the arguments and
/// the request key; after an uncertain outcome, retry with it unchanged.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PublishTurn {
    pub idempotency_key: String,
    /// The seat's agent; an owner connection names it.
    pub target_agent_id: Option<String>,
    /// The seat's session; an owner connection names it.
    pub session_id: Option<String>,
    pub conversation_id: String,
    pub harness: String,
    pub model: TurnModel,
    pub vendor_turn_id: String,
    pub occurred_at_us: i64,
    pub role: TurnRole,
    pub body_text: Option<String>,
    /// Bytes of the original body left out of `body_text`.
    pub body_omitted_bytes: u64,
    /// SHA-256 of the complete UTF-8 body.
    pub content_sha256: Sha256Hex,
    pub usage: Option<TurnUsage>,
    pub diff_summary: Option<TurnDiff>,
    pub decision: Option<TurnDecision>,
    pub nesting: Option<TurnNesting>,
}

impl PublishTurn {
    /// Why the host would refuse this turn, checked before it is sent.
    pub fn check(&self) -> Result<(), String> {
        let key = self.idempotency_key.as_bytes();
        if key.is_empty() || key.len() > MAX_KEY_BYTES || !key.iter().all(u8::is_ascii_graphic) {
            return Err("the idempotency key is not 1 to 128 printable ASCII bytes".into());
        }
        if !(0..=MAX_OCCURRED_AT_US).contains(&self.occurred_at_us) {
            return Err("occurred_at_us is outside 0 through the end of year 9999".into());
        }
        let paths = self
            .diff_summary
            .as_ref()
            .map_or(&[][..], |diff| diff.paths.as_slice());
        if paths.len() > MAX_PATHS {
            return Err("the diff names more than 256 paths".into());
        }
        if self
            .nesting
            .as_ref()
            .is_some_and(|nesting| !(1..=MAX_NESTING_DEPTH).contains(&nesting.depth))
        {
            return Err("a nesting depth is not 1 to 64".into());
        }
        let decision = self.decision.as_ref().map_or(0, |decision| {
            decision.title.len() + decision.rationale.len() + decision.scope.len()
        });
        let decision_and_paths = decision + paths.iter().map(String::len).sum::<usize>();
        if decision_and_paths > MAX_DECISION_AND_PATHS_BYTES {
            return Err("the decision and paths together exceed 64 KiB".into());
        }
        let body = self.body_text.as_ref().map_or(0, String::len);
        if body + decision_and_paths > MAX_PROSE_BYTES {
            return Err("the body, decision and paths together exceed 1 MiB".into());
        }
        Ok(())
    }
}

/// Whether the host recorded the turn now or replayed an earlier commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitState {
    Committed,
    Replayed,
}

/// The original commit a replay keeps.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TurnCommit {
    pub state: CommitState,
    pub event_id: String,
}

/// What the host recorded. Host measurements beside these fields are not
/// part of the receipt and are not kept.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PublishTurnReceipt {
    pub agent_id: String,
    pub session_id: String,
    pub vendor_turn_id: String,
    /// The event's store sequence number.
    pub cursor: u64,
    pub body_stored: bool,
    pub content_sha256_stored: bool,
    pub content_verified: bool,
    pub commit: TurnCommit,
}
