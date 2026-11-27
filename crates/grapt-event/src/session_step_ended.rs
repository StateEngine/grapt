//! The strict canonical payload contract for `session.step.ended.2`.
//!
//! # What this owns
//!
//! The Rust-native durable Step SUCCESS terminal: a positive [`StepNumber`] and
//! one [`ProviderCompletionDisposition`], encoded through this crate's existing
//! [`crate::canonical`] writer so byte and hash determinism are inherited
//! rather than re-invented, and decoded by a hand-rolled fail-closed reader in
//! the same style as [`crate::session_step_failed`].
//!
//! Until this module existed, a Step that SUCCEEDED wrote nothing durable. Every
//! failing Step wrote `session.step.failed.2`; the success case had a declared
//! kind, a registered name and no producer, which is why nothing was red — there
//! was no gate to fail.
//!
//! v1 of the same name is the frozen oracle payload — `{sessionID,
//! assistantMessageID, finish, cost, tokens, snapshot, files}` — and is NOT this
//! contract. Three of those coordinates have no producer in this lane, and its
//! `finish` is the raw provider `FinishReason` that R-l1 keeps out of durable
//! bytes entirely. Decode keys on the exact `(name, version)` pair, so the two
//! shapes can never answer for each other, and **no v1 byte changes because
//! this module exists**.
//!
//! # Why a disposition and never a `FinishReason`
//!
//! `FinishReason` is declared in `grapt-provider`, which depends on this
//! crate's dependency `grapt-types`; naming it here would invert that
//! direction. The load-bearing reason is not layering hygiene though — it is
//! that a durable schema must not move when a provider adds a finish reason.
//! Routing the fact through the normalized disposition makes a new
//! `FinishReason` variant a compile error at one named site instead of a silent
//! change to stored bytes.
//!
//! # What the payload deliberately does NOT carry
//!
//! **Session identity.** The aggregate is the session. `EventEnvelopeV2` already
//! carries `AggregateId`, `EventId`, `EventSeq`, `WriterEpoch` and `created`;
//! repeating any of them here would be a second carrier of one identity, and two
//! carriers of one fact are how they drift apart.
//!
//! **Cost and token usage.** The oracle's v1 payload carried both. They are
//! `AttemptUsage`'s, recorded per physical attempt with their observation phase
//! and reported-versus-estimated provenance; folding a flattened copy into the
//! Step terminal would create a second, phase-free accounting truth.

use grapt_types::{ProviderCompletionDisposition, StepNumber};
use serde_json::{Map, Value};

const AT: &str = "session.step.ended.2";

/// One durable Step success terminal.
///
/// Fields are private and there is one constructor: a caller cannot build a
/// terminal from a literal, and cannot construct one carrying a step number of
/// zero because [`StepNumber`] itself refuses that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepEndedFact {
    step: StepNumber,
    completion: ProviderCompletionDisposition,
}

impl StepEndedFact {
    /// The only constructor.
    pub fn new(step: StepNumber, completion: ProviderCompletionDisposition) -> Self {
        Self { step, completion }
    }

    pub fn step(&self) -> StepNumber {
        self.step
    }

    pub fn completion(&self) -> ProviderCompletionDisposition {
        self.completion
    }
}

/// Why a `session.step.ended.2` payload is not a fact.
///
/// The categories are separate because they are separate defects: a missing
/// field is a writer that never had the coordinate, an explicit null is a
/// writer that had it and erased it, a wrong type is a writer that meant
/// something else, an unknown field is a writer this contract has not been
/// reviewed against, and an unknown variant is a vocabulary this build does not
/// share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    /// The payload is not a JSON object at all.
    NotAnObject { at: &'static str },
    /// A required field is absent.
    MissingField { at: &'static str, field: String },
    /// A field is present as an explicit `null`.
    NullField { at: &'static str, field: String },
    /// A field is present with the wrong JSON type or an invalid value.
    InvalidField {
        at: &'static str,
        field: String,
        expected: &'static str,
    },
    /// A field this contract does not name.
    UnknownField { at: &'static str, field: String },
    /// A closed vocabulary this build does not share.
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

// ---------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------

/// Encode the fact as canonical JSON.
pub fn encode(fact: &StepEndedFact) -> Value {
    let mut root = Map::new();
    root.insert("step".to_owned(), Value::from(fact.step.value()));
    root.insert(
        "completion".to_owned(),
        Value::from(completion_tag(fact.completion)),
    );
    Value::Object(root)
}

/// Canonical bytes, for hashing and durable storage.
pub fn encode_canonical_bytes(fact: &StepEndedFact) -> Vec<u8> {
    crate::canonical::canonical_string(&encode(fact)).into_bytes()
}

/// The wire spelling of each disposition.
///
/// These four tags are deliberately the same spellings
/// [`crate::session_step_failed`] uses for the disposition it carries inside
/// `StepTerminalReason::Ended`. One vocabulary, spelled one way, whichever
/// terminal carries it.
fn completion_tag(completion: ProviderCompletionDisposition) -> &'static str {
    match completion {
        ProviderCompletionDisposition::AssertedComplete => "asserted-complete",
        ProviderCompletionDisposition::AssertedContinue => "asserted-continue",
        ProviderCompletionDisposition::Truncated => "truncated",
        ProviderCompletionDisposition::Unstated => "unstated",
    }
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// Decode a payload, failing closed.
///
/// Unknown fields are refused rather than ignored: an unrecognized coordinate
/// means the bytes were written by a contract this build has not been reviewed
/// against, and silently dropping it is how a durable field becomes optional by
/// accident.
pub fn decode(value: &Value) -> Result<StepEndedFact, PayloadError> {
    let root = object(value, AT)?;
    let step = required_u64(root, AT, "step")?;
    let step = u32::try_from(step)
        .ok()
        .and_then(StepNumber::new)
        .ok_or_else(|| invalid(AT, "step", "a positive step number"))?;
    let completion = completion_of(required_str(root, AT, "completion")?)?;
    reject_unknown(root, AT, &["step", "completion"])?;
    Ok(StepEndedFact::new(step, completion))
}

fn completion_of(raw: &str) -> Result<ProviderCompletionDisposition, PayloadError> {
    match raw {
        "asserted-complete" => Ok(ProviderCompletionDisposition::AssertedComplete),
        "asserted-continue" => Ok(ProviderCompletionDisposition::AssertedContinue),
        "truncated" => Ok(ProviderCompletionDisposition::Truncated),
        "unstated" => Ok(ProviderCompletionDisposition::Unstated),
        other => Err(PayloadError::UnknownVariant {
            at: AT,
            field: "completion".to_owned(),
            value: other.to_owned(),
        }),
    }
}

fn object<'a>(value: &'a Value, at: &'static str) -> Result<&'a Map<String, Value>, PayloadError> {
    value.as_object().ok_or(PayloadError::NotAnObject { at })
}

fn required<'a>(
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

fn required_str<'a>(
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

fn invalid(at: &'static str, field: &str, expected: &'static str) -> PayloadError {
    PayloadError::InvalidField {
        at,
        field: field.to_owned(),
        expected,
    }
}

fn reject_unknown(
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
