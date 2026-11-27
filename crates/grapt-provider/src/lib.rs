//! Normalized provider-stream vocabulary for one physical attempt.
//!
//! Law: `docs/reports/SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! in the `grapt` repository, binding for this crate.
//! Every deviation from it is a **disclosed** behaviour
//! change, marked `DISCLOSED` at the point of departure — never silent.
//!
//! # Scope
//!
//! This crate owns three things:
//!
//! 1. [`fragment::AttemptFragment`] — the closed alphabet adapters emit after
//!    normalizing provider bytes, plus its supporting vocabularies (finish
//!    reasons, failure classes, usage, tool execution owner).
//! 2. [`normalize::StreamValidator`] — the ordering rules that decide whether
//!    a fragment is legal at this point in the stream, and whether the attempt
//!    has yet produced visible output.
//! 3. `transport` and `routes` — the one governed transport and the four
//!    route families it serves: OpenAI Responses, ChatGPT/Codex, Anthropic
//!    Messages, and OpenAI-compatible chat completions (Grok). The transport
//!    owns what is transport (TLS, SSE framing, block ordinals, status
//!    classification, the capacity registry, the single send); each route owns
//!    only its own vendor vocabulary — request body, event meaning, finish
//!    mapping, usage reading.
//!
//!    Both modules are crate-private, so they are named here in prose rather
//!    than as doc links: a public link to a private module resolves to
//!    nothing a reader of these docs can open. Their public surface is the
//!    root re-exports [`ProviderAdapter`], [`ProviderOpen`] and
//!    [`ProviderStream`].
//!
//!    CORRECTION, recorded rather than silently rewritten: this list previously
//!    named `codex` as "the one governed transport for that route" and cited
//!    the master's crate-graph amendment, "no adapter may exceed one route
//!    family". The amendment is unchanged and still holds — each ROUTE decoder
//!    here serves exactly one family, and the Responses decoder's two families
//!    are one protocol reached at two endpoints. The transport beneath them is
//!    shared rather than copied per route.
//!
//! # Scope boundaries (deliberate non-ownership)
//!
//! - **No transport outside an adapter.** The alphabet and the validator open
//!   no socket and parse no vendor JSON; only [`codex`] does, and only for its
//!   one route.
//! - **No durable state.** Nothing here writes SQLite or emits events. The
//!   serialized session owner does that, synchronously, after this crate says
//!   the fragment is legal.
//! - **No channel.** The bounded `mpsc` handoff and its backpressure
//!   diagnostics belong to the attempt supervisor, not to the alphabet.
//! - **No retry bound.** The five-attempt cap is defined and enforced by
//!   `grapt_runtime::step::MAX_TOTAL_ATTEMPTS`, where `StepPolicy::new`
//!   refuses a larger value. This crate supplies two inputs to that policy and
//!   owns neither the bound nor the decision: class-level retry eligibility
//!   ([`fragment::ProviderFailureClass::is_retryable`]) and whether visible
//!   output has occurred ([`normalize::StreamValidator::visible_output`]).
//!
//!   CORRECTION, recorded rather than silently rewritten: this paragraph
//!   previously read "The five-attempt cap is folded from durable events by
//!   `grapt-execution`, which stays the single owner of that law." That was
//!   false in two ways — `grapt-execution` observes `max_retry_attempt` and
//!   compares it to nothing, and the cap lives in `grapt-runtime`. The same
//!   claim also appeared in `grapt-runtime`'s own crate doc and in the
//!   since-removed `grapt-execution`; the retry bound remains the runtime's
//!   responsibility.
//!
//! # No panics
//!
//! The oracle carries malformed provider fragments as `Effect.die` crash-class
//! defects at 30 sites, bypassing `Step.Failed`. The corrected expectation is
//! one typed failure. Accordingly this crate has
//! no panicking path outside of `#[cfg(test)]`: every rejection is a
//! [`normalize::StreamViolation`] carrying a typed
//! [`normalize::AttemptFailure`].

mod anthropic_model;
pub mod authority;
pub mod codex;
pub mod credential;
#[cfg(any(test, feature = "deterministic-testkit"))]
pub mod deterministic;
pub mod disposition;
pub mod fragment;
mod gemini;
mod image_input;
mod watsonx;
pub use gemini::GoogleGeminiRequest;
pub use image_input::{
    ImageDetail, ImageMedia, InputImage, InputImages, MAX_IMAGE_ENCODED_BYTES, MAX_REQUEST_IMAGES,
};
pub mod normalize;
pub mod opener;
pub use anthropic_model::supports_anthropic_system_updates;

mod replay;
pub mod request;
mod request_encoding;
pub mod tool_eligibility;
#[cfg(any(test, feature = "loopback-testkit"))]
pub use replay::compaction_fixture_state;
pub use replay::{
    drop_foreign_replays, CompactionTarget, ProviderReplay, ReplayError, COMPACTION_MALFORMED,
    COMPACTION_OVERSIZE, MAX_COMPACTION_ITEM_BYTES,
};
pub use request_encoding::RequestEncoding;
pub mod route;
pub(crate) mod routes;
pub(crate) mod transport;
pub mod usage;

pub use authority::{AttemptCustody, CarrierRefusal};
pub use codex::{CodexConfigError, CodexDeadlines, CodexError, CodexLimits};
pub use credential::{
    messages_placement, CredentialError, CredentialLease, CredentialOwner, CredentialPlacement,
    CredentialRenewalFuture, CredentialRenewalRefusal, CredentialRevision, CredentialUseAuthority,
    CurrentCredential, MessagesAuthMode, ProviderCredential, RequestAuthenticationFuture,
    RequestAuthenticator,
};
#[cfg(any(test, feature = "deterministic-testkit"))]
pub use deterministic::{AdapterError, AttemptStream, DeterministicAdapter, Script, ScriptBuilder};
pub use disposition::{AttemptDisposition, NoVisibleOutput};
pub use fragment::{
    AdapterState, AdapterStateTooLarge, AttemptFragment, BlockOrdinal, BoundedDetail,
    FailureClassification, FinishReason, NormalizedFinish, ProviderFailureClass, ToolExecution,
};
pub use normalize::{
    Accepted, AttemptFailure, BlockKind, IgnoredEvidence, StragglerKind, StreamValidator,
    StreamViolation,
};
pub use opener::{
    AttemptCapacity, AttemptOpenRefusal, AttemptOpener, AttemptReader, OpenedAttempt,
    OpenerCredential, MAX_PHYSICAL_ATTEMPT_ID_BYTES,
};
pub use request::{
    prefix_digest, AnthropicInstructionMode, AnthropicMessagesRequest, AnthropicPromptCache,
    BedrockConverseRequest, ChatGptCodexRequest, CodexInputItem, CodexReasoning, CodexToolText,
    CodexToolTextTooLarge, CodexTurn, CompatiblePromptCache, CompatibleProtocol,
    CompatibleReasoningReplay, InputMessage, MaxOutputTokens, MessageRole, OpenAiCompatibleRequest,
    OpenAiResponsesRequest, PromptCacheKey, PromptCacheKeyError, ProtocolFamily, ProviderRequest,
    ThinkingConfig, ToolChoice, ToolDefinition, ToolInputFormat, MAX_CODEX_TOOL_TEXT_BYTES,
    MAX_PROMPT_CACHE_KEY_BYTES,
};
mod anthropic_web_search;
mod citations;
mod native_search;
mod web_search;
pub use anthropic_web_search::{AnthropicWebSearchSpec, AnthropicWebSearchUsesError};
/// Re-exported for the runtime consumer of [`opener::AttemptOpener::open`],
/// whose signature names it. `grapt-runtime` deliberately has no direct
/// `grapt-policy` dependency edge; the opener's input type travels with the
/// opener.
pub use grapt_policy::{PhysicalAttemptAuthoritySnapshot, ToolName, ToolVisibility};
#[cfg(any(test, feature = "loopback-testkit"))]
pub use route::LoopbackBase;
pub use route::{
    credential_can_authenticate, credential_placement, family_default_endpoint, select_model,
    EndpointConfig, EndpointSource, ProviderEndpoint, ResolvedEndpoint, RouteConfigError,
};
pub use routes::request_fingerprint::{RequestDelta, RequestFingerprint};
pub use transport::adapter::{placement_for, AttemptInput, ProviderAdapter, ProviderOpen};
/// The public capture seam for raw-byte storage consumers.
///
/// Re-exported at the crate root so an external consumer can name the types
/// without depending on the private transport module.
pub use transport::capture::{
    capture_exchange, AttemptCapture, CaptureEvent, CaptureExchange, CaptureLoss, CaptureOutcome,
};
pub use transport::dispatch::{
    DispatchObserver, DispatchRefusal, DispatchSample, DispatchSamples, VendorCode,
    DISPATCH_SAMPLE_CAPACITY, DISPATCH_USAGE_CAPACITY,
};
pub use transport::error_text::{
    MaskedErrorText, ProviderErrorDetail, ProviderErrorText, ERROR_TEXT_BYTES,
};
pub use transport::stream::ProviderStream;
pub use transport::terminal_detail::{MaskedTerminalFrame, TERMINAL_DETAIL_BYTES};
pub use usage::{
    TokenUsage, UsageNormalizationError, UsageNormalizationReason, UsageRoute, UsageWireField,
};
pub use web_search::{
    DomainFilter, WebSearchAccess, WebSearchContext, WebSearchDomainsError, WebSearchSpec,
};

mod effort;
pub use effort::{supports_anthropic_effort_updates, EffortMode, EffortPolicy, ReasoningEffort};

mod cohere;
pub use cohere::CohereChatRequest;
mod vertex_anthropic;
