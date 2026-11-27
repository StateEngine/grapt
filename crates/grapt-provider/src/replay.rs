//! Bounded adapter replay, bound to the exact durable generating selection.
use crate::authority::RouteBoundEndpoint;
use crate::fragment::AdapterState;
use grapt_types::{EventSeq, ModelId, ModelRef, ModelVariantId, ProviderId};
use sha2::Digest;

mod restore_error;
pub use restore_error::ReplayError;
mod anthropic;
pub(crate) mod bedrock;
pub(crate) use anthropic::{anthropic_search_state, anthropic_text_state};
mod sendable;
pub use sendable::drop_foreign_replays;
pub(crate) use sendable::drop_unreplayable_reasoning;

/// An extra replay restriction, never a capability or an authorization receipt.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ReplayBinding([u8; 32]);

impl std::fmt::Debug for ReplayBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReplayBinding([REDACTED])")
    }
}

impl ReplayBinding {
    pub(crate) fn for_bedrock(
        input: &crate::transport::adapter::AttemptInput,
    ) -> Result<Self, &'static str> {
        let coordinates = Self::from_input(input)?;
        let mut hash = sha2::Sha256::new();
        hash.update(b"secode/bedrock-reasoning-binding/v1\0");
        hash.update(coordinates.0);
        Ok(Self(hash.finalize().into()))
    }

    pub(crate) fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub(crate) fn from_input(
        input: &crate::transport::adapter::AttemptInput,
    ) -> Result<Self, &'static str> {
        if input.endpoint.base().len() > 16 * 1024 {
            return Err("replay endpoint exceeds its byte bound");
        }
        let url = input.endpoint.url();
        let parsed = reqwest::Url::parse(&url).map_err(|_| "replay endpoint is malformed")?;
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("replay endpoint has unsupported credential-bearing components");
        }
        let coordinates = Self::from_fields([
            parsed.as_str(),
            input.endpoint.route().as_str(),
            input.lease.account().as_str(),
            input.lease.credential_version().as_str(),
        ])?;
        if input.request.family() == crate::ProtocolFamily::VertexAnthropic {
            let mut hash = sha2::Sha256::new();
            hash.update(b"secode/vertex-anthropic-replay-binding/v1\0");
            hash.update(coordinates.0);
            return Ok(Self(hash.finalize().into()));
        }
        Ok(coordinates)
    }

    /// The binding of a Responses reasoning replay: [`Self::from_input`]'s
    /// coordinates and the protocol family. The two Responses families share
    /// a wire shape, not their encrypted content.
    pub(crate) fn for_reasoning(input: &crate::transport::adapter::AttemptInput) -> Option<Self> {
        let family: &[u8] = match input.request.family() {
            crate::ProtocolFamily::OpenAiResponses => b"openai-responses",
            crate::ProtocolFamily::ChatGptCodex => b"chatgpt-codex",
            crate::ProtocolFamily::AnthropicMessages
            | crate::ProtocolFamily::VertexAnthropic
            | crate::ProtocolFamily::OpenAiCompatible
            | crate::ProtocolFamily::BedrockConverse
            | crate::ProtocolFamily::GoogleGemini
            | crate::ProtocolFamily::CohereChat
            | crate::ProtocolFamily::WatsonxChat
            | crate::ProtocolFamily::VertexGemini => return None,
        };
        let coordinates = Self::from_input(input).ok()?;
        let mut hash = sha2::Sha256::new();
        hash.update(b"secode/responses-reasoning-binding/v1\0");
        hash.update((family.len() as u64).to_be_bytes());
        hash.update(family);
        hash.update(coordinates.0);
        Some(Self(hash.finalize().into()))
    }

    /// The binding of a provider compaction item: the protocol family, route,
    /// account and credential version. Unlike [`Self::for_reasoning`] it
    /// leaves out the endpoint URL, so the product can check a stored item
    /// against its selected target BEFORE building a request and recover from
    /// durable history instead of sending an item the route cannot decrypt.
    pub(crate) fn for_compaction(input: &crate::transport::adapter::AttemptInput) -> Option<Self> {
        Self::compaction_parts(
            input.request.family(),
            input.endpoint.route(),
            input.lease.account(),
            input.lease.credential_version(),
        )
    }

    pub(crate) fn for_search(input: &crate::transport::adapter::AttemptInput) -> Option<Self> {
        let coordinates = Self::for_reasoning(input)?;
        let mut hash = sha2::Sha256::new();
        hash.update(b"secode/responses-native-search-binding/v1\0");
        hash.update(coordinates.0);
        Some(Self(hash.finalize().into()))
    }

    fn compaction_parts(
        family: crate::ProtocolFamily,
        route: &grapt_types::ModelRouteId,
        account: &grapt_types::AccountId,
        credential: &grapt_types::CredentialVersion,
    ) -> Option<Self> {
        let family: &str = match family {
            crate::ProtocolFamily::OpenAiResponses => "openai-responses",
            crate::ProtocolFamily::ChatGptCodex => "chatgpt-codex",
            crate::ProtocolFamily::AnthropicMessages
            | crate::ProtocolFamily::VertexAnthropic
            | crate::ProtocolFamily::OpenAiCompatible
            | crate::ProtocolFamily::BedrockConverse
            | crate::ProtocolFamily::GoogleGemini
            | crate::ProtocolFamily::CohereChat
            | crate::ProtocolFamily::WatsonxChat
            | crate::ProtocolFamily::VertexGemini => return None,
        };
        let mut hash = sha2::Sha256::new();
        hash.update(b"secode/responses-compaction-binding/v1\0");
        for field in [
            family,
            route.as_str(),
            account.as_str(),
            credential.as_str(),
        ] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        Some(Self(hash.finalize().into()))
    }

    fn from_fields(fields: [&str; 4]) -> Result<Self, &'static str> {
        let mut hash = sha2::Sha256::new();
        hash.update(b"secode/anthropic-replay-binding/v1\0");
        for field in fields {
            if field.len() > 16 * 1024 {
                return Err("replay binding field exceeds its byte bound");
            }
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        Ok(Self(hash.finalize().into()))
    }

    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self([0; 32])
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ReplayPayload {
    BedrockThinking(bedrock::Thinking),
    GeminiPart(serde_json::Value),
    NativeSearch(crate::native_search::SearchHistoryItem),
    AnthropicSearch(anthropic::Search),
    AnthropicText(anthropic::Text),
    Signed {
        thinking: String,
        signature: String,
    },
    Redacted {
        data: String,
    },
    /// A Responses `reasoning` output item, reduced to the fields the route
    /// accepts back as input: `type`, `id`, `summary` and the opaque
    /// `encrypted_content`, with the length of that serialized form.
    ResponsesReasoning {
        item: serde_json::Value,
        bytes: usize,
    },
    /// A Responses `compaction` output item (`type`, `id` and the opaque
    /// `encrypted_content`), with the length of that serialized form.
    ResponsesCompaction {
        item: serde_json::Value,
        bytes: usize,
    },
}

/// The most one provider compaction item may serialize to.
pub const MAX_COMPACTION_ITEM_BYTES: usize = 64 * 1024;
/// Why a provider compaction item was not retained, for the fallback record.
pub const COMPACTION_OVERSIZE: &str = "provider compaction item exceeds its byte bound";
pub const COMPACTION_MALFORMED: &str = "provider compaction item is malformed";

/// The most one replayed reasoning item may serialize to, as the request
/// carries it.
pub const MAX_REASONING_ITEM_BYTES: usize = 64 * 1024;

/// The coordinates a provider compaction item is bound to, taken from the
/// selected target: [`ProviderReplay::redeemable_for`] compares against them.
#[derive(Clone, PartialEq, Eq)]
pub struct CompactionTarget {
    model: ModelRef,
    route: grapt_types::ModelRouteId,
    account: grapt_types::AccountId,
    credential: grapt_types::CredentialVersion,
    family: crate::ProtocolFamily,
    effort: crate::EffortPolicy,
}

impl CompactionTarget {
    /// Request-only generation compatibility. Does not change the target's
    /// provider, model, route, account or credential binding.
    pub fn with_effort_policy(mut self, effort: crate::EffortPolicy) -> Self {
        self.effort = effort;
        self
    }
    pub fn effort_policy(&self) -> crate::EffortPolicy {
        self.effort
    }
    pub fn model(&self) -> &ModelRef {
        &self.model
    }
    pub fn family(&self) -> crate::ProtocolFamily {
        self.family
    }

    pub fn of(target: &grapt_types::ResolvedModelTarget, family: crate::ProtocolFamily) -> Self {
        Self {
            model: target.model_ref().clone(),
            route: target.route().clone(),
            account: target.account().clone(),
            credential: target.credential_version().clone(),
            family,
            effort: crate::EffortPolicy::default(),
        }
    }
}

impl std::fmt::Debug for CompactionTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompactionTarget([REDACTED])")
    }
}

/// Closed adapter replay. Canonical history stores opaque artifact bytes;
/// signatures are never prose or portable across selected targets.
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderReplay {
    provider: ProviderId,
    model: ModelId,
    variant: Option<ModelVariantId>,
    payload: ReplayPayload,
    instruction_epochs: bool,
    binding: ReplayBinding,
}

impl std::fmt::Debug for ProviderReplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProviderReplay([REDACTED])")
    }
}

impl ProviderReplay {
    pub fn restore(
        provider: ProviderId,
        model: ModelId,
        variant: Option<ModelVariantId>,
        bytes: &str,
    ) -> Result<Self, ReplayError> {
        if bytes.len() > AdapterState::MAX_BYTES {
            return Err("provider replay exceeds its byte bound".into());
        }
        let value: serde_json::Value =
            serde_json::from_str(bytes).map_err(|_| "provider replay artifact is malformed")?;
        let object = value
            .as_object()
            .ok_or("provider replay is not an object")?;
        let payload = match value["kind"].as_str() {
            Some("bedrock-reasoning-v1") if object.len() == 3 => {
                ReplayPayload::BedrockThinking(bedrock::Thinking::restore(&value["item"])?)
            }
            Some("anthropic-thinking-v1" | "anthropic-thinking-v2") if object.len() == 4 => {
                let signature = value["signature"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .ok_or("provider replay signature is missing")?;
                ReplayPayload::Signed {
                    thinking: value["thinking"]
                        .as_str()
                        .ok_or("provider replay thinking is missing")?
                        .into(),
                    signature: signature.into(),
                }
            }
            Some("anthropic-redacted-thinking-v1" | "anthropic-redacted-thinking-v2")
                if object.len() == 3 =>
            {
                ReplayPayload::Redacted {
                    data: value["data"]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .ok_or("redacted thinking payload is missing")?
                        .into(),
                }
            }
            Some("openai-reasoning-v1") if object.len() == 3 => {
                let (item, bytes) = reasoning_item(&value["item"])
                    .ok_or("provider replay reasoning item is malformed")?;
                ReplayPayload::ResponsesReasoning { item, bytes }
            }
            Some("openai-compaction-v1") if object.contains_key("rejected") => {
                return Err(ReplayError::CompactionOversize)
            }
            Some("openai-compaction-v1") if object.len() == 3 => {
                let (item, bytes) = compaction_item(&value["item"])?;
                ReplayPayload::ResponsesCompaction { item, bytes }
            }
            Some("openai-web-search-v1") if object.len() == 3 => ReplayPayload::NativeSearch(
                crate::native_search::SearchHistoryItem::from_output(&value["item"])
                    .map_err(|_| "native web search replay is malformed")?,
            ),
            Some("anthropic-web-search-v1") if object.len() == 3 => {
                let pair = value["item"]
                    .as_array()
                    .filter(|pair| pair.len() == 2)
                    .ok_or("Anthropic search replay is not a pair")?;
                ReplayPayload::AnthropicSearch(anthropic::Search::restore(&pair[0], &pair[1])?)
            }
            Some("anthropic-text-v1") if object.len() == 4 => {
                ReplayPayload::AnthropicText(anthropic::Text::restore(
                    &value["item"],
                    value["rendered"]
                        .as_str()
                        .ok_or("Anthropic rendered text is missing")?,
                )?)
            }
            Some("gemini-part-v1") if object.len() == 3 => {
                ReplayPayload::GeminiPart(crate::gemini::replay::part(&value["item"])?)
            }
            _ => return Err("provider replay format is unsupported".into()),
        };
        let encoded = value["binding"]
            .as_array()
            .filter(|bytes| bytes.len() == 32)
            .ok_or("provider replay binding is missing")?;
        let mut binding = [0u8; 32];
        for (index, byte) in encoded.iter().enumerate() {
            binding[index] = byte
                .as_u64()
                .and_then(|value| u8::try_from(value).ok())
                .ok_or("provider replay binding is malformed")?;
        }
        Ok(Self {
            provider,
            model,
            variant,
            payload,
            instruction_epochs: matches!(
                value["kind"].as_str(),
                Some("anthropic-thinking-v2" | "anthropic-redacted-thinking-v2")
            ),
            binding: ReplayBinding(binding),
        })
    }
    /// Old native signed state must retain the prefix representation that produced it.
    pub fn requires_legacy_anthropic_prefix(&self) -> bool {
        !self.instruction_epochs && self.is_anthropic_thinking()
    }
    /// Thinking signatures require the exact conversation prefix that produced
    /// them. A fork can retain conversation text without retaining this state.
    pub fn is_anthropic_thinking(&self) -> bool {
        matches!(
            self.payload,
            ReplayPayload::Signed { .. } | ReplayPayload::Redacted { .. }
        )
    }
    pub fn is_bedrock_thinking(&self) -> bool {
        matches!(self.payload, ReplayPayload::BedrockThinking(_))
    }
    pub(crate) fn bedrock_block(&self) -> Option<serde_json::Value> {
        match &self.payload {
            ReplayPayload::BedrockThinking(thinking) => Some(thinking.block()),
            _ => None,
        }
    }
    pub fn is_anthropic_search(&self) -> bool {
        matches!(self.payload, ReplayPayload::AnthropicSearch(_))
    }
    pub fn is_gemini(&self) -> bool {
        matches!(self.payload, ReplayPayload::GeminiPart(_))
    }
    pub(crate) fn gemini_part(&self) -> Option<&serde_json::Value> {
        match &self.payload {
            ReplayPayload::GeminiPart(part) => Some(part),
            _ => None,
        }
    }
    pub fn gemini_rendered_text(&self) -> Option<&str> {
        self.gemini_part()
            .filter(|part| part["thought"] != true)?
            .get("text")?
            .as_str()
    }
    pub fn is_anthropic_text(&self) -> bool {
        matches!(self.payload, ReplayPayload::AnthropicText(_))
    }
    /// Exact visible text captured atomically beside the native text block.
    pub fn anthropic_rendered_text(&self) -> Option<&str> {
        match &self.payload {
            ReplayPayload::AnthropicText(text) => Some(&text.rendered),
            _ => None,
        }
    }

    /// Thinking is signed over the conversation prefix that produced it.
    /// Rebuilding that prefix invalidates earlier blocks, while later blocks
    /// can still carry the current tool-use exchange.
    pub fn can_replay_after_prefix_rewrite(
        &self,
        produced_at: EventSeq,
        rewritten_at: Option<EventSeq>,
    ) -> bool {
        !(self.is_anthropic_thinking() || self.is_bedrock_thinking())
            || rewritten_at.is_none_or(|rewrite| produced_at > rewrite)
    }
    pub fn matches(&self, model: &ModelRef) -> bool {
        &self.provider == model.provider()
            && &self.model == model.model()
            && self.variant.as_ref() == model.variant()
    }
    pub(crate) fn matches_effort(&self, model: &ModelRef, effort: crate::EffortPolicy) -> bool {
        &self.provider == model.provider()
            && &self.model == model.model()
            && effort.variants_match(self.variant.as_ref(), model.variant())
    }
    /// Compacted context can retain generation overrides. A changed effort
    /// needs chronological support and a known value to reassert after it.
    pub(crate) fn matches_compaction(&self, model: &ModelRef, effort: crate::EffortPolicy) -> bool {
        self.matches(model)
            || (effort.updates()
                && effort.resolve(model.variant()).is_some()
                && self.matches_effort(model, effort))
    }
    pub fn byte_len(&self) -> usize {
        match &self.payload {
            ReplayPayload::BedrockThinking(thinking) => thinking.bytes(),
            ReplayPayload::NativeSearch(item) => item.byte_len(),
            ReplayPayload::GeminiPart(part) => part.to_string().len(),
            ReplayPayload::AnthropicSearch(search) => search.bytes,
            ReplayPayload::AnthropicText(text) => text.bytes,
            ReplayPayload::Signed {
                thinking,
                signature,
            } => thinking.len().saturating_add(signature.len()),
            ReplayPayload::Redacted { data } => data.len(),
            ReplayPayload::ResponsesReasoning { bytes, .. }
            | ReplayPayload::ResponsesCompaction { bytes, .. } => *bytes,
        }
    }
    /// The already-executed native call represented by this history item.
    pub fn native_search_call(&self) -> Option<&grapt_types::ToolCallId> {
        match &self.payload {
            ReplayPayload::NativeSearch(item) => Some(item.call()),
            ReplayPayload::AnthropicSearch(search) => Some(&search.call),
            _ => None,
        }
    }

    /// Whether this replays a Responses compaction item.
    pub fn is_responses_compaction(&self) -> bool {
        matches!(self.payload, ReplayPayload::ResponsesCompaction { .. })
    }
    /// Whether this compaction item may be sent to `target` over `family`:
    /// the same provider and model, an equal variant or a recognized effort
    /// selection under explicit generation policy, and the same family, route,
    /// account and credential version that produced it. Anything else is
    /// recovered from durable history rather than sent.
    pub fn redeemable_for(&self, target: &CompactionTarget) -> bool {
        self.is_responses_compaction()
            && self.matches_compaction(&target.model, target.effort)
            && ReplayBinding::compaction_parts(
                target.family,
                &target.route,
                &target.account,
                &target.credential,
            )
            .is_some_and(|binding| self.binding == binding)
    }
    /// Whether this replays a Responses reasoning item rather than Anthropic
    /// thinking.
    pub fn is_responses_reasoning(&self) -> bool {
        matches!(self.payload, ReplayPayload::ResponsesReasoning { .. })
    }
    pub(crate) fn responses_item(&self) -> Option<&serde_json::Value> {
        match &self.payload {
            ReplayPayload::ResponsesReasoning { item, .. }
            | ReplayPayload::ResponsesCompaction { item, .. } => Some(item),
            ReplayPayload::NativeSearch(item) => Some(item.item()),
            ReplayPayload::BedrockThinking(_)
            | ReplayPayload::Signed { .. }
            | ReplayPayload::GeminiPart(_)
            | ReplayPayload::Redacted { .. }
            | ReplayPayload::AnthropicSearch(_)
            | ReplayPayload::AnthropicText(_) => None,
        }
    }
    /// Every byte this replay carries, as length-prefixed fields, for
    /// [`crate::request::prefix_digest`].
    pub(crate) fn digest_fields(&self, field: &mut impl FnMut(&[u8])) {
        if self.instruction_epochs {
            field(b"anthropic-instruction-epochs-v1");
        }
        field(self.provider.as_str().as_bytes());
        field(self.model.as_str().as_bytes());
        field(
            self.variant
                .as_ref()
                .map_or(&[][..], |variant| variant.as_str().as_bytes()),
        );
        field(&self.binding.0);
        match &self.payload {
            ReplayPayload::BedrockThinking(thinking) => thinking.digest(field),
            ReplayPayload::GeminiPart(part) => {
                field(b"gemini-part-v1");
                field(part.to_string().as_bytes());
            }
            ReplayPayload::AnthropicSearch(search) => {
                field(b"anthropic-web-search-v1");
                field(search.item.to_string().as_bytes());
            }
            ReplayPayload::AnthropicText(text) => {
                field(b"anthropic-text-v1");
                field(text.item.to_string().as_bytes());
                field(text.rendered.as_bytes());
            }
            ReplayPayload::NativeSearch(item) => {
                field(b"native-web-search");
                field(item.item().to_string().as_bytes());
            }
            ReplayPayload::Signed {
                thinking,
                signature,
            } => {
                field(b"signed");
                field(thinking.as_bytes());
                field(signature.as_bytes());
            }
            ReplayPayload::Redacted { data } => {
                field(b"redacted");
                field(data.as_bytes());
            }
            ReplayPayload::ResponsesReasoning { item, .. } => {
                field(b"reasoning");
                field(item.to_string().as_bytes());
            }
            ReplayPayload::ResponsesCompaction { item, .. } => {
                field(b"compaction");
                field(item.to_string().as_bytes());
            }
        }
    }
    pub(crate) fn bound_to(&self, binding: &ReplayBinding) -> bool {
        &self.binding == binding
    }
    pub(crate) fn anthropic_block(&self) -> Option<serde_json::Value> {
        match &self.payload {
            ReplayPayload::AnthropicSearch(search) => Some(search.item.clone()),
            ReplayPayload::AnthropicText(text) => Some(text.item.clone()),
            ReplayPayload::Signed {
                thinking,
                signature,
            } => Some(
                serde_json::json!({"type":"thinking", "thinking":thinking, "signature":signature}),
            ),
            ReplayPayload::Redacted { data } => {
                Some(serde_json::json!({"type":"redacted_thinking", "data":data}))
            }
            ReplayPayload::BedrockThinking(_)
            | ReplayPayload::NativeSearch(_)
            | ReplayPayload::GeminiPart(_)
            | ReplayPayload::ResponsesReasoning { .. }
            | ReplayPayload::ResponsesCompaction { .. } => None,
        }
    }
}

pub(crate) fn thinking_state(
    thinking: String,
    signature: String,
    binding: &ReplayBinding,
    mode: crate::AnthropicInstructionMode,
) -> Result<AdapterState, &'static str> {
    let kind = match mode {
        crate::AnthropicInstructionMode::CurrentPrefix => "anthropic-thinking-v1",
        crate::AnthropicInstructionMode::Epochs => "anthropic-thinking-v2",
    };
    let bytes = serde_json::json!({"kind":kind, "thinking":thinking,"signature":signature,"binding":binding.0}).to_string();
    AdapterState::retain(bytes).map_err(|_| "thinking replay exceeds its byte bound")
}

pub(crate) fn redacted_state(
    data: String,
    binding: &ReplayBinding,
    mode: crate::AnthropicInstructionMode,
) -> Result<AdapterState, &'static str> {
    let kind = match mode {
        crate::AnthropicInstructionMode::CurrentPrefix => "anthropic-redacted-thinking-v1",
        crate::AnthropicInstructionMode::Epochs => "anthropic-redacted-thinking-v2",
    };
    let bytes = serde_json::json!({"kind":kind,"data":data,"binding":binding.0}).to_string();
    AdapterState::retain(bytes).map_err(|_| "redacted thinking exceeds its replay bound")
}

/// The replay state of a Responses `reasoning` output item, or `None` when it
/// carries no encrypted content, is malformed, or exceeds
/// [`MAX_REASONING_ITEM_BYTES`]. Such an item is not replayed; the turn goes on.
pub(crate) fn reasoning_state(
    item: &serde_json::Value,
    binding: &ReplayBinding,
) -> Option<AdapterState> {
    let (item, _) = reasoning_item(item)?;
    let bytes = serde_json::json!({"kind":"openai-reasoning-v1","item":item,"binding":binding.0})
        .to_string();
    AdapterState::retain(bytes).ok()
}

/// The fields of a reasoning item the Responses route takes back as input, and
/// their serialized length, when they are well formed and serialize within
/// [`MAX_REASONING_ITEM_BYTES`]. The stream's frame bound already caps the
/// item read here at [`AdapterState::MAX_BYTES`].
fn reasoning_item(item: &serde_json::Value) -> Option<(serde_json::Value, usize)> {
    if item["type"].as_str() != Some("reasoning") {
        return None;
    }
    let id = item["id"].as_str().filter(|id| !id.is_empty())?;
    let encrypted = item["encrypted_content"]
        .as_str()
        .filter(|content| !content.is_empty())?;
    let summary = item["summary"]
        .as_array()?
        .iter()
        .map(|part| {
            (part["type"].as_str() == Some("summary_text"))
                .then(|| part["text"].as_str())
                .flatten()
                .map(|text| serde_json::json!({"type":"summary_text","text":text}))
        })
        .collect::<Option<Vec<_>>>()?;
    let item = serde_json::json!({
        "type": "reasoning",
        "id": id,
        "summary": summary,
        "encrypted_content": encrypted,
    });
    // Counted as serialized: empty summary parts still cost request bytes.
    let bytes = item.to_string().len();
    (bytes <= MAX_REASONING_ITEM_BYTES).then_some((item, bytes))
}

/// Refuse a request carrying a compaction item this attempt cannot send. The
/// product checks [`ProviderReplay::redeemable_for`] first and recovers; this
/// is the boundary's own check, so a mismatch never reaches the route.
pub(crate) fn refuse_unbound_compaction(
    input: &crate::transport::adapter::AttemptInput,
) -> Result<(), &'static str> {
    let binding = ReplayBinding::for_compaction(input);
    let model = input.request.model();
    let items = match &input.request {
        crate::ProviderRequest::OpenAiResponses(request) => &request.input,
        crate::ProviderRequest::ChatGptCodex(request) => &request.input,
        crate::ProviderRequest::AnthropicMessages(request)
        | crate::ProviderRequest::VertexAnthropic(request) => &request.messages,
        crate::ProviderRequest::OpenAiCompatible(request)
        | crate::ProviderRequest::WatsonxChat(request) => &request.messages,
        crate::ProviderRequest::BedrockConverse(request) => &request.messages,
        crate::ProviderRequest::GoogleGemini(request)
        | crate::ProviderRequest::VertexGemini(request) => &request.messages,
        crate::ProviderRequest::CohereChat(request) => &request.messages,
    };
    let updates = match &input.request {
        crate::ProviderRequest::ChatGptCodex(request) => {
            crate::effort::projection(&request.input, &request.model, request.reasoning.effort)
                .updates
        }
        _ => false,
    };
    let mut needs_update = false;
    for item in items {
        if let crate::CodexInputItem::ProviderReplay(replay) = item {
            if replay.is_responses_compaction()
                && !(replay.matches_compaction(model, crate::effort::policy(&input.request))
                    && binding
                        .as_ref()
                        .is_some_and(|binding| replay.bound_to(binding)))
            {
                return Err("provider compaction does not match this route, account or model");
            }
            if replay.is_responses_compaction() && !replay.matches(model) {
                needs_update = true;
            }
        } else if matches!(item, crate::CodexInputItem::EffortUpdate(_)) && updates {
            needs_update = false;
        } else if needs_update
            && (matches!(item, crate::CodexInputItem::ImageUser { .. })
                || matches!(item, crate::CodexInputItem::Message(message) if message.role == crate::MessageRole::User))
        {
            return Err("changed compaction effort requires a fresh update before User input");
        }
    }
    if needs_update {
        return Err("changed compaction effort requires a fresh update before User input");
    }
    Ok(())
}

/// The replay state of a Responses `compaction` output item. Always produced,
/// so a compaction call can tell a missing item from a rejected one: an item
/// too large to retain becomes a marker that restores as
/// [`COMPACTION_OVERSIZE`].
pub(crate) fn compaction_state(item: &serde_json::Value, binding: &ReplayBinding) -> AdapterState {
    let bytes = serde_json::json!({"kind":"openai-compaction-v1","item":item,"binding":binding.0})
        .to_string();
    AdapterState::retain(bytes).unwrap_or_else(|_| {
        AdapterState::retain(
            serde_json::json!({"kind":"openai-compaction-v1","rejected":"oversize","binding":binding.0})
                .to_string(),
        )
        .unwrap_or_else(|_| AdapterState::none())
    })
}

/// A retained compaction state bound to `target`, as a compaction call would
/// retain it there. For product journeys that commit a provider compaction
/// without a live route.
#[cfg(any(test, feature = "loopback-testkit"))]
pub fn compaction_fixture_state(item: &serde_json::Value, target: &CompactionTarget) -> String {
    let binding = ReplayBinding::compaction_parts(
        target.family,
        &target.route,
        &target.account,
        &target.credential,
    )
    .unwrap_or(ReplayBinding([0; 32]));
    serde_json::json!({"kind":"openai-compaction-v1","item":item,"binding":binding.0}).to_string()
}

/// The fields of a compaction item the route takes back as input, and their
/// serialized length, within [`MAX_COMPACTION_ITEM_BYTES`].
fn compaction_item(item: &serde_json::Value) -> Result<(serde_json::Value, usize), ReplayError> {
    if item["type"].as_str() != Some("compaction") {
        return Err(COMPACTION_MALFORMED.into());
    }
    let id = item["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or(COMPACTION_MALFORMED)?;
    let encrypted = item["encrypted_content"]
        .as_str()
        .filter(|content| !content.is_empty())
        .ok_or(COMPACTION_MALFORMED)?;
    let item = serde_json::json!({"type": "compaction", "id": id, "encrypted_content": encrypted});
    let bytes = item.to_string().len();
    if bytes > MAX_COMPACTION_ITEM_BYTES {
        return Err(ReplayError::CompactionOversize);
    }
    Ok((item, bytes))
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;

/// Native history stays opaque until it reaches its generating adapter again.
pub(crate) fn search_state(
    item: &crate::native_search::SearchHistoryItem,
    binding: &ReplayBinding,
) -> Result<AdapterState, &'static str> {
    AdapterState::retain(
        serde_json::json!({"kind":"openai-web-search-v1","item":item.item(),"binding":binding.0})
            .to_string(),
    )
    .map_err(|_| "native web search replay exceeds its byte bound")
}
