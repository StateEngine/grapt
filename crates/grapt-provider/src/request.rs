//! Provider request boundary — one named request type per protocol family.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Provider request boundary".
//!
//! # The defect this closes
//!
//! The oracle builds one generic body and then *removes* fields the route
//! cannot accept. The oracle's `openai-responses.ts:122`:
//!
//! ```text
//! const { max_output_tokens: _maxOutputTokens, ...withoutMaxOutputTokens } = body
//! return {
//!   ...(supportsMaxOutputTokens === false ? withoutMaxOutputTokens : body),
//! ```
//!
//! The removal is driven by a runtime compatibility flag, and the guard is a
//! test asserting absence — the oracle's `model-resolver.test.ts:405`:
//! `expect(prepared.body).not.toHaveProperty("max_output_tokens")`. That is a
//! recurring regression: nothing stops the field being reintroduced upstream,
//! and the only thing standing between it and the wire is a destructuring
//! line someone must remember to keep.
//!
//! The contract is explicit: "The existing oracle regression
//! proves why an assertion that a serializer strips the field is weaker." So
//! here the field is not stripped — on the Codex route it **does not exist**.
//! [`ChatGptCodexRequest`] has no `max_output_tokens` member, so the
//! regression is a compile error rather than a test someone can delete.
//!
//! # Forbidden by contract, and absent here
//!
//! Generic optional parameter bags, flattened provider maps, post-hoc
//! request-key deletion, JSON merge patches at the provider boundary, and
//! `HashMap<String, serde_json::Value>` request surfaces. There is deliberately
//! no `extra`, `options`, or `provider_metadata` field anywhere in this module.
//!
//! # Scope
//!
//! The field sets below contain the parameters the first fixtures
//! actually exercise plus the ones that differ between routes, which is what
//! makes the boundary meaningful. Each adapter extends its own type as its
//! fixtures demand. Growth happens per-route, never through a shared bag.
//!
//! Serialization is not here. The contract reserves it: "The selected adapter
//! alone may serialize its request into HTTP bytes."

use crate::CohereChatRequest;
use grapt_policy::ToolName;
use grapt_types::{ModelRef, SessionId, ToolCallId};

/// Maximum UTF-8 bytes accepted in one Codex function-call argument or
/// function-call output item. This matches the runtime's durable tool capture
/// ceiling, so continuation can never widen an already bounded artifact.
pub const MAX_CODEX_TOOL_TEXT_BYTES: usize = 1_024 * 1_024;

mod cache;
pub use cache::{PromptCacheKey, PromptCacheKeyError, MAX_PROMPT_CACHE_KEY_BYTES};

/// A positive output-token bound.
///
/// Zero is unrepresentable: a request for zero output tokens is not a
/// meaningful bound, and the oracle's optional number cannot say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MaxOutputTokens(u32);

impl MaxOutputTokens {
    pub fn new(value: u32) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(Self(value))
        }
    }

    pub fn value(self) -> u32 {
        self.0
    }
}

/// How the model may use tools. Closed, per route-independent semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Named { tool: String },
}

/// Tool input syntax; text tools retain a JSON fallback for function-only routes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ToolInputFormat {
    #[default]
    Json,
    /// A Lark grammar for provider-native text input. The normalized input is
    /// the JSON object `{ "input": <text> }` on every route.
    Text { grammar: String },
}

/// A tool offered to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDefinition {
    pub input_format: ToolInputFormat,
    pub name: String,
    /// Concise model-facing semantics. `None` omits the field on every wire
    /// family; callers must not encode absence as an empty description.
    pub description: Option<String>,
    /// JSON Schema for the tool input, carried as text. It is opaque to the
    /// runtime and is not a place to smuggle request parameters.
    pub input_schema: String,
}

/// One turn of conversation input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputMessage {
    pub role: MessageRole,
    pub content: String,
}

/// Bounded model-facing text carried by a Codex tool call or output item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexToolText(String);

impl CodexToolText {
    pub fn try_new(value: String) -> Result<Self, CodexToolTextTooLarge> {
        if value.len() > MAX_CODEX_TOOL_TEXT_BYTES {
            return Err(CodexToolTextTooLarge { bytes: value.len() });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexToolTextTooLarge {
    pub bytes: usize,
}

impl core::fmt::Display for CodexToolTextTooLarge {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "Codex tool text carries {} bytes, exceeding the {}-byte bound",
            self.bytes, MAX_CODEX_TOOL_TEXT_BYTES
        )
    }
}

impl std::error::Error for CodexToolTextTooLarge {}

/// Closed conversation input shared by the concrete request families.
///
/// These are protocol-neutral typed facts. Provider-shaped JSON exists only
/// in the route adapter that serializes this enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexInputItem {
    /// Request-local boundary of one durable assistant response. Never emitted
    /// as a vendor item; keeps adjacent responses distinct during projection.
    AssistantResponseStart,
    /// Trusted, request-local generation selection before its originating input.
    EffortUpdate(crate::ReasoningEffort),
    ProviderReplay(crate::replay::ProviderReplay),
    /// Committed reasoning for an explicitly opted-in compatible endpoint.
    CompatibleReasoning(CodexToolText),
    Message(InputMessage),
    /// User-supplied image content, kept separate from prose and file paths.
    ImageUser {
        text: CodexToolText,
        images: crate::InputImages,
    },
    ToolCall {
        call: ToolCallId,
        tool: ToolName,
        arguments: CodexToolText,
    },
    /// Provider-neutral text tool input, reconstructed from ordinary committed
    /// JSON arguments by the product's tool registry. No vendor event is stored.
    TextToolCall {
        call: ToolCallId,
        tool: ToolName,
        input: CodexToolText,
    },
    ToolOutput {
        call: ToolCallId,
        output: CodexToolText,
        is_error: bool,
    },
    /// A successful image tool result; failures remain ordinary ToolOutput.
    ImageToolOutput {
        call: ToolCallId,
        output: CodexToolText,
        images: crate::InputImages,
    },
    /// Ask an OpenAI Responses route to compact the input before it: the
    /// response carries the provider's opaque compaction item. Only the
    /// Responses families accept it.
    CompactionTrigger,
}

pub use crate::gemini::GoogleGeminiRequest;

mod prefix;
pub use prefix::prefix_digest;
mod tool_arguments;

impl From<InputMessage> for CodexInputItem {
    fn from(message: InputMessage) -> Self {
        Self::Message(message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// Anthropic thinking. Anthropic-only by construction — no other
/// request type in this module has a field of this type.
///
/// Copyright 2023-2026 SpaceXAI.
/// Apache-2.0. Modified by Grapt: no serde dependency, adaptive summary only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingConfig {
    /// Legacy models require a caller-set budget below the output bound.
    Enabled { budget_tokens: u32 },
    /// Adaptive models choose their own budget; expose the supported summary.
    Adaptive,
}

/// Provenance for opaque native thinking state; this is not a vendor parameter.
/// Existing signed history keeps its original instruction representation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AnthropicInstructionMode {
    #[default]
    CurrentPrefix,
    Epochs,
}

/// Anthropic prompt caching. Anthropic-only by construction, like
/// [`ThinkingConfig`]: the OpenAI routes key their cache by
/// [`PromptCacheKey`] and have no field of this type.
///
/// The adapter owns marker placement. A caller chooses whether the request asks
/// for caching; it cannot place, count or time-limit individual markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicPromptCache {
    /// No `cache_control` marker is emitted.
    Off,
    /// Mark the stable prefix boundaries — the last tool, the end of the system
    /// instructions and the conversation tail — with a one-hour ephemeral
    /// lifetime. Markers are requests for reuse, never evidence of it.
    StablePrefix,
}

/// Prompt-cache markers on the OpenAI-compatible chat route.
///
/// Only Anthropic models behind a compatible gateway (OpenRouter) need explicit
/// breakpoints; other models cache an unchanged prefix implicitly and get
/// none. As with [`AnthropicPromptCache`], the caller chooses whether to ask
/// and the adapter owns placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatiblePromptCache {
    /// No `cache_control` marker is emitted.
    Off,
    /// The oracle's rule (`ProviderTransform` caching): the first two system
    /// messages and the last two other messages that carry text get an
    /// ephemeral `cache_control` on their content, four markers at most.
    AnthropicBreakpoints,
}

/// OpenAI Responses API request.
///
/// This route *does* support `max_output_tokens`, so the field exists here.
/// It has no `thinking` — that is Anthropic's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiResponsesRequest {
    pub model: ModelRef,
    pub input: Vec<CodexInputItem>,
    pub prompt_cache_key: Option<PromptCacheKey>,
    pub max_output_tokens: Option<MaxOutputTokens>,
    pub tools: Vec<ToolDefinition>,
    pub web_search: Option<crate::WebSearchSpec>,
    pub tool_choice: Option<ToolChoice>,
}

/// ChatGPT/Codex backend request.
///
/// Deliberately has **no** `max_output_tokens` field. The Codex backend
/// rejects it, and the oracle's answer was to build it and delete it again
/// (`openai-responses.ts:122`). Here it is unnameable, so the recurring
/// regression cannot be reintroduced without a compile error.
///
/// Compile-fail probe 04 is the counterexample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptCodexRequest {
    pub model: ModelRef,
    pub input: Vec<CodexInputItem>,
    pub prompt_cache_key: Option<PromptCacheKey>,
    pub tools: Vec<ToolDefinition>,
    pub web_search: Option<crate::WebSearchSpec>,
    pub tool_choice: Option<ToolChoice>,
    /// What the request asks of the model's reasoning, from the route's
    /// account catalog (see [`CodexReasoning`]).
    pub reasoning: CodexReasoning,
    /// The execution's sticky-routing token (see [`CodexTurn`]).
    pub turn: CodexTurn,
}

/// The Codex route's sticky-routing token for one execution (Codex 0.155.1
/// client.rs, `x-codex-turn-state`): the first response of a turn carries it
/// and every later request of that turn sends it back, so the turn's requests
/// reach the backend holding its cache. One handle is shared by an
/// execution's requests and never by another execution's; the token is sent
/// only for the account that received it. The stable thread identity is kept
/// separately from the prompt cache bucket, which may be shared by a project.
/// Never displayed.
#[derive(Clone, Default)]
pub struct CodexTurn {
    state: std::sync::Arc<std::sync::Mutex<Option<(grapt_types::AccountId, String)>>>,
    thread: Option<SessionId>,
}

/// The most a sticky-routing token may hold.
const MAX_TURN_STATE_BYTES: usize = 4096;

impl CodexTurn {
    /// A new execution's routing state, bound to its actual durable Session.
    pub fn for_session(session: SessionId) -> Self {
        Self {
            thread: Some(session),
            ..Self::default()
        }
    }

    pub(crate) fn thread_id(&self) -> Option<&str> {
        self.thread.as_ref().map(SessionId::as_str)
    }

    /// The token to send for `account`, once this turn received one for it.
    pub(crate) fn header_for(&self, account: &grapt_types::AccountId) -> Option<String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .as_ref()
            .filter(|(owner, _)| owner == account)
            .map(|(_, token)| token.clone())
    }

    /// Keep the token a response carried, unless this turn already holds one
    /// for the same account. A bounded, visible-ASCII token only.
    pub(crate) fn capture(&self, account: &grapt_types::AccountId, token: &str) {
        if token.is_empty()
            || token.len() > MAX_TURN_STATE_BYTES
            || !token.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.as_ref().is_none_or(|(owner, _)| owner != account) {
            *state = Some((account.clone(), token.to_owned()));
        }
    }
}

/// Routing state never distinguishes one request from another.
impl PartialEq for CodexTurn {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for CodexTurn {}

impl std::fmt::Debug for CodexTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CodexTurn([REDACTED])")
    }
}

/// The reasoning a ChatGPT/Codex request asks for, from the account
/// catalog's model capabilities. Constant for a selected
/// model, so a Session's request prefix does not change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexReasoning {
    /// Account-advertised chronological effort support and known default.
    pub effort: crate::EffortPolicy,
    /// `use_responses_lite`: keep every turn's reasoning in context
    /// (`reasoning.context: "all_turns"`). The Codex-internal Responses Lite
    /// header is not sent: it requires parallel tool calls off and changes the
    /// rendered prompt.
    pub all_turns: bool,
    /// `supports_reasoning_summary_parameter` (or, where a row omits it,
    /// `supports_reasoning_summaries`): ask for a reasoning summary
    /// (`reasoning.summary: "auto"`), only where the route
    /// accepts the parameter. On the Luna OAuth route only such a
    /// request returns the encrypted reasoning items replay carries back.
    pub summary: bool,
}

/// Anthropic Messages API request.
///
/// `max_tokens` is **required** here, unlike the OpenAI routes' optional
/// bound — the Messages API rejects a request without it. An `Option` would
/// misrepresent the protocol, so the type does not offer one.
///
/// Responses-only cache routing is not an Anthropic request coordinate:
/// ```compile_fail
/// use grapt_provider::{AnthropicMessagesRequest, PromptCacheKey};
/// fn set_responses_cache_key(request: &mut AnthropicMessagesRequest, key: PromptCacheKey) {
///     request.prompt_cache_key = Some(key);
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicMessagesRequest {
    /// Trusted System context placed after the stable system cache boundary.
    /// The product extracts it by typed instruction identity, never text matching.
    pub uncached_system_suffix: Option<String>,
    /// Native effort configuration; unsupported models retain the API default.
    pub effort: crate::EffortPolicy,
    pub model: ModelRef,
    pub messages: Vec<CodexInputItem>,
    pub max_tokens: MaxOutputTokens,
    pub thinking: Option<ThinkingConfig>,
    pub instruction_mode: AnthropicInstructionMode,
    pub prompt_cache: AnthropicPromptCache,
    pub tools: Vec<ToolDefinition>,
    pub web_search: Option<crate::AnthropicWebSearchSpec>,
    pub tool_choice: Option<ToolChoice>,
}

/// An OpenAI-compatible chat-completions request.
///
/// This is the family with the widest membership and the least authority: Grok
/// (`api.x.ai`), gateways, and self-hosted servers all speak it.
///
/// # Why this is a family and not "OpenAI with a different base URL"
///
/// The chat-completions wire shape is genuinely a different protocol from the
/// Responses API — `messages` rather than `input`, `max_tokens` rather than
/// `max_output_tokens`, and `choices[].delta` rather than typed
/// `response.output_text.*` events. Folding it into [`OpenAiResponsesRequest`]
/// with a base-URL swap is the mistake the contract's forbidden "generic body
/// patch" is describing: the two routes would share a field set that neither
/// fully supports.
///
/// It has **no** `thinking` and no `max_output_tokens`: both are other routes'
/// fields, and on this one they are unnameable rather than stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiCompatibleRequest {
    pub reasoning_replay: CompatibleReasoningReplay,
    pub model: ModelRef,
    pub messages: Vec<CodexInputItem>,
    /// The chat-completions spelling. Optional, unlike Anthropic's required
    /// bound and unlike the Responses route's differently-named one.
    pub max_tokens: Option<MaxOutputTokens>,
    pub prompt_cache: CompatiblePromptCache,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
    /// The Session this request continues. Never part of the body: the
    /// adapter sends it only as the routing header of a session-routed
    /// gateway that requires one.
    pub routing_session: Option<PromptCacheKey>,
}

/// A compatible endpoint's declared reasoning continuation contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CompatibleReasoningReplay {
    #[default]
    None,
    ReasoningContent,
}

/// Native Amazon Bedrock Converse request; auth and endpoints are separate capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BedrockConverseRequest {
    pub model: ModelRef,
    pub messages: Vec<CodexInputItem>,
    pub max_tokens: MaxOutputTokens,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
}

/// The closed provider dispatch. One named protocol family per adapter.
///
/// The session runtime passes one of these — never `serde_json::Value`, a
/// generic body patch, or an `additionalModelRequestFields` equivalent.
///
/// # `Clone` is deliberate, and removing it would not be a seal
///
/// Removing `Clone` cannot stop a borrowed request from becoming an owned
/// duplicate beside a cloned receipt: every concrete per-route request keeps
/// public fields whose types are `Clone` or `Copy`, so anyone holding a borrow
/// rebuilds an identical owned value field by field. What closes that route is
/// not lending the request beside its receipt at all, which is a property of
/// the authority seam. See [`crate::authority`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRequest {
    GoogleGemini(GoogleGeminiRequest),
    CohereChat(CohereChatRequest),
    VertexGemini(GoogleGeminiRequest),
    OpenAiResponses(OpenAiResponsesRequest),
    ChatGptCodex(ChatGptCodexRequest),
    AnthropicMessages(AnthropicMessagesRequest),
    VertexAnthropic(AnthropicMessagesRequest),
    OpenAiCompatible(OpenAiCompatibleRequest),
    BedrockConverse(BedrockConverseRequest),
    /// IBM native chat uses the same message vocabulary with distinct routing.
    WatsonxChat(OpenAiCompatibleRequest),
}

/// Which protocol family a request belongs to, for diagnostics and routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolFamily {
    GoogleGemini,
    CohereChat,
    VertexGemini,
    OpenAiResponses,
    ChatGptCodex,
    AnthropicMessages,
    VertexAnthropic,
    OpenAiCompatible,
    BedrockConverse,
    WatsonxChat,
}

impl ProtocolFamily {
    pub fn is_vertex(self) -> bool {
        matches!(self, Self::VertexGemini | Self::VertexAnthropic)
    }

    pub fn is_anthropic(self) -> bool {
        matches!(self, Self::AnthropicMessages | Self::VertexAnthropic)
    }
}

/// A compatible API's fixed request format, selected from its catalogue identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatibleProtocol {
    OpenAi,
    Groq,
    CompletionTokens,
}

impl ProviderRequest {
    pub fn family(&self) -> ProtocolFamily {
        match self {
            Self::GoogleGemini(_) => ProtocolFamily::GoogleGemini,
            Self::CohereChat(_) => ProtocolFamily::CohereChat,
            Self::VertexGemini(_) => ProtocolFamily::VertexGemini,
            Self::OpenAiResponses(_) => ProtocolFamily::OpenAiResponses,
            Self::ChatGptCodex(_) => ProtocolFamily::ChatGptCodex,
            Self::AnthropicMessages(_) => ProtocolFamily::AnthropicMessages,
            Self::VertexAnthropic(_) => ProtocolFamily::VertexAnthropic,
            Self::OpenAiCompatible(_) => ProtocolFamily::OpenAiCompatible,
            Self::BedrockConverse(_) => ProtocolFamily::BedrockConverse,
            Self::WatsonxChat(_) => ProtocolFamily::WatsonxChat,
        }
    }

    pub fn model(&self) -> &ModelRef {
        match self {
            Self::CohereChat(request) => &request.model,
            Self::OpenAiResponses(request) => &request.model,
            Self::ChatGptCodex(request) => &request.model,
            Self::AnthropicMessages(request) | Self::VertexAnthropic(request) => &request.model,
            Self::OpenAiCompatible(request) | Self::WatsonxChat(request) => &request.model,
            Self::BedrockConverse(request) => &request.model,
            Self::GoogleGemini(request) | Self::VertexGemini(request) => &request.model,
        }
    }

    /// Native hosted spec, separate from callable function definitions.
    pub fn web_search(&self) -> Option<&crate::WebSearchSpec> {
        match self {
            Self::OpenAiResponses(request) => request.web_search.as_ref(),
            Self::ChatGptCodex(request) => request.web_search.as_ref(),
            Self::AnthropicMessages(_)
            | Self::OpenAiCompatible(_)
            | Self::BedrockConverse(_)
            | Self::GoogleGemini(_)
            | Self::CohereChat(_)
            | Self::VertexAnthropic(_)
            | Self::WatsonxChat(_)
            | Self::VertexGemini(_) => None,
        }
    }

    /// Anthropic's live-only search contract, not the Responses cached/live spec.
    pub fn anthropic_web_search(&self) -> Option<&crate::AnthropicWebSearchSpec> {
        match self {
            Self::AnthropicMessages(request) | Self::VertexAnthropic(request) => {
                request.web_search.as_ref()
            }
            _ => None,
        }
    }

    pub fn tools(&self) -> &[ToolDefinition] {
        match self {
            Self::CohereChat(request) => &request.tools,
            Self::OpenAiResponses(request) => &request.tools,
            Self::ChatGptCodex(request) => &request.tools,
            Self::AnthropicMessages(request) | Self::VertexAnthropic(request) => &request.tools,
            Self::OpenAiCompatible(request) | Self::WatsonxChat(request) => &request.tools,
            Self::BedrockConverse(request) => &request.tools,
            Self::GoogleGemini(request) | Self::VertexGemini(request) => &request.tools,
        }
    }

    pub fn tool_choice(&self) -> Option<&ToolChoice> {
        match self {
            Self::CohereChat(request) => request.tool_choice.as_ref(),
            Self::OpenAiResponses(request) => request.tool_choice.as_ref(),
            Self::ChatGptCodex(request) => request.tool_choice.as_ref(),
            Self::AnthropicMessages(request) | Self::VertexAnthropic(request) => {
                request.tool_choice.as_ref()
            }
            Self::OpenAiCompatible(request) | Self::WatsonxChat(request) => {
                request.tool_choice.as_ref()
            }
            Self::BedrockConverse(request) => request.tool_choice.as_ref(),
            Self::GoogleGemini(request) | Self::VertexGemini(request) => {
                request.tool_choice.as_ref()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::{
        BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
        ProviderId,
    };

    fn model(provider: &str, model: &str) -> ModelRef {
        // Mechanical migration, non-authorizing: the catalog is the authority
        // on base model and family, so a test target comes from a one-row
        // release rather than from coordinates asserted here.
        match (
            ProviderId::new(provider),
            ModelId::new(model),
            BaseModelId::new(model),
            ModelFamilyId::new(provider),
            ModelCatalogVersion::new("test-catalog"),
        ) {
            (Some(provider), Some(model), Some(base_model), Some(family), Some(version)) => {
                let record = ModelCatalogRecord::new(
                    provider.clone(),
                    model.clone(),
                    base_model,
                    family,
                    None,
                );
                let catalog = match ModelCatalog::load(version, vec![record]) {
                    Some(catalog) => catalog,
                    None => panic!("test catalog"),
                };
                match catalog.entry(&provider, &model, None) {
                    Some(entry) => ModelRef::from_catalog(entry),
                    None => panic!("catalog entry"),
                }
            }
            _ => panic!("test model coordinates must be non-empty"),
        }
    }

    fn hello() -> Vec<InputMessage> {
        vec![InputMessage {
            role: MessageRole::User,
            content: "Hello".into(),
        }]
    }

    #[test]
    fn codex_request_is_constructible_without_a_token_bound() {
        // The whole point: the Codex route carries no max_output_tokens, and
        // building a valid request does not require working around its
        // absence. Probe 04 proves the field cannot be added.
        let request = ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
            model: model("openai", "gpt-5-codex"),
            input: hello().into_iter().map(Into::into).collect(),
            prompt_cache_key: None,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
            reasoning: Default::default(),
            turn: Default::default(),
        });
        assert_eq!(request.family(), ProtocolFamily::ChatGptCodex);
        assert_eq!(request.model().provider().as_str(), "openai");
        assert!(request.tools().is_empty());
    }

    #[test]
    fn openai_responses_route_does_carry_the_bound() {
        // The field is not banned globally — it is banned on the route that
        // rejects it. A blanket removal would be a different (wrong) fix.
        let bound = MaxOutputTokens::new(4096);
        assert!(bound.is_some());
        let request = OpenAiResponsesRequest {
            model: model("openai", "gpt-5"),
            input: hello().into_iter().map(Into::into).collect(),
            prompt_cache_key: None,
            max_output_tokens: bound,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
        };
        assert_eq!(
            request.max_output_tokens.map(MaxOutputTokens::value),
            Some(4096)
        );
    }

    #[test]
    fn zero_output_tokens_is_unrepresentable() {
        assert_eq!(MaxOutputTokens::new(0), None);
        assert_eq!(MaxOutputTokens::new(1).map(MaxOutputTokens::value), Some(1));
    }

    #[test]
    fn project_cache_keys_are_shared_per_project_and_model_and_keep_the_session_header(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let key = PromptCacheKey::for_project("prj_a", "openai", "gpt-6-luna");
        assert_eq!(key.as_str().len(), MAX_PROMPT_CACHE_KEY_BYTES);
        assert!(key.as_str().bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            key,
            PromptCacheKey::for_project("prj_a", "openai", "gpt-6-luna")
        );
        for other in [
            PromptCacheKey::for_project("prj_b", "openai", "gpt-6-luna"),
            PromptCacheKey::for_project("prj_a", "anthropic", "gpt-6-luna"),
            PromptCacheKey::for_project("prj_a", "openai", "gpt-6-terra"),
        ] {
            assert_ne!(key.as_str(), other.as_str());
        }
        // The header follows the key unless a Session value is attached.
        assert_eq!(key.session_header(), key.as_str());
        let session = PromptCacheKey::for_session(&"ses_header".parse()?)?;
        let split = key.clone().with_session_header(&session);
        assert_eq!(split.as_str(), key.as_str());
        assert_eq!(split.session_header(), session.as_str());
        Ok(())
    }

    #[test]
    fn prompt_cache_keys_are_session_stable_distinct_bounded_and_redacted(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let session: SessionId = "ses_cache_a".parse()?;
        let other: SessionId = "ses_cache_b".parse()?;
        let key = PromptCacheKey::for_session(&session)?;
        let same = PromptCacheKey::for_session(&session)?;
        let other = PromptCacheKey::for_session(&other)?;

        assert_eq!(key, same);
        assert_ne!(key, other);
        assert_eq!(key.as_str(), "ses_cache_a");
        assert_eq!(format!("{key:?}"), "PromptCacheKey([REDACTED])");
        Ok(())
    }

    #[test]
    fn oracle_sha_session_keys_shed_the_prefix_and_first_over_bound_refuses(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let suffix = "a".repeat(MAX_PROMPT_CACHE_KEY_BYTES);
        let oracle: SessionId = format!("ses_{suffix}").parse()?;
        assert_eq!(PromptCacheKey::for_session(&oracle)?.as_str(), suffix);

        let imported: SessionId =
            format!("ses_{}", "z".repeat(MAX_PROMPT_CACHE_KEY_BYTES)).parse()?;
        assert_eq!(
            PromptCacheKey::for_session(&imported),
            Err(PromptCacheKeyError {
                bytes: MAX_PROMPT_CACHE_KEY_BYTES + "ses_".len()
            })
        );
        assert_eq!(
            PromptCacheKey::try_new(String::new()),
            Err(PromptCacheKeyError { bytes: 0 })
        );
        Ok(())
    }

    #[test]
    fn codex_tool_text_refuses_the_first_byte_over_its_bound() {
        assert!(CodexToolText::try_new("x".repeat(MAX_CODEX_TOOL_TEXT_BYTES)).is_ok());
        assert_eq!(
            CodexToolText::try_new("x".repeat(MAX_CODEX_TOOL_TEXT_BYTES + 1)),
            Err(CodexToolTextTooLarge {
                bytes: MAX_CODEX_TOOL_TEXT_BYTES + 1
            })
        );
    }

    #[test]
    fn anthropic_requires_a_token_bound_and_owns_thinking() {
        let max_tokens = match MaxOutputTokens::new(8192) {
            Some(value) => value,
            None => panic!("8192 is a valid bound"),
        };
        let request = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
            web_search: None,
            uncached_system_suffix: None,
            effort: crate::EffortPolicy::default(),
            model: model("anthropic", "claude-opus-5"),
            messages: hello().into_iter().map(Into::into).collect(),
            // Not an Option — the Messages API rejects a request without it.
            max_tokens,
            instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
            thinking: Some(ThinkingConfig::Enabled {
                budget_tokens: 2048,
            }),
            prompt_cache: AnthropicPromptCache::Off,
            tools: Vec::new(),
            tool_choice: Some(ToolChoice::Auto),
        });
        assert_eq!(request.family(), ProtocolFamily::AnthropicMessages);
    }

    #[test]
    fn every_family_is_reachable_and_distinct() {
        let families = [
            ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
                model: model("openai", "gpt-5"),
                input: hello().into_iter().map(Into::into).collect(),
                prompt_cache_key: None,
                max_output_tokens: None,
                tools: Vec::new(),
                web_search: None,
                tool_choice: None,
            })
            .family(),
            ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
                model: model("openai", "gpt-5-codex"),
                input: hello().into_iter().map(Into::into).collect(),
                prompt_cache_key: None,
                tools: Vec::new(),
                web_search: None,
                tool_choice: None,
                reasoning: Default::default(),
                turn: Default::default(),
            })
            .family(),
        ];
        assert_ne!(
            families[0], families[1],
            "the two OpenAI-shaped routes must not collapse into one family"
        );
    }
}

/// Each concrete request names the model it will be sent for and the tools it
/// offers, so [`crate::authority::ProviderAttemptCarrier`] can refuse one whose
/// model is not the issuance the registry minted, or whose tools the effective
/// graph does not make visible.
///
/// The fields stay public: a caller may build any request it likes, and the
/// carrier is what decides whether it may travel.
macro_rules! targeted_request {
    ($request:ty $(, $native:ident)?) => {
        impl crate::authority::TargetedRequest for $request {
            fn model(&self) -> &ModelRef {
                &self.model
            }

            fn offered_tools(&self) -> Vec<String> {
                let names = self.tools.iter().map(|tool| tool.name.clone());
                $(let names = self.$native.as_ref().map(|_| "web_search".to_owned()).into_iter().chain(names);)?
                names.collect()
            }

            fn tool_choice(&self) -> crate::authority::RequestToolChoice {
                match &self.tool_choice {
                    // `None` and `Auto` both leave the provider free; neither
                    // constrains which tool is called, so neither is a check the
                    // carrier can make.
                    None | Some(ToolChoice::Auto) | Some(ToolChoice::None) => {
                        crate::authority::RequestToolChoice::Unconstrained
                    }
                    Some(ToolChoice::Required) => crate::authority::RequestToolChoice::RequiresSome,
                    Some(ToolChoice::Named { tool }) => {
                        crate::authority::RequestToolChoice::RequiresNamed(tool.clone())
                    }
                }
            }
        }
    };
}

targeted_request!(OpenAiResponsesRequest, web_search);
targeted_request!(ChatGptCodexRequest, web_search);
targeted_request!(AnthropicMessagesRequest, web_search);
targeted_request!(OpenAiCompatibleRequest);
targeted_request!(BedrockConverseRequest);
targeted_request!(GoogleGeminiRequest);
targeted_request!(CohereChatRequest);

/// The dispatch itself is targetable, by delegating to whichever concrete
/// request it carries.
///
/// This is what lets the carrier bind ONE `ProviderRequest` rather than needing
/// a separate generic instantiation per family — and therefore what lets every
/// family travel the same sealed issue/bind/decompose sequence rather than
/// three copies of it.
impl crate::authority::TargetedRequest for ProviderRequest {
    fn model(&self) -> &ModelRef {
        self.model()
    }

    fn offered_tools(&self) -> Vec<String> {
        match self {
            Self::OpenAiResponses(request) => request.offered_tools(),
            Self::ChatGptCodex(request) => request.offered_tools(),
            Self::AnthropicMessages(request) | Self::VertexAnthropic(request) => {
                request.offered_tools()
            }
            Self::OpenAiCompatible(request) | Self::WatsonxChat(request) => request.offered_tools(),
            Self::BedrockConverse(request) => request.offered_tools(),
            Self::GoogleGemini(request) | Self::VertexGemini(request) => request.offered_tools(),
            Self::CohereChat(request) => request.offered_tools(),
        }
    }

    fn tool_choice(&self) -> crate::authority::RequestToolChoice {
        match self.tool_choice() {
            None | Some(ToolChoice::Auto) | Some(ToolChoice::None) => {
                crate::authority::RequestToolChoice::Unconstrained
            }
            Some(ToolChoice::Required) => crate::authority::RequestToolChoice::RequiresSome,
            Some(ToolChoice::Named { tool }) => {
                crate::authority::RequestToolChoice::RequiresNamed(tool.clone())
            }
        }
    }
}
