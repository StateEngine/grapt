//! Strict Rust-native payloads for the assistant message a Step produces:
//! its identity, its ordered content, and its usage.
//!
//! # Why these are new NAMES rather than new versions of oracle kinds
//!
//! The oracle carries `message.updated.1`, `message.part.updated.1`,
//! `session.text.started.1`, `session.reasoning.started.1` and
//! `session.usage.recorded.1`. Every one of them is a v1 manifest key, and
//! [`crate::kind::KindProvenance`] states the law plainly: "no new v1 write of
//! any provenance is legal". So the vertical that has to WRITE assistant
//! content cannot use them. It could mint v2 of each name — which is what
//! `session.step.ended.2` did — but that would carry the oracle's SHAPE
//! forward, and the shape is the thing the supersession matrix corrects:
//!
//! > Separate text/reasoning/tool arrays may lose interleaving or pairing.
//! > One `NormalizedContentSequence` with exact `ContentOrdinal` preserves
//! > text, reasoning/signature, tool and opaque-item position through
//! > replay/truncation.
//!
//! Per-kind start/end names ARE the parallel-array shape, spelled as events.
//! One ordered `session.content.appended.1` carrying an ordinal and a closed
//! kind is the corrected fact, so it gets a corrected name.
//!
//! # Separate settlement facts
//!
//! Identity, content and usage settle at different moments and reconcile under
//! different laws — a later usage observation reconciles an earlier one by
//! field and phase, while a content item at an ordinal is immutable once
//! written. Folding them into one fact would make the message identity
//! rewritable by a usage correction.
//!
//! # Payloads are references, never inline bytes
//!
//! [`NormalizedContentItem`] carries an [`ArtifactRef`], and that is deliberate
//! upstream law: "The payload is a bounded `ArtifactRef`, never inline bytes,
//! so the sequence stays byte-bounded while remaining replayable." Capture is a
//! separate recovery unit and must already have committed before a caller
//! constructs one of these facts — exactly as
//! [`crate::session_tool_terminal`] requires.

use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, AttemptUsage, CaptureDisposition, ContentKind,
    ContentOrdinal, LegacyAttemptUsage, MessageId, NormalizedContentItem, PhysicalAttemptId,
    StepNumber, ToolCallId,
};
use serde_json::{Map, Value};

mod usage;
use usage::{decode_usage_v1, decode_usage_v2, encode_usage};

const OPENED: &str = "session.assistant-message.opened.1";
const BLOCK_OPENED: &str = "session.content-block.opened.1";
const APPENDED: &str = "session.content.appended.1";
const APPENDED_ITEM: &str = "session.content.appended.1.item";
const APPENDED_PAYLOAD: &str = "session.content.appended.1.item.payload";
const OBSERVED_V1: &str = "session.usage.observed.1";
const OBSERVED_V1_USAGE: &str = "session.usage.observed.1.usage";
const OBSERVED_V2: &str = "session.usage.observed.2";
const OBSERVED_V2_USAGE: &str = "session.usage.observed.2.usage";
const OBSERVED_V2_FIELD: &str = "session.usage.observed.2.usage.field";

/// The assistant message one logical Step owns, bound to the Physical Attempt
/// that opened it.
///
/// The Step number is carried because a Step may span several attempts: the
/// message belongs to the STEP, while the attempt names which physical call
/// first admitted provider I/O for it. A fact carrying only the attempt would mint a
/// second message identity on a retry, and "a retry is not a new Step".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantMessageOpenedFact {
    attempt: PhysicalAttemptId,
    step: StepNumber,
    message: MessageId,
    selection: Option<AssistantSelection>,
}

/// The selection actually captured for this Step, rather than a mutable
/// Session default read later while hydrating history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantSelection {
    agent: grapt_types::AgentId,
    provider: grapt_types::ProviderId,
    model: grapt_types::ModelId,
    variant: Option<grapt_types::ModelVariantId>,
}

impl AssistantSelection {
    pub fn new(
        agent: grapt_types::AgentId,
        provider: grapt_types::ProviderId,
        model: grapt_types::ModelId,
        variant: Option<grapt_types::ModelVariantId>,
    ) -> Self {
        Self {
            agent,
            provider,
            model,
            variant,
        }
    }
    pub fn agent(&self) -> &grapt_types::AgentId {
        &self.agent
    }
    pub fn provider(&self) -> &grapt_types::ProviderId {
        &self.provider
    }
    pub fn model(&self) -> &grapt_types::ModelId {
        &self.model
    }
    pub fn variant(&self) -> Option<&grapt_types::ModelVariantId> {
        self.variant.as_ref()
    }
}

/// A streaming block's kind, independent of provider or public event spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentBlockKind {
    Text,
    Reasoning,
}

/// Committed identity of one block before its transient bytes are published.
/// The block ordinal is scoped to its attempt; canonical captured content keeps
/// its separate message-wide ordinal when the block closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentBlockOpenedFact {
    attempt: PhysicalAttemptId,
    message: MessageId,
    block: u32,
    kind: ContentBlockKind,
}

impl ContentBlockOpenedFact {
    pub fn new(
        attempt: PhysicalAttemptId,
        message: MessageId,
        block: u32,
        kind: ContentBlockKind,
    ) -> Self {
        Self {
            attempt,
            message,
            block,
            kind,
        }
    }
    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }
    pub fn message(&self) -> &MessageId {
        &self.message
    }
    pub fn block(&self) -> u32 {
        self.block
    }
    pub fn kind(&self) -> ContentBlockKind {
        self.kind
    }
}

impl AssistantMessageOpenedFact {
    pub fn new(attempt: PhysicalAttemptId, step: StepNumber, message: MessageId) -> Self {
        Self {
            attempt,
            step,
            message,
            selection: None,
        }
    }

    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    pub fn step(&self) -> StepNumber {
        self.step
    }

    pub fn message(&self) -> &MessageId {
        &self.message
    }

    pub fn with_selection(mut self, selection: AssistantSelection) -> Self {
        self.selection = Some(selection);
        self
    }
    pub fn selection(&self) -> Option<&AssistantSelection> {
        self.selection.as_ref()
    }
}

/// One content item appended to an assistant message at an exact ordinal.
///
/// # Why the capture disposition rides here and not on the reference
///
/// The Agency/Continuity Amendment 3.3 requires that the canonical fact
/// reference "a bounded typed artifact identity carrying at least exact content
/// length and integrity identity, plus a capture disposition that says whether
/// bytes were discarded at the boundary." [`ArtifactRef`] carries the first two
/// and not the third, and it is a shared foundation type this lane does not
/// own, so the third coordinate lives on the fact that references it.
///
/// Omitting it was not a cosmetic gap. A bounded producer that stops AT its
/// capture bound and an unbounded one that overruns it hand the store the same
/// length and the same digest -- they differ ONLY in disposition. Without this
/// field a reader cannot tell a complete capture from a truncated one, and a
/// mutation that removes the producer's bound survives green. Row `IGSD07` is
/// exactly that mutant and it DID survive the first battery; this field is what
/// makes it fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentAppendedFact {
    message: MessageId,
    item: NormalizedContentItem,
    disposition: CaptureDisposition,
}

impl ContentAppendedFact {
    /// The only route to the fact, and it CHECKS the disposition against the
    /// payload it describes.
    ///
    /// The two coordinates are not independent. `Complete` means the referenced
    /// artifact IS the whole value; `TruncatedAtBoundary { captured }` means
    /// exactly `captured` bytes were kept, which is the referenced artifact's
    /// own length and nothing else. An infallible constructor let a producer
    /// build `truncated { captured: 0 }` over a four-kilobyte payload and hand
    /// it straight to the encoder, so the decoder-side rule was only half a
    /// rule: the contradiction was unreachable through stored bytes and wide
    /// open through the public type. Refusing here closes the producer door
    /// with the SAME predicate the decoder applies.
    pub fn new(
        message: MessageId,
        item: NormalizedContentItem,
        disposition: CaptureDisposition,
    ) -> Result<Self, PayloadError> {
        agrees_with_payload(&item, disposition)?;
        Ok(Self {
            message,
            item,
            disposition,
        })
    }

    pub fn message(&self) -> &MessageId {
        &self.message
    }

    pub fn item(&self) -> &NormalizedContentItem {
        &self.item
    }

    /// Whether the referenced bytes are the whole value or a bounded prefix.
    pub fn disposition(&self) -> CaptureDisposition {
        self.disposition
    }
}

/// One usage observation for one Physical Attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageObservedFact {
    attempt: PhysicalAttemptId,
    usage: AttemptUsage,
}

impl UsageObservedFact {
    pub fn new(attempt: PhysicalAttemptId, usage: AttemptUsage) -> Self {
        Self { attempt, usage }
    }

    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    pub fn usage(&self) -> &AttemptUsage {
        &self.usage
    }
}

/// One already-shipped v1 usage observation decoded for replay/migration.
/// It is deliberately a different type from [`UsageObservedFact`], so no v2
/// constructor, store append, or encoder accepts the ambiguous input slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyUsageObservedFact {
    attempt: PhysicalAttemptId,
    usage: LegacyAttemptUsage,
}

impl LegacyUsageObservedFact {
    fn from_decoded_v1(attempt: PhysicalAttemptId, usage: LegacyAttemptUsage) -> Self {
        Self { attempt, usage }
    }

    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    pub fn usage(&self) -> &LegacyAttemptUsage {
        &self.usage
    }
}

/// Why a stored payload is not a member of this contract.
///
/// A sibling of [`crate::session_tool_terminal::PayloadError`] rather than a
/// reuse of it: these are separate versioned payload contracts, and a shared
/// error type would let a change to one contract's failure vocabulary silently
/// change the other's public surface.
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

pub fn encode_opened(fact: &AssistantMessageOpenedFact) -> Value {
    let mut root = Map::new();
    root.insert("attempt".to_owned(), Value::from(fact.attempt.as_str()));
    root.insert("step".to_owned(), Value::from(fact.step.value()));
    root.insert("message".to_owned(), Value::from(fact.message.as_str()));
    if let Some(selection) = &fact.selection {
        let mut value = serde_json::json!({"agent":selection.agent.as_str(),"provider":selection.provider.as_str(),"model":selection.model.as_str()});
        if let Some(variant) = &selection.variant {
            value["variant"] = Value::from(variant.as_str());
        }
        root.insert("selection".into(), value);
    }
    Value::Object(root)
}

pub fn decode_opened_v2(value: &Value) -> Result<AssistantMessageOpenedFact, PayloadError> {
    const AT: &str = "session.assistant-message.opened.2";
    let root = object(value, AT)?;
    reject_unknown(root, AT, &["attempt", "step", "message", "selection"])?;
    let selection = object(
        root.get("selection")
            .ok_or_else(|| invalid(AT, "selection", "a required selection object"))?,
        AT,
    )?;
    reject_unknown(selection, AT, &["agent", "provider", "model", "variant"])?;
    let agent = grapt_types::AgentId::new(required_str(selection, AT, "agent")?)
        .ok_or_else(|| invalid(AT, "agent", "a validated agent identity"))?;
    let provider = grapt_types::ProviderId::new(required_str(selection, AT, "provider")?)
        .ok_or_else(|| invalid(AT, "provider", "a provider identity"))?;
    let model = grapt_types::ModelId::new(required_str(selection, AT, "model")?)
        .ok_or_else(|| invalid(AT, "model", "a model identity"))?;
    let variant = selection
        .get("variant")
        .map(|value| {
            value
                .as_str()
                .and_then(grapt_types::ModelVariantId::new)
                .ok_or_else(|| invalid(AT, "variant", "a variant identity"))
        })
        .transpose()?;
    let mut legacy = root.clone();
    legacy.remove("selection");
    Ok(decode_opened(&Value::Object(legacy))?
        .with_selection(AssistantSelection::new(agent, provider, model, variant)))
}

pub fn encode_block_opened(fact: &ContentBlockOpenedFact) -> Value {
    let mut root = Map::new();
    root.insert("attempt".into(), Value::from(fact.attempt.as_str()));
    root.insert("message".into(), Value::from(fact.message.as_str()));
    root.insert("block".into(), Value::from(fact.block));
    root.insert(
        "kind".into(),
        Value::from(match fact.kind {
            ContentBlockKind::Text => "text",
            ContentBlockKind::Reasoning => "reasoning",
        }),
    );
    Value::Object(root)
}

pub fn decode_block_opened(value: &Value) -> Result<ContentBlockOpenedFact, PayloadError> {
    let root = object(value, BLOCK_OPENED)?;
    let attempt = PhysicalAttemptId::new(required_str(root, BLOCK_OPENED, "attempt")?)
        .ok_or_else(|| invalid(BLOCK_OPENED, "attempt", "a non-empty PhysicalAttemptId"))?;
    let message = required_str(root, BLOCK_OPENED, "message")?
        .parse()
        .map_err(|_| invalid(BLOCK_OPENED, "message", "a validated msg_* identity"))?;
    let block = u32::try_from(required_u64(root, BLOCK_OPENED, "block")?)
        .map_err(|_| invalid(BLOCK_OPENED, "block", "a bounded block ordinal"))?;
    let kind = match required_str(root, BLOCK_OPENED, "kind")? {
        "text" => ContentBlockKind::Text,
        "reasoning" => ContentBlockKind::Reasoning,
        _ => return Err(invalid(BLOCK_OPENED, "kind", "text or reasoning")),
    };
    reject_unknown(root, BLOCK_OPENED, &["attempt", "message", "block", "kind"])?;
    Ok(ContentBlockOpenedFact::new(attempt, message, block, kind))
}

pub fn decode_opened(value: &Value) -> Result<AssistantMessageOpenedFact, PayloadError> {
    let root = object(value, OPENED)?;
    let attempt = PhysicalAttemptId::new(required_str(root, OPENED, "attempt")?)
        .ok_or_else(|| invalid(OPENED, "attempt", "a non-empty PhysicalAttemptId"))?;
    let step = u32::try_from(required_u64(root, OPENED, "step")?)
        .ok()
        .and_then(StepNumber::new)
        .ok_or_else(|| invalid(OPENED, "step", "a positive step number"))?;
    let message: MessageId = required_str(root, OPENED, "message")?
        .parse()
        .map_err(|_| invalid(OPENED, "message", "a validated msg_* identity"))?;
    reject_unknown(root, OPENED, &["attempt", "step", "message"])?;
    Ok(AssistantMessageOpenedFact::new(attempt, step, message))
}

pub fn encode_appended(fact: &ContentAppendedFact) -> Value {
    let mut root = Map::new();
    root.insert("message".to_owned(), Value::from(fact.message.as_str()));
    root.insert("item".to_owned(), encode_item(&fact.item, fact.disposition));
    Value::Object(root)
}

pub fn decode_appended(value: &Value) -> Result<ContentAppendedFact, PayloadError> {
    let root = object(value, APPENDED)?;
    let message: MessageId = required_str(root, APPENDED, "message")?
        .parse()
        .map_err(|_| invalid(APPENDED, "message", "a validated msg_* identity"))?;
    let (item, disposition) = decode_item(required(root, APPENDED, "item")?)?;
    reject_unknown(root, APPENDED, &["message", "item"])?;
    ContentAppendedFact::new(message, item, disposition)
}

pub fn encode_observed(fact: &UsageObservedFact) -> Value {
    let mut root = Map::new();
    root.insert("attempt".to_owned(), Value::from(fact.attempt.as_str()));
    root.insert("usage".to_owned(), encode_usage(&fact.usage));
    Value::Object(root)
}

pub fn decode_observed(value: &Value) -> Result<UsageObservedFact, PayloadError> {
    let root = object(value, OBSERVED_V2)?;
    let attempt = PhysicalAttemptId::new(required_str(root, OBSERVED_V2, "attempt")?)
        .ok_or_else(|| invalid(OBSERVED_V2, "attempt", "a non-empty PhysicalAttemptId"))?;
    let usage = decode_usage_v2(required(root, OBSERVED_V2, "usage")?)?;
    reject_unknown(root, OBSERVED_V2, &["attempt", "usage"])?;
    Ok(UsageObservedFact::new(attempt, usage))
}

/// Decode the already-shipped v1 payload. Its single input counter is routed
/// to the migration-only ambiguous input coordinate; assigning it to fresh or inclusive input would
/// invent route semantics the durable bytes never recorded.
pub fn decode_observed_v1(value: &Value) -> Result<LegacyUsageObservedFact, PayloadError> {
    let root = object(value, OBSERVED_V1)?;
    let attempt = PhysicalAttemptId::new(required_str(root, OBSERVED_V1, "attempt")?)
        .ok_or_else(|| invalid(OBSERVED_V1, "attempt", "a non-empty PhysicalAttemptId"))?;
    let usage = decode_usage_v1(required(root, OBSERVED_V1, "usage")?)?;
    reject_unknown(root, OBSERVED_V1, &["attempt", "usage"])?;
    Ok(LegacyUsageObservedFact::from_decoded_v1(attempt, usage))
}

pub fn encode_canonical_opened(fact: &AssistantMessageOpenedFact) -> Vec<u8> {
    crate::canonical::canonical_string(&encode_opened(fact)).into_bytes()
}

pub fn encode_canonical_appended(fact: &ContentAppendedFact) -> Vec<u8> {
    crate::canonical::canonical_string(&encode_appended(fact)).into_bytes()
}

pub fn encode_canonical_observed(fact: &UsageObservedFact) -> Vec<u8> {
    crate::canonical::canonical_string(&encode_observed(fact)).into_bytes()
}

fn encode_item(item: &NormalizedContentItem, disposition: CaptureDisposition) -> Value {
    let mut out = Map::new();
    out.insert("ordinal".to_owned(), Value::from(item.ordinal().value()));
    match item.kind() {
        ContentKind::Text => {
            out.insert("kind".to_owned(), Value::from("text"));
        }
        ContentKind::Reasoning => {
            out.insert("kind".to_owned(), Value::from("reasoning"));
        }
        ContentKind::Tool { call } => {
            out.insert("kind".to_owned(), Value::from("tool"));
            out.insert("call".to_owned(), Value::from(call.as_str()));
        }
        ContentKind::Opaque => {
            out.insert("kind".to_owned(), Value::from("opaque"));
        }
    }
    out.insert("payload".to_owned(), encode_reference(item.payload()));
    match disposition {
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

fn decode_item(value: &Value) -> Result<(NormalizedContentItem, CaptureDisposition), PayloadError> {
    let out = object(value, APPENDED_ITEM)?;
    let ordinal = ContentOrdinal::new(required_u64(out, APPENDED_ITEM, "ordinal")?);
    let kind = match required_str(out, APPENDED_ITEM, "kind")? {
        "text" => ContentKind::Text,
        "reasoning" => ContentKind::Reasoning,
        "tool" => {
            let call = ToolCallId::new(required_str(out, APPENDED_ITEM, "call")?)
                .ok_or_else(|| invalid(APPENDED_ITEM, "call", "a non-empty ToolCallId"))?;
            ContentKind::Tool { call }
        }
        "opaque" => ContentKind::Opaque,
        other => {
            return Err(PayloadError::UnknownVariant {
                at: APPENDED_ITEM,
                field: "kind".to_owned(),
                value: other.to_owned(),
            })
        }
    };
    let payload = decode_reference(required(out, APPENDED_ITEM, "payload")?)?;
    let disposition = match required_str(out, APPENDED_ITEM, "disposition")? {
        "complete" => CaptureDisposition::Complete,
        "truncated" => CaptureDisposition::TruncatedAtBoundary {
            captured: required_u64(out, APPENDED_ITEM, "captured")?,
        },
        other => {
            return Err(PayloadError::UnknownVariant {
                at: APPENDED_ITEM,
                field: "disposition".to_owned(),
                value: other.to_owned(),
            })
        }
    };
    // The legal field set is decided by the disposition as well as the kind, so
    // it cannot be applied until BOTH are known. Rejecting unknowns inside the
    // kind arms — as an earlier revision did — necessarily used a set that
    // named `captured` unconditionally, which is exactly the hole below.
    reject_unknown(out, APPENDED_ITEM, &item_fields(&kind, disposition))?;
    let item = NormalizedContentItem::new(ordinal, kind, payload);
    agrees_with_payload(&item, disposition)?;
    Ok((item, disposition))
}

/// The fields a content item carries, given its kind AND its disposition.
///
/// Deliberately NOT one shared list. `captured` is a truncated-only coordinate:
/// the earlier shared constant named it for every disposition, so
/// `{disposition: "complete", captured: N}` decoded — the complete arm simply
/// ignored the field — and then re-encoded WITHOUT it. A payload that decodes
/// to something which does not re-encode to itself is not canonical, and the
/// contradiction it admitted is precisely "a whole value that also reports how
/// much of itself was kept".
fn item_fields(kind: &ContentKind, disposition: CaptureDisposition) -> Vec<&'static str> {
    let mut fields = vec!["ordinal", "kind", "payload", "disposition"];
    if matches!(kind, ContentKind::Tool { .. }) {
        // A tool item additionally carries its exact call identity.
        fields.push("call");
    }
    if !disposition.is_complete() {
        fields.push("captured");
    }
    fields
}

/// The one predicate that binds a disposition to the payload it describes.
///
/// Shared by the public constructor and the decoder ON PURPOSE: two copies of
/// this rule is two places for them to disagree, and a producer-side rule that
/// disagreed with the decoder would let a fact be built that its own canonical
/// bytes could not be read back as.
fn agrees_with_payload(
    item: &NormalizedContentItem,
    disposition: CaptureDisposition,
) -> Result<(), PayloadError> {
    let CaptureDisposition::TruncatedAtBoundary { captured } = disposition else {
        return Ok(());
    };
    if captured == item.payload().length() {
        return Ok(());
    }
    Err(invalid(
        APPENDED_ITEM,
        "captured",
        "exactly the referenced artifact's length",
    ))
}

fn encode_reference(reference: &ArtifactRef) -> Value {
    let mut out = Map::new();
    out.insert("id".to_owned(), Value::from(reference.id().as_str()));
    out.insert("length".to_owned(), Value::from(reference.length()));
    out.insert(
        "digest".to_owned(),
        Value::from(reference.digest().as_str()),
    );
    Value::Object(out)
}

fn decode_reference(value: &Value) -> Result<ArtifactRef, PayloadError> {
    let out = object(value, APPENDED_PAYLOAD)?;
    let id = ArtifactId::new(required_str(out, APPENDED_PAYLOAD, "id")?)
        .ok_or_else(|| invalid(APPENDED_PAYLOAD, "id", "a non-empty ArtifactId"))?;
    let length = required_u64(out, APPENDED_PAYLOAD, "length")?;
    let digest = ArtifactDigest::new(required_str(out, APPENDED_PAYLOAD, "digest")?)
        .ok_or_else(|| invalid(APPENDED_PAYLOAD, "digest", "a non-empty ArtifactDigest"))?;
    reject_unknown(out, APPENDED_PAYLOAD, &["id", "length", "digest"])?;
    Ok(ArtifactRef::new(id, length, digest))
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

pub mod checkpoint;

#[cfg(test)]
mod tests;
