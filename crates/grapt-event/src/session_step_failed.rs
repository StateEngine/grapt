//! The canonical payload contract for `session.step.failed.2`: strict on
//! integrity, tolerant on vocabulary.
//!
//! # What this owns
//!
//! The Rust-native durable Step terminal: a positive [`StepNumber`] and a typed
//! [`StepTerminalReason`], encoded through this crate's existing
//! [`crate::canonical`] writer so byte and hash determinism are inherited
//! rather than re-invented, and decoded by a hand-rolled reader that is
//! strict on integrity and tolerant on vocabulary, in the same style as
//! [`crate::session_compaction`].
//!
//! v1 of the same name is the frozen oracle payload — `{sessionID,
//! assistantMessageID, error: {type, message}, cost, tokens, snapshot, files}`
//! — and is NOT this contract. Its `error.type` is exactly the bare-string
//! carrier R2a removed from the runtime. Decode keys on the exact
//! `(name, version)` pair, so the two shapes can never answer for each other,
//! and no v1 byte changes because this module exists.
//!
//! # What the payload deliberately does NOT carry
//!
//! **Session identity.** The aggregate is the session. `EventEnvelopeV2`
//! already carries `AggregateId`, `EventId`, `EventSeq`, `WriterEpoch` and
//! `created`; repeating any of them here would be a second carrier of one
//! identity, and two carriers of one fact are how they drift apart.
//!
//! **The diagnostic.** R2a's `BoundedDetail` is non-authoritative by its own
//! documentation — nothing branches on it — and it lives in `grapt-provider`,
//! which this crate may not depend on. Rather than mint a second bounded string
//! here and end up with two carriers of one bound, the durable fact carries
//! authority-bearing typed state only (R2b ruling 1). A diagnostic, if one is
//! ever wanted durably, is a later explicit decision. A 2026-10-09
//! ruling admits closed diagnostic codes in an advisory, postcommit
//! Session notice; this canonical terminal contract still carries no detail.
//!
//! **The v1 compatibility coordinates.** No `assistantMessageID`, `cost`,
//! `tokens`, `snapshot` or `files`. Usage has its own vocabulary with per-field
//! optionality, phase and provenance; re-deriving it here would fork it.
//! Whether a future `grapt-api` publicly projects v2 — and if so how a typed
//! ten-variant reason maps onto v1's `{type, message}` — is an explicit
//! residual, not this codec's decision.
//!
//! # Integrity is strict, vocabulary is tolerant
//!
//! A payload is refused when a REQUIRED coordinate is missing, when it is
//! explicitly `null`, when it has the wrong JSON type, or when a REQUIRED
//! word is blank — anywhere the contract requires a word, known or not.
//! Those are the integrity faults: a broken record this build shares.
//!
//! Words are the future's property. A NON-EMPTY `kind` this build does not
//! know, or an unknown non-empty word inside a known kind, decodes to
//! [`StepTerminalReason::Unknown`] with a byte-capped tag qualified by the
//! path it was read from — never a refusal, never a guess at what the word
//! meant. A blank is no word at all, and no terminal evidence is built from
//! it. Unknown extra fields are ignored at every level. A later writer may
//! add a reason kind and may add optional fields; it may not add a REQUIRED
//! field to an existing kind, because a known kind with a broken required
//! coordinate is integrity, not vocabulary, and is refused.
//!
//! # `Ended` is refused, explicitly
//!
//! [`StepTerminalReason`] spans both outcomes of a Step, but this is the
//! FAILURE fact. `kind: "ended"` is refused by decode as a constructor fact,
//! through an explicit match arm rather than a catch-all, and the refusal
//! holds even when the completion word is itself unknown: the kind is the
//! fact, and the kind is not a failure. That is what separates "this build
//! does not know that word" (vocabulary: it decodes to `Unknown`) from "that
//! word is known and is not a failure" (fact: it is refused), so a reader can
//! always tell a version skew from an illegal write.

use grapt_types::{
    AccountId, AgentId, AmbiguityEvidence, AuthorityPrincipal, ContextFailure, ExternalEffectState,
    HookId, InterruptionSource, LayerName, PolicyVersion, ProviderCompletionDisposition,
    ProviderFailureClass, StepNumber, StepTerminalReason,
};
use serde_json::{Map, Value};

/// The durable Step-failure fact.
///
/// Fields are private and construction is checked: the only way to build one is
/// [`StepFailedFact::new`], which refuses a success reason. A public struct
/// literal would let a caller assemble the one shape this contract exists to
/// forbid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepFailedFact {
    step: StepNumber,
    reason: StepTerminalReason,
}

/// Why a candidate is not a Step-failure fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactError {
    /// `StepTerminalReason::Ended` is a SUCCESS terminal. A Step that ended
    /// writes `session.step.ended`; this kind is the failure fact and cannot
    /// represent it.
    NotAFailure,
    /// `StepTerminalReason::Unknown` is produced by decode only. A writer in
    /// this build that could construct it would hold a durable fact it cannot
    /// re-encode, so the checked constructor refuses it, and the encoders
    /// refuse it a second time at the boundary.
    DecodeOnly,
}

impl std::fmt::Display for FactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FactError::NotAFailure => f.write_str(
                "session.step.failed.2 cannot carry StepTerminalReason::Ended: a Step that ended \
                 successfully is session.step.ended, not a failure",
            ),
            FactError::DecodeOnly => f.write_str(
                "StepTerminalReason::Unknown is produced by decode only: this build's writers \
                 cannot construct it, and its encoders refuse to write it back",
            ),
        }
    }
}

impl std::error::Error for FactError {}

impl StepFailedFact {
    /// The only CONSTRUCTING constructor, for writers in this build.
    ///
    /// The `Ended` and `Unknown` arms are matched EXPLICITLY rather than
    /// through a wildcard. That is deliberate: a wildcard would keep
    /// compiling when an eleventh variant is added, silently admitting it as
    /// a failure. This way the compiler asks. `Unknown` is the decode-only
    /// variant: a decoded fact is built by [`StepFailedFact::from_decoded`],
    /// never through here.
    pub fn new(step: StepNumber, reason: StepTerminalReason) -> Result<Self, FactError> {
        match &reason {
            StepTerminalReason::Ended { .. } => Err(FactError::NotAFailure),
            StepTerminalReason::Unknown { .. } => Err(FactError::DecodeOnly),
            StepTerminalReason::ModelAssertedComplete
            | StepTerminalReason::PolicyLimit { .. }
            | StepTerminalReason::BudgetExhausted { .. }
            | StepTerminalReason::Interrupted { .. }
            | StepTerminalReason::ProviderFailure { .. }
            | StepTerminalReason::ContextFailure { .. }
            | StepTerminalReason::AuthorityRefused { .. }
            | StepTerminalReason::DurabilityFailure
            | StepTerminalReason::ProcessLoss { .. } => Ok(Self { step, reason }),
        }
    }

    /// The decode-side constructor.
    ///
    /// Decode alone may read a durable record written by a build whose
    /// vocabulary this one does not share, and that record decodes to
    /// `StepTerminalReason::Unknown`. This constructor admits exactly what
    /// decode can produce — including `Unknown` — and nothing more, so the
    /// writer path stays closed: the only way out of this crate with an
    /// `Unknown` fact is a refusal, which the encoders carry.
    fn from_decoded(step: StepNumber, reason: StepTerminalReason) -> Self {
        Self { step, reason }
    }

    pub fn step(&self) -> StepNumber {
        self.step
    }

    pub fn reason(&self) -> &StepTerminalReason {
        &self.reason
    }
}

/// Why a `session.step.failed.2` payload is not a fact.
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
    /// A closed vocabulary received a value outside it.
    UnknownVariant {
        at: &'static str,
        field: String,
        value: String,
    },
    /// The payload decoded, but the fact's own constructor refused it. Carried
    /// rather than flattened so a structurally valid payload that is not a
    /// legal failure stays distinguishable from a malformed one.
    RefusedByConstructor { failure: FactError },
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PayloadError::NotAnObject { at } => write!(f, "{at}: payload is not a JSON object"),
            PayloadError::MissingField { at, field } => {
                write!(f, "{at}: required field `{field}` is missing")
            }
            PayloadError::NullField { at, field } => {
                write!(f, "{at}: field `{field}` is explicitly null")
            }
            PayloadError::InvalidField {
                at,
                field,
                expected,
            } => {
                write!(f, "{at}: field `{field}` is not {expected}")
            }
            PayloadError::UnknownField { at, field } => {
                write!(f, "{at}: unknown field `{field}`")
            }
            PayloadError::UnknownVariant { at, field, value } => {
                write!(f, "{at}: field `{field}` has unknown variant `{value}`")
            }
            PayloadError::RefusedByConstructor { failure } => {
                write!(
                    f,
                    "payload is structurally valid but not a failure: {failure}"
                )
            }
        }
    }
}

impl std::error::Error for PayloadError {}

const AT: &str = "session.step.failed.2";
const AT_REASON: &str = "session.step.failed.2.reason";
const AT_PRINCIPAL: &str = "session.step.failed.2.reason.principal";
const AT_EFFECT: &str = "session.step.failed.2.reason.effect";

// ---------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------

/// Encode the fact as canonical JSON.
///
/// Fallible on one named refusal: a fact holding
/// [`StepTerminalReason::Unknown`] is decode-only, and no encoder may write
/// it back. Writers in this build cannot reach it through
/// [`StepFailedFact::new`], but the signature locks the door anyway: a
/// decoded fact passed to a store seam is refused here, never written as a
/// foreign vocabulary this build cannot own.
pub fn encode(fact: &StepFailedFact) -> Result<Value, FactError> {
    let mut root = Map::new();
    root.insert("step".to_owned(), Value::from(fact.step.value()));
    root.insert(
        "reason".to_owned(),
        Value::Object(encode_reason(&fact.reason)?),
    );
    Ok(Value::Object(root))
}

/// Canonical bytes, for hashing and durable storage.
pub fn encode_canonical_bytes(fact: &StepFailedFact) -> Result<Vec<u8>, FactError> {
    Ok(crate::canonical::canonical_string(&encode(fact)?).into_bytes())
}

/// The tagged encoding of every reason this fact may carry.
///
/// `Ended` is unreachable here because [`StepFailedFact::new`] refuses it, and
/// `Unknown` is refused by this encoder, but each is matched explicitly rather
/// than collapsed into a wildcard: the match is the place a future eleventh
/// variant must be considered, and a `_` arm would remove that place. The
/// `Ended` arm is a dead honest path; the `Unknown` arm is the door the
/// fallible signature exists to lock.
fn encode_reason(reason: &StepTerminalReason) -> Result<Map<String, Value>, FactError> {
    let mut out = Map::new();
    match reason {
        StepTerminalReason::Ended { completion } => {
            out.insert("kind".to_owned(), Value::from("ended"));
            out.insert(
                "completion".to_owned(),
                Value::from(completion_tag(*completion)),
            );
        }
        StepTerminalReason::ModelAssertedComplete => {
            out.insert("kind".to_owned(), Value::from("model-asserted-complete"));
        }
        StepTerminalReason::PolicyLimit { policy } => {
            out.insert("kind".to_owned(), Value::from("policy-limit"));
            out.insert("policy".to_owned(), Value::from(policy.as_str()));
        }
        StepTerminalReason::BudgetExhausted { policy } => {
            out.insert("kind".to_owned(), Value::from("budget-exhausted"));
            out.insert("policy".to_owned(), Value::from(policy.as_str()));
        }
        StepTerminalReason::Interrupted { by } => {
            out.insert("kind".to_owned(), Value::from("interrupted"));
            out.insert("by".to_owned(), Value::from(interruption_tag(*by)));
        }
        StepTerminalReason::ProviderFailure { class } => {
            out.insert("kind".to_owned(), Value::from("provider-failure"));
            out.insert("class".to_owned(), Value::from(class_tag(*class)));
        }
        StepTerminalReason::ContextFailure { failure } => {
            out.insert("kind".to_owned(), Value::from("context-failure"));
            out.insert("failure".to_owned(), Value::from(context_tag(*failure)));
        }
        StepTerminalReason::AuthorityRefused { principal } => {
            out.insert("kind".to_owned(), Value::from("authority-refused"));
            out.insert("principal".to_owned(), encode_principal(principal));
        }
        StepTerminalReason::DurabilityFailure => {
            out.insert("kind".to_owned(), Value::from("durability-failure"));
        }
        StepTerminalReason::ProcessLoss { effect } => {
            out.insert("kind".to_owned(), Value::from("process-loss"));
            out.insert("effect".to_owned(), encode_effect(effect));
        }
        StepTerminalReason::Unknown { .. } => return Err(FactError::DecodeOnly),
    }
    Ok(out)
}

fn encode_principal(principal: &AuthorityPrincipal) -> Value {
    let mut out = Map::new();
    match principal {
        AuthorityPrincipal::Human { account } => {
            out.insert("kind".to_owned(), Value::from("human"));
            out.insert("account".to_owned(), Value::from(account.as_str()));
        }
        AuthorityPrincipal::System => {
            out.insert("kind".to_owned(), Value::from("system"));
        }
        AuthorityPrincipal::PolicyLayer { layer } => {
            out.insert("kind".to_owned(), Value::from("policy-layer"));
            out.insert("layer".to_owned(), Value::from(layer.as_str()));
        }
        AuthorityPrincipal::Hook { hook } => {
            out.insert("kind".to_owned(), Value::from("hook"));
            out.insert("hook".to_owned(), Value::from(hook.as_str()));
        }
        AuthorityPrincipal::AutomatedReviewer { agent } => {
            out.insert("kind".to_owned(), Value::from("automated-reviewer"));
            out.insert("agent".to_owned(), Value::from(agent.as_str()));
        }
    }
    Value::Object(out)
}

fn encode_effect(effect: &ExternalEffectState) -> Value {
    let mut out = Map::new();
    match effect {
        ExternalEffectState::NoneInitiated => {
            out.insert("kind".to_owned(), Value::from("none-initiated"));
        }
        ExternalEffectState::Settled => {
            out.insert("kind".to_owned(), Value::from("settled"));
        }
        ExternalEffectState::Ambiguous { evidence } => {
            out.insert("kind".to_owned(), Value::from("ambiguous"));
            out.insert("evidence".to_owned(), Value::from(evidence.as_str()));
        }
    }
    Value::Object(out)
}

fn completion_tag(completion: ProviderCompletionDisposition) -> &'static str {
    match completion {
        ProviderCompletionDisposition::AssertedComplete => "asserted-complete",
        ProviderCompletionDisposition::AssertedContinue => "asserted-continue",
        ProviderCompletionDisposition::Truncated => "truncated",
        ProviderCompletionDisposition::Unstated => "unstated",
    }
}

fn interruption_tag(by: InterruptionSource) -> &'static str {
    match by {
        InterruptionSource::User => "user",
        InterruptionSource::ManagedShutdown => "managed-shutdown",
    }
}

fn class_tag(class: ProviderFailureClass) -> &'static str {
    match class {
        ProviderFailureClass::RateLimit => "rate-limit",
        ProviderFailureClass::ProviderInternal => "provider-internal",
        ProviderFailureClass::Transport => "transport",
        ProviderFailureClass::NoProgressTimeout => "no-progress-timeout",
        ProviderFailureClass::Authentication => "authentication",
        ProviderFailureClass::Quota => "quota",
        ProviderFailureClass::ContentPolicy => "content-policy",
        ProviderFailureClass::InvalidRequest => "invalid-request",
        ProviderFailureClass::InvalidProviderOutput => "invalid-provider-output",
        ProviderFailureClass::UnknownProvider => "unknown-provider",
        ProviderFailureClass::NoRoute => "no-route",
        ProviderFailureClass::LocalRefusal => "local-refusal",
    }
}

fn context_tag(failure: ContextFailure) -> &'static str {
    match failure {
        ContextFailure::ContinuityUnavailable => "continuity-unavailable",
        ContextFailure::Overflow => "overflow",
        ContextFailure::CompactionFailed => "compaction-failed",
        // A repeat or a summary that did not fit is recorded as the
        // no-progress it is: an older build on the same store must still read
        // the Session (J07 revert). The step's detail names the difference;
        // the typed difference lives in memory.
        ContextFailure::CompactionNoProgress
        | ContextFailure::CompactionRepeated
        | ContextFailure::CompactionDidNotFit => "compaction-no-progress",
        ContextFailure::CompactionIncompatibleSummary => "compaction-incompatible-summary",
    }
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// Decode a `session.step.failed.2` payload.
///
/// The root's unknown fields are ignored: the reason level decides everything
/// this fact carries, and a coordinate a future writer adds at the root is
/// vocabulary, not corruption. The fact is built by the decode-side
/// constructor, which is what admits a future writer's `Unknown` reason —
/// the writer's [`StepFailedFact::new`] would refuse it.
pub fn decode(value: &Value) -> Result<StepFailedFact, PayloadError> {
    let root = object(value, AT)?;
    let step = required_step(root, AT, "step")?;
    let reason = decode_reason(required(root, AT, "reason")?)?;
    Ok(StepFailedFact::from_decoded(step, reason))
}

/// Decode the `reason` object. This is the place the tolerant contract is
/// pinned, and its law has three classes of input with three different
/// answers:
///
/// 1. **Corruption is refused.** A missing, `null`, wrong-typed or blank
///    REQUIRED coordinate — of a KNOWN reason, of an unknown one, or of a
///    nested kind — is a broken record this build shares, and it is refused
///    as such. A blank is not vocabulary for the fallback to name.
/// 2. **A known non-failure is refused explicitly.** `kind: "ended"` is
///    refused by the constructor, not by vocabulary — and the refusal holds
///    even when the completion word is itself unknown to this build, because
///    the word `ended` IS known, and it is not a failure.
/// 3. **Unknown words decode to `Unknown`.** A NON-EMPTY `kind` this build
///    does not know, or an unknown non-empty word inside a known kind,
///    becomes [`StepTerminalReason::Unknown`] with a byte-capped tag
///    qualified by the path it was read from. A decoded `Unknown` is a real failure fact — the
///    Step failed for a reason this build cannot name — and it carries no
///    effect authority: nothing downstream may grant an effect permission
///    because of it.
///
/// All KNOWN required fields are validated BEFORE any fallback, so a payload
/// that is both corrupt and unknown is corrupt, and says so. Unknown EXTRA
/// fields are ignored at every level, including inside a known reason.
///
/// The future-writer contract this encodes: a later build may ADD a reason
/// kind, and may add OPTIONAL fields anywhere; it may NOT add a REQUIRED
/// field to an existing kind, because this reader would then refuse the
/// record, and a refused terminal is an unproven one.
fn decode_reason(value: &Value) -> Result<StepTerminalReason, PayloadError> {
    let object = object(value, AT_REASON)?;
    let kind = required_word(object, AT_REASON, "kind", "a non-empty reason kind")?;
    let reason = match kind {
        // `ended` is read structurally so a broken ended payload is still
        // named as corruption, then refused as a constructor fact.
        "ended" => {
            required_str(object, AT_REASON, "completion")?;
            return Err(PayloadError::RefusedByConstructor {
                failure: FactError::NotAFailure,
            });
        }
        "model-asserted-complete" => StepTerminalReason::ModelAssertedComplete,
        "policy-limit" => StepTerminalReason::PolicyLimit {
            // The policy VERSION is open vocabulary this build validates as a
            // non-empty string: empty is corruption, a word it does not know
            // is still a valid version.
            policy: policy_of(required_str(object, AT_REASON, "policy")?, "policy")?,
        },
        "budget-exhausted" => StepTerminalReason::BudgetExhausted {
            policy: policy_of(required_str(object, AT_REASON, "policy")?, "policy")?,
        },
        "interrupted" => {
            let raw = required_word(object, AT_REASON, "by", "a non-empty interruption source")?;
            match interruption_of(raw) {
                Some(by) => StepTerminalReason::Interrupted { by },
                None => StepTerminalReason::Unknown {
                    tag: unknown_reason_tag("interrupted", "by", raw),
                },
            }
        }
        "provider-failure" => {
            let raw = required_word(
                object,
                AT_REASON,
                "class",
                "a non-empty provider failure class",
            )?;
            match class_of(raw) {
                Some(class) => StepTerminalReason::ProviderFailure { class },
                None => StepTerminalReason::Unknown {
                    tag: unknown_reason_tag("provider-failure", "class", raw),
                },
            }
        }
        "context-failure" => {
            let raw = required_word(object, AT_REASON, "failure", "a non-empty context failure")?;
            match context_of(raw) {
                Some(failure) => StepTerminalReason::ContextFailure { failure },
                None => StepTerminalReason::Unknown {
                    tag: unknown_reason_tag("context-failure", "failure", raw),
                },
            }
        }
        "authority-refused" => match decode_principal(required(object, AT_REASON, "principal")?)? {
            PrincipalRead::Typed(principal) => StepTerminalReason::AuthorityRefused { principal },
            PrincipalRead::Unknown { tag } => StepTerminalReason::Unknown {
                tag: cap_tag(&format!("authority-refused/{tag}")),
            },
        },
        "durability-failure" => StepTerminalReason::DurabilityFailure,
        "process-loss" => match decode_effect(required(object, AT_REASON, "effect")?)? {
            EffectRead::Typed(effect) => StepTerminalReason::ProcessLoss { effect },
            EffectRead::Unknown { tag } => StepTerminalReason::Unknown {
                tag: cap_tag(&format!("process-loss/{tag}")),
            },
        },
        other => StepTerminalReason::Unknown {
            tag: cap_tag(other),
        },
    };
    Ok(reason)
}

/// What the `principal` object decoded to: the typed principal this build
/// names, or the unknown kind it named instead.
enum PrincipalRead {
    Typed(AuthorityPrincipal),
    Unknown { tag: String },
}

/// Decode the principal level. Known kinds keep their STRICT field
/// validation — a blank coordinate is corruption, refused. An unknown
/// NON-EMPTY kind is vocabulary: it decodes to the path-qualified tag, never
/// to a neighbouring arm. A blank kind is corruption, refused.
fn decode_principal(value: &Value) -> Result<PrincipalRead, PayloadError> {
    let object = object(value, AT_PRINCIPAL)?;
    let kind = required_word(object, AT_PRINCIPAL, "kind", "a non-empty principal kind")?;
    let principal = match kind {
        "human" => AuthorityPrincipal::Human {
            account: AccountId::new(required_str(object, AT_PRINCIPAL, "account")?).ok_or(
                PayloadError::InvalidField {
                    at: AT_PRINCIPAL,
                    field: "account".to_owned(),
                    expected: "a non-empty account id",
                },
            )?,
        },
        "system" => AuthorityPrincipal::System,
        "policy-layer" => AuthorityPrincipal::PolicyLayer {
            layer: LayerName::new(required_str(object, AT_PRINCIPAL, "layer")?).ok_or(
                PayloadError::InvalidField {
                    at: AT_PRINCIPAL,
                    field: "layer".to_owned(),
                    expected: "a non-empty layer name",
                },
            )?,
        },
        "hook" => AuthorityPrincipal::Hook {
            hook: HookId::new(required_str(object, AT_PRINCIPAL, "hook")?).ok_or(
                PayloadError::InvalidField {
                    at: AT_PRINCIPAL,
                    field: "hook".to_owned(),
                    expected: "a non-empty hook id",
                },
            )?,
        },
        "automated-reviewer" => AuthorityPrincipal::AutomatedReviewer {
            agent: AgentId::new(required_str(object, AT_PRINCIPAL, "agent")?).ok_or(
                PayloadError::InvalidField {
                    at: AT_PRINCIPAL,
                    field: "agent".to_owned(),
                    expected: "a non-empty agent id",
                },
            )?,
        },
        other => {
            return Ok(PrincipalRead::Unknown {
                tag: format!("principal/kind/{other}"),
            });
        }
    };
    Ok(PrincipalRead::Typed(principal))
}

/// What the `effect` object decoded to: the typed effect this build names, or
/// the unknown kind it named instead.
enum EffectRead {
    Typed(ExternalEffectState),
    Unknown { tag: String },
}

/// Decode the effect level. Known kinds keep their STRICT field validation —
/// empty evidence is not evidence, and is refused. An unknown NON-EMPTY kind
/// is vocabulary: it decodes to the path-qualified tag. A blank kind is
/// corruption, refused.
fn decode_effect(value: &Value) -> Result<EffectRead, PayloadError> {
    let object = object(value, AT_EFFECT)?;
    let kind = required_word(object, AT_EFFECT, "kind", "a non-empty effect kind")?;
    let effect = match kind {
        "none-initiated" => ExternalEffectState::NoneInitiated,
        "settled" => ExternalEffectState::Settled,
        "ambiguous" => ExternalEffectState::Ambiguous {
            // Empty evidence is not evidence, and the type refuses it. A
            // present-but-blank string is a wrong VALUE, not a missing
            // field, so it is reported as an invalid field rather than
            // silently becoming absence.
            evidence: AmbiguityEvidence::new(required_str(object, AT_EFFECT, "evidence")?).ok_or(
                PayloadError::InvalidField {
                    at: AT_EFFECT,
                    field: "evidence".to_owned(),
                    expected: "non-empty evidence",
                },
            )?,
        },
        other => {
            return Ok(EffectRead::Unknown {
                tag: format!("effect/kind/{other}"),
            });
        }
    };
    Ok(EffectRead::Typed(effect))
}

// ---------------------------------------------------------------------------
// Strict readers
// ---------------------------------------------------------------------------

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
        .ok_or(PayloadError::InvalidField {
            at,
            field: field.to_owned(),
            expected: "a string",
        })
}

/// A REQUIRED vocabulary word: a string the contract must classify — a
/// known word, or a NON-EMPTY word this build does not know. Blank is not
/// a word; it is a broken coordinate of a record this build shares, and it
/// is refused as corruption, never folded into `Unknown` (an empty tag
/// would be terminal evidence with no content). Whitespace is not
/// normalized: `""` is blank, and a space is a non-empty word like any
/// other this build does not know.
fn required_word<'a>(
    object: &'a Map<String, Value>,
    at: &'static str,
    field: &str,
    expected: &'static str,
) -> Result<&'a str, PayloadError> {
    let raw = required_str(object, at, field)?;
    if raw.is_empty() {
        return Err(PayloadError::InvalidField {
            at,
            field: field.to_owned(),
            expected,
        });
    }
    Ok(raw)
}

/// A Step number is positive by type. `0`, a negative, a float and a
/// beyond-`u32` integer are all wrong VALUES for the same field.
fn required_step(
    object: &Map<String, Value>,
    at: &'static str,
    field: &str,
) -> Result<StepNumber, PayloadError> {
    let invalid = || PayloadError::InvalidField {
        at,
        field: field.to_owned(),
        expected: "a positive integer step number",
    };
    let raw = required(object, at, field)?.as_u64().ok_or_else(invalid)?;
    let narrowed = u32::try_from(raw).map_err(|_| invalid())?;
    StepNumber::new(narrowed).ok_or_else(invalid)
}

/// The closed vocabularies, as OPTIONS: `None` is the word this build does
/// not know, which the reason level turns into `Unknown` rather than a
/// refusal.
fn interruption_of(raw: &str) -> Option<InterruptionSource> {
    match raw {
        "user" => Some(InterruptionSource::User),
        "managed-shutdown" => Some(InterruptionSource::ManagedShutdown),
        _ => None,
    }
}

fn class_of(raw: &str) -> Option<ProviderFailureClass> {
    match raw {
        "rate-limit" => Some(ProviderFailureClass::RateLimit),
        "provider-internal" => Some(ProviderFailureClass::ProviderInternal),
        "transport" => Some(ProviderFailureClass::Transport),
        "no-progress-timeout" => Some(ProviderFailureClass::NoProgressTimeout),
        "authentication" => Some(ProviderFailureClass::Authentication),
        "quota" => Some(ProviderFailureClass::Quota),
        "content-policy" => Some(ProviderFailureClass::ContentPolicy),
        "invalid-request" => Some(ProviderFailureClass::InvalidRequest),
        "invalid-provider-output" => Some(ProviderFailureClass::InvalidProviderOutput),
        "unknown-provider" => Some(ProviderFailureClass::UnknownProvider),
        "no-route" => Some(ProviderFailureClass::NoRoute),
        "local-refusal" => Some(ProviderFailureClass::LocalRefusal),
        _ => None,
    }
}

fn context_of(raw: &str) -> Option<ContextFailure> {
    match raw {
        "continuity-unavailable" => Some(ContextFailure::ContinuityUnavailable),
        "overflow" => Some(ContextFailure::Overflow),
        "compaction-failed" => Some(ContextFailure::CompactionFailed),
        "compaction-no-progress" => Some(ContextFailure::CompactionNoProgress),
        "compaction-incompatible-summary" => Some(ContextFailure::CompactionIncompatibleSummary),
        _ => None,
    }
}

fn policy_of(raw: &str, field: &'static str) -> Result<PolicyVersion, PayloadError> {
    PolicyVersion::new(raw).ok_or(PayloadError::InvalidField {
        at: AT_REASON,
        field: field.to_owned(),
        expected: "a non-empty policy version",
    })
}

/// The tag an unknown word inside a known reason carries: the reason kind,
/// the field it was read from, then the word itself.
fn unknown_reason_tag(kind: &str, field: &str, word: &str) -> String {
    cap_tag(&format!("{kind}/{field}/{word}"))
}

/// A durable tag is untrusted text. The cap keeps it bounded; it does not
/// sanitize it — sanitization is the display layer's job, and the codec must
/// not rewrite what it only witnessed. The cut lands on a char boundary.
fn cap_tag(tag: &str) -> String {
    const MAX_TAG_BYTES: usize = 64;
    if tag.len() <= MAX_TAG_BYTES {
        return tag.to_owned();
    }
    let mut end = MAX_TAG_BYTES;
    while !tag.is_char_boundary(end) {
        end -= 1;
    }
    tag[..end].to_owned()
}

#[cfg(test)]
mod tests;
