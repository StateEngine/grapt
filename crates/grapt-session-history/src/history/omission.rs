//! The named gaps a fold leaves behind.
//!
//! A durable writer is allowed to be wrong and the reader must still answer, so
//! a row this fold OWNS that cannot be decoded costs that one item and is
//! recorded here rather than refusing the whole transcript. A shorter read must
//! never be reported as a complete one, so every gap is carried with enough
//! evidence to find its writer and, when the payload still proves one, the
//! message it belonged to.
use super::*;

/// Why one durable row or replay step could not be part of the fold.
///
/// The class is coarse on purpose: it names what a reader can act on without
/// repeating a decoder's message, and it stays a closed enum so a log line and
/// a projection marker can never disagree about the same omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryOmissionClass {
    /// The row's kind or version could not be classified (a lost or malformed
    /// type suffix, a negative sequence).
    Unclassified,
    /// The row's payload did not decode as the contract its kind names.
    Undecodable,
    /// The row's data was withheld because it exceeded the read's byte bound.
    Oversized,
}

impl HistoryOmissionClass {
    /// The stable name a log line and a projection marker carry.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unclassified => "unclassified",
            Self::Undecodable => "undecodable",
            Self::Oversized => "oversized",
        }
    }
}

/// One durable fact the fold left out, with enough evidence to find its
/// writer.
///
/// The sequence locates the row. `message` is present when the omission is
/// attributable to one assistant message, so a projection can coalesce its
/// markers and a replay can drop that message's tool pairs rather than send a
/// provider an orphaned call or result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryOmission {
    pub seq: u64,
    pub kind: String,
    pub class: HistoryOmissionClass,
    /// The reason, for a server log only. Never displayed and never sent on the
    /// wire: it is a decoder's own text and can name internals.
    pub detail: String,
    pub message: Option<MessageId>,
}

impl MessageHistory {
    /// Whether one stored row's payload decodes as the contract its kind
    /// names, decided by the fold's own dispatch.
    ///
    /// The gap scan asks this instead of folding the whole range: a fresh fold
    /// carries no state, so the probe is O(1) in memory and cannot disagree
    /// with [`MessageHistory::apply`] about what a decodable payload is.
    ///
    /// The answer is the probe's own OMISSION, never an escaping error:
    /// [`MessageHistory::apply`] already degrades an undecodable owned payload
    /// to a recorded gap and answers `Ok(())`, so inspecting the returned
    /// error would classify every such payload as readable. A structural
    /// contradiction is not a decode failure and still answers `true`; the
    /// real read refuses it rather than reporting it as a gap.
    pub fn payload_is_readable(kind_name: &str, kind_version: u32, data: &Value) -> bool {
        let mut probe = Self::default();
        let applied = probe.apply(HistoryRow {
            seq: 0,
            kind_name,
            kind_version,
            data,
        });
        match applied {
            Ok(()) => probe.omissions.is_empty(),
            Err(error) => !matches!(error, HistoryError::Payload { .. }),
        }
    }
}

impl MessageHistory {
    /// Every fact this fold left out because it could not be read, in sequence
    /// order. Empty means the transcript is complete; it never means "nothing
    /// was checked".
    pub fn omissions(&self) -> &[HistoryOmission] {
        &self.omissions
    }

    /// Record one fact the fold left out. The reader calls this instead of
    /// refusing the whole read, so one bad row costs that item and is named.
    pub fn omit(&mut self, omission: HistoryOmission) {
        self.omissions.push(omission);
    }

    /// Record the outcome of one row whose payload did not decode.
    ///
    /// The caller reaches this only for [`HistoryError::Payload`]: a structural
    /// contradiction (two items at one ordinal, an attempt in two messages, a
    /// terminal that precedes its opening) still refuses, because degrading it
    /// would present an unreadable transcript as a merely shorter one.
    pub(super) fn record_unreadable(
        &mut self,
        seq: u64,
        kind_name: &str,
        data: &Value,
        error: &HistoryError,
    ) {
        // Attribute the gap to a message where the payload proves one, so a
        // replay can drop that message's tool pairs rather than send a provider
        // an orphaned call or result.
        let message = self.omitted_message(kind_name, data);
        self.omit(HistoryOmission {
            seq,
            kind: kind_name.to_owned(),
            class: HistoryOmissionClass::Undecodable,
            detail: error.to_string(),
            message,
        });
    }

    /// The message an unreadable row belongs to, when its payload still yields
    /// the identity. A payload that cannot even name its message is unattached,
    /// and a usage row is attached through the attempt it names.
    fn omitted_message(&self, kind_name: &str, data: &Value) -> Option<MessageId> {
        match kind_name {
            "session.content-block.opened" => session_message::decode_block_opened(data)
                .ok()
                .map(|fact| fact.message().clone()),
            "session.content.appended" => session_message::decode_appended(data)
                .ok()
                .map(|fact| fact.message().clone()),
            "session.assistant-message.opened" => session_message::decode_opened_v2(data)
                .or_else(|_| session_message::decode_opened(data))
                .ok()
                .map(|fact| fact.message().clone()),
            "session.usage.observed" => session_message::decode_observed(data)
                .ok()
                .and_then(|fact| self.attempt_messages.get(fact.attempt()).cloned()),
            _ => None,
        }
    }
}
