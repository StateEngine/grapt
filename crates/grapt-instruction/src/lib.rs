//! Instruction epochs: content-addressed blobs, hash deltas in durable
//! events, last-write-wins per-session state.
//!
//! MEASURED LAW (frozen clone, 2026-08-08, 309 blobs): the content-address
//! law is TWO-WAY and PERFECTLY CLEAN — 283 blobs hash as sha256(raw utf8
//! value); all 26 structured values hash as sha256 of their CANONICAL
//! serialization (grapt-event::canonical_string — UTF-16 code-unit key
//! order, ES number formatting). Zero violations. A generic
//! json.dumps(sort_keys=True) approximation explains only 8 of the 26 —
//! only the true canonical bytes reproduce the store, which independently
//! validates the canonical codec a second way.

use grapt_types::{InstructionHash, InstructionHashParseError, InstructionKey, SessionId};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

mod service;

pub use service::{
    CurrentInstructions, EntryOutcome, InstructionBoundary, InstructionChange, InstructionError,
    InstructionHistory, InstructionProjector, InstructionService, InstructionUpdate,
    InstructionValue, RefreshInput, RefreshOutcome, ResolvedInstruction, SkippedInstructionEvent,
    SourceObservation,
};

/// How a stored blob relates to its hash key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobAddressing {
    RawUtf8,
    CanonicalJson,
    Violation,
}

pub fn classify_blob(hash: InstructionHash, value: &str) -> BlobAddressing {
    let raw: [u8; 32] = Sha256::digest(value.as_bytes()).into();
    if raw == *hash.as_bytes() {
        return BlobAddressing::RawUtf8;
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(value) {
        let canon = grapt_event::canonical_string(&parsed);
        let canonical: [u8; 32] = Sha256::digest(canon.as_bytes()).into();
        if canonical == *hash.as_bytes() {
            return BlobAddressing::CanonicalJson;
        }
    }
    BlobAddressing::Violation
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstructionDeltaError {
    #[error("instruction delta must be an object")]
    NotObject,
    #[error("instruction key cannot be empty")]
    EmptyKey,
    #[error("instruction delta for {key} must be a hash string or the removed sentinel")]
    NonStringValue { key: String },
    #[error("instruction delta for {key} has an invalid hash: {source}")]
    InvalidHash {
        key: String,
        #[source]
        source: InstructionHashParseError,
    },
}

/// Last-write-wins fold of `session.instructions.updated.2` deltas.
#[derive(Debug, Default)]
pub struct InstructionFold {
    /// session -> key -> hash
    pub current: HashMap<SessionId, HashMap<InstructionKey, InstructionHash>>,
    pub deltas_applied: u64,
}

impl InstructionFold {
    /// Applies one event's delta map (key -> hash string or `"removed"`).
    pub fn apply(
        &mut self,
        session: SessionId,
        delta: &Value,
    ) -> Result<(), InstructionDeltaError> {
        let Value::Object(map) = delta else {
            return Err(InstructionDeltaError::NotObject);
        };
        let decoded = map
            .iter()
            .map(|(key, hash)| {
                let instruction_key =
                    InstructionKey::new(key).ok_or(InstructionDeltaError::EmptyKey)?;
                let Value::String(hash) = hash else {
                    return Err(InstructionDeltaError::NonStringValue { key: key.clone() });
                };
                if hash == "removed" {
                    return Ok((instruction_key, None));
                }
                hash.parse::<InstructionHash>()
                    .map(|hash| (instruction_key, Some(hash)))
                    .map_err(|source| InstructionDeltaError::InvalidHash {
                        key: key.clone(),
                        source,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let state = self.current.entry(session).or_default();
        for (key, hash) in decoded {
            match hash {
                Some(hash) => {
                    state.insert(key, hash);
                }
                None => {
                    state.remove(&key);
                }
            }
            self.deltas_applied += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn raw_text_blobs_classify_as_raw_utf8() -> Result<(), Box<dyn std::error::Error>> {
        // sha256("hello") well-known value.
        assert_eq!(
            classify_blob(
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".parse()?,
                "hello"
            ),
            BlobAddressing::RawUtf8
        );
        Ok(())
    }

    #[test]
    fn mismatched_blob_is_a_violation() {
        assert_eq!(
            classify_blob(InstructionHash::from_bytes([0; 32]), "hello"),
            BlobAddressing::Violation
        );
    }

    #[test]
    fn fold_is_last_write_wins_per_key() -> Result<(), Box<dyn std::error::Error>> {
        let mut fold = InstructionFold::default();
        fold.apply(
            "ses_a".parse()?,
            &json!({"core/date": "aa".repeat(32), "core/env": "bb".repeat(32)}),
        )?;
        fold.apply("ses_a".parse()?, &json!({"core/date": "cc".repeat(32)}))?;
        let session: SessionId = "ses_a".parse()?;
        let date = InstructionKey::new("core/date").ok_or("date key")?;
        let env = InstructionKey::new("core/env").ok_or("env key")?;
        assert_eq!(fold.current[&session][&date].to_string(), "cc".repeat(32));
        assert_eq!(fold.current[&session][&env].to_string(), "bb".repeat(32));
        assert_eq!(fold.deltas_applied, 3);
        Ok(())
    }

    #[test]
    fn removed_sentinel_deletes_the_prior_value() -> Result<(), Box<dyn std::error::Error>> {
        let mut fold = InstructionFold::default();
        fold.apply(
            "ses_a".parse()?,
            &json!({"core/date": "aa".repeat(32), "core/env": "bb".repeat(32)}),
        )?;
        fold.apply("ses_a".parse()?, &json!({"core/date": "removed"}))?;
        let session: SessionId = "ses_a".parse()?;
        let date = InstructionKey::new("core/date").ok_or("date key")?;
        let env = InstructionKey::new("core/env").ok_or("env key")?;
        assert!(!fold.current[&session].contains_key(&date));
        assert_eq!(fold.current[&session][&env].to_string(), "bb".repeat(32));
        assert_eq!(fold.deltas_applied, 3);
        Ok(())
    }

    #[test]
    fn malformed_delta_does_not_half_apply() -> Result<(), Box<dyn std::error::Error>> {
        let mut fold = InstructionFold::default();
        let result = fold.apply(
            "ses_a".parse()?,
            &json!({"core/date": "aa".repeat(32), "core/env": 42}),
        );
        assert!(matches!(
            result,
            Err(InstructionDeltaError::NonStringValue { .. })
        ));
        assert!(fold.current.is_empty());
        assert_eq!(fold.deltas_applied, 0);
        Ok(())
    }
}
