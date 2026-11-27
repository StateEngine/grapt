//! Strict Rust-native payload for `session.tool.terminal.1`.
//!
//! The Session is the envelope aggregate. The payload carries only the
//! Physical Attempt, call identity, and one closed terminal outcome. Output and
//! failure detail are paired artifact references; raw tool bytes and strings
//! are never durable terminal control flow.

use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition, InterruptionSource,
    PhysicalAttemptId, ToolCallId, ToolFailureReason, ToolTerminalArtifact, ToolTerminalOutcome,
};
use serde_json::{Map, Value};

const AT: &str = "session.tool.terminal.1";
const AT_OUTCOME: &str = "session.tool.terminal.1.outcome";
const AT_OUTPUT: &str = "session.tool.terminal.1.outcome.output";
const AT_DETAIL: &str = "session.tool.terminal.1.outcome.detail";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTerminalFact {
    attempt: PhysicalAttemptId,
    call: ToolCallId,
    outcome: ToolTerminalOutcome,
}

impl ToolTerminalFact {
    pub fn new(attempt: PhysicalAttemptId, call: ToolCallId, outcome: ToolTerminalOutcome) -> Self {
        Self {
            attempt,
            call,
            outcome,
        }
    }

    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    pub fn call(&self) -> &ToolCallId {
        &self.call
    }

    pub fn outcome(&self) -> &ToolTerminalOutcome {
        &self.outcome
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    NotAnObject {
        at: &'static str,
    },
    MissingField {
        at: &'static str,
        field: String,
    },
    NullField {
        at: &'static str,
        field: String,
    },
    InvalidField {
        at: &'static str,
        field: String,
        expected: &'static str,
    },
    UnknownField {
        at: &'static str,
        field: String,
    },
    UnknownVariant {
        at: &'static str,
        field: String,
        value: String,
    },
}

impl core::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAnObject { at } => write!(f, "{at}: value is not an object"),
            Self::MissingField { at, field } => {
                write!(f, "{at}: required field `{field}` is missing")
            }
            Self::NullField { at, field } => {
                write!(f, "{at}: field `{field}` is explicitly null")
            }
            Self::InvalidField {
                at,
                field,
                expected,
            } => write!(f, "{at}: field `{field}` is invalid; expected {expected}"),
            Self::UnknownField { at, field } => write!(f, "{at}: unknown field `{field}`"),
            Self::UnknownVariant { at, field, value } => {
                write!(f, "{at}: field `{field}` has unknown variant `{value}`")
            }
        }
    }
}

impl std::error::Error for PayloadError {}

pub fn encode(fact: &ToolTerminalFact) -> Value {
    let mut root = Map::new();
    root.insert("attempt".to_owned(), Value::from(fact.attempt.as_str()));
    root.insert("call".to_owned(), Value::from(fact.call.as_str()));
    root.insert("outcome".to_owned(), encode_outcome(&fact.outcome));
    Value::Object(root)
}

pub fn encode_canonical_bytes(fact: &ToolTerminalFact) -> Vec<u8> {
    crate::canonical::canonical_string(&encode(fact)).into_bytes()
}

/// How every Session reader classifies one stored terminal row.
///
/// A row that is not JSON, or not a valid terminal, is unreadable: it proves
/// nothing about any call. Readers treat it as absent evidence and warn about
/// it; they never stop the Session on it. Writers stay strict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredTerminal {
    Readable(ToolTerminalFact),
    /// `call` is the call the row names, when it names one as text: a hint
    /// for a warning, never evidence of that call's outcome.
    Unreadable {
        call: Option<ToolCallId>,
    },
}

/// A stored terminal is a few identities and an artifact reference. A row
/// longer than this is not one: readers classify it without fetching it.
pub const MAX_STORED_TERMINAL_BYTES: usize = 64 * 1024;

impl StoredTerminal {
    /// Classifies stored bytes. Text that is not UTF-8, and a row past
    /// [`MAX_STORED_TERMINAL_BYTES`], read as unreadable rather than failing.
    pub fn read(data: &[u8]) -> Self {
        if data.len() > MAX_STORED_TERMINAL_BYTES {
            return Self::Unreadable { call: None };
        }
        let Ok(value) = serde_json::from_slice::<Value>(data) else {
            return Self::Unreadable { call: None };
        };
        match decode(&value) {
            Ok(fact) => Self::Readable(fact),
            Err(_) => Self::Unreadable {
                call: value
                    .get("call")
                    .and_then(Value::as_str)
                    .and_then(ToolCallId::new),
            },
        }
    }
}

pub fn decode(value: &Value) -> Result<ToolTerminalFact, PayloadError> {
    let root = object(value, AT)?;
    let attempt = PhysicalAttemptId::new(required_str(root, AT, "attempt")?)
        .ok_or_else(|| invalid(AT, "attempt", "a non-empty PhysicalAttemptId"))?;
    let call = ToolCallId::new(required_str(root, AT, "call")?)
        .ok_or_else(|| invalid(AT, "call", "a non-empty ToolCallId"))?;
    let outcome = decode_outcome(required(root, AT, "outcome")?)?;
    reject_unknown(root, AT, &["attempt", "call", "outcome"])?;
    Ok(ToolTerminalFact::new(attempt, call, outcome))
}

fn encode_outcome(outcome: &ToolTerminalOutcome) -> Value {
    let mut out = Map::new();
    match outcome {
        ToolTerminalOutcome::Completed { output } => {
            out.insert("kind".to_owned(), Value::from("completed"));
            if let Some(output) = output {
                out.insert("output".to_owned(), encode_artifact(output));
            }
        }
        ToolTerminalOutcome::Failed { reason, detail } => {
            out.insert("kind".to_owned(), Value::from("failed"));
            out.insert("reason".to_owned(), Value::from(failure_tag(*reason)));
            if let Some(detail) = detail {
                out.insert("detail".to_owned(), encode_artifact(detail));
            }
        }
        ToolTerminalOutcome::Cancelled { by } => {
            out.insert("kind".to_owned(), Value::from("cancelled"));
            out.insert("by".to_owned(), Value::from(interruption_tag(*by)));
        }
        ToolTerminalOutcome::ResultMissing => {
            out.insert("kind".to_owned(), Value::from("result-missing"));
        }
    }
    Value::Object(out)
}

fn decode_outcome(value: &Value) -> Result<ToolTerminalOutcome, PayloadError> {
    let out = object(value, AT_OUTCOME)?;
    let kind = required_str(out, AT_OUTCOME, "kind")?;
    let outcome = match kind {
        "completed" => {
            let output = optional_artifact(out, AT_OUTCOME, "output", AT_OUTPUT)?;
            reject_unknown(out, AT_OUTCOME, &["kind", "output"])?;
            ToolTerminalOutcome::Completed { output }
        }
        "failed" => {
            let reason = failure_of(required_str(out, AT_OUTCOME, "reason")?)?;
            let detail = optional_artifact(out, AT_OUTCOME, "detail", AT_DETAIL)?;
            reject_unknown(out, AT_OUTCOME, &["kind", "reason", "detail"])?;
            ToolTerminalOutcome::Failed { reason, detail }
        }
        "cancelled" => {
            let by = interruption_of(required_str(out, AT_OUTCOME, "by")?)?;
            reject_unknown(out, AT_OUTCOME, &["kind", "by"])?;
            ToolTerminalOutcome::Cancelled { by }
        }
        "result-missing" => {
            reject_unknown(out, AT_OUTCOME, &["kind"])?;
            ToolTerminalOutcome::ResultMissing
        }
        other => {
            return Err(PayloadError::UnknownVariant {
                at: AT_OUTCOME,
                field: "kind".to_owned(),
                value: other.to_owned(),
            })
        }
    };
    Ok(outcome)
}

pub(crate) fn encode_artifact(artifact: &ToolTerminalArtifact) -> Value {
    let reference = artifact.reference();
    let mut out = Map::new();
    out.insert("id".to_owned(), Value::from(reference.id().as_str()));
    out.insert("length".to_owned(), Value::from(reference.length()));
    out.insert(
        "digest".to_owned(),
        Value::from(reference.digest().as_str()),
    );
    match artifact.disposition() {
        CaptureDisposition::Complete => {
            out.insert("disposition".to_owned(), Value::from("complete"));
        }
        CaptureDisposition::TruncatedAtBoundary { captured } => {
            out.insert("disposition".to_owned(), Value::from("truncated"));
            out.insert("captured".to_owned(), Value::from(captured));
        }
    }
    Value::Object(out)
}

fn optional_artifact(
    object: &Map<String, Value>,
    at: &'static str,
    field: &str,
    artifact_at: &'static str,
) -> Result<Option<ToolTerminalArtifact>, PayloadError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Err(PayloadError::NullField {
            at,
            field: field.to_owned(),
        });
    }
    decode_artifact(value, artifact_at).map(Some)
}

pub(crate) fn decode_artifact(
    value: &Value,
    at: &'static str,
) -> Result<ToolTerminalArtifact, PayloadError> {
    let object = object(value, at)?;
    let id = ArtifactId::new(required_str(object, at, "id")?)
        .ok_or_else(|| invalid(at, "id", "a non-empty ArtifactId"))?;
    let length = required_u64(object, at, "length")?;
    let digest = ArtifactDigest::new(required_str(object, at, "digest")?)
        .ok_or_else(|| invalid(at, "digest", "a non-empty ArtifactDigest"))?;
    let disposition = match required_str(object, at, "disposition")? {
        "complete" => {
            reject_unknown(object, at, &["id", "length", "digest", "disposition"])?;
            CaptureDisposition::Complete
        }
        "truncated" => {
            let captured = required_u64(object, at, "captured")?;
            reject_unknown(
                object,
                at,
                &["id", "length", "digest", "disposition", "captured"],
            )?;
            CaptureDisposition::TruncatedAtBoundary { captured }
        }
        other => {
            return Err(PayloadError::UnknownVariant {
                at,
                field: "disposition".to_owned(),
                value: other.to_owned(),
            })
        }
    };
    let reference = ArtifactRef::new(id, length, digest);
    ToolTerminalArtifact::new(reference, disposition)
        .ok_or_else(|| invalid(at, "captured", "the exact artifact length"))
}

fn failure_tag(reason: ToolFailureReason) -> &'static str {
    match reason {
        ToolFailureReason::AuthorityUnavailable => "authority-unavailable",
        ToolFailureReason::ApprovalUnavailable => "approval-unavailable",
        ToolFailureReason::EffectFailed => "effect-failed",
        ToolFailureReason::CaptureFailed => "capture-failed",
        ToolFailureReason::EffectUnknown => "effect-unknown",
    }
}

fn failure_of(raw: &str) -> Result<ToolFailureReason, PayloadError> {
    match raw {
        "authority-unavailable" => Ok(ToolFailureReason::AuthorityUnavailable),
        "approval-unavailable" => Ok(ToolFailureReason::ApprovalUnavailable),
        "effect-failed" => Ok(ToolFailureReason::EffectFailed),
        "capture-failed" => Ok(ToolFailureReason::CaptureFailed),
        "effect-unknown" => Ok(ToolFailureReason::EffectUnknown),
        other => Err(PayloadError::UnknownVariant {
            at: AT_OUTCOME,
            field: "reason".to_owned(),
            value: other.to_owned(),
        }),
    }
}

fn interruption_tag(by: InterruptionSource) -> &'static str {
    match by {
        InterruptionSource::User => "user",
        InterruptionSource::ManagedShutdown => "managed-shutdown",
    }
}

fn interruption_of(raw: &str) -> Result<InterruptionSource, PayloadError> {
    match raw {
        "user" => Ok(InterruptionSource::User),
        "managed-shutdown" => Ok(InterruptionSource::ManagedShutdown),
        other => Err(PayloadError::UnknownVariant {
            at: AT_OUTCOME,
            field: "by".to_owned(),
            value: other.to_owned(),
        }),
    }
}

pub(crate) fn object<'a>(
    value: &'a Value,
    at: &'static str,
) -> Result<&'a Map<String, Value>, PayloadError> {
    value.as_object().ok_or(PayloadError::NotAnObject { at })
}

pub(crate) fn required<'a>(
    object: &'a Map<String, Value>,
    at: &'static str,
    field: &str,
) -> Result<&'a Value, PayloadError> {
    match object.get(field) {
        None => Err(PayloadError::MissingField {
            at,
            field: field.to_owned(),
        }),
        Some(Value::Null) => Err(PayloadError::NullField {
            at,
            field: field.to_owned(),
        }),
        Some(value) => Ok(value),
    }
}

pub(crate) fn required_str<'a>(
    object: &'a Map<String, Value>,
    at: &'static str,
    field: &str,
) -> Result<&'a str, PayloadError> {
    required(object, at, field)?
        .as_str()
        .ok_or_else(|| invalid(at, field, "a string"))
}

fn required_u64(
    object: &Map<String, Value>,
    at: &'static str,
    field: &str,
) -> Result<u64, PayloadError> {
    required(object, at, field)?
        .as_u64()
        .ok_or_else(|| invalid(at, field, "a non-negative integer"))
}

pub(crate) fn invalid(at: &'static str, field: &str, expected: &'static str) -> PayloadError {
    PayloadError::InvalidField {
        at,
        field: field.to_owned(),
        expected,
    }
}

pub(crate) fn reject_unknown(
    object: &Map<String, Value>,
    at: &'static str,
    known: &[&str],
) -> Result<(), PayloadError> {
    for field in object.keys() {
        if !known.contains(&field.as_str()) {
            return Err(PayloadError::UnknownField {
                at,
                field: field.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
