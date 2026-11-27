//! The one provider adapter: stage an attempt for any route family, send it
//! once, and hand back a stream.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`. Contract
//! rule 1 — "One PhysicalAttempt performs exactly one provider `stream(request)`
//! call" — is enforced here for every family at once, by one capacity registry.
//!
//! # What differs per family, and what does not
//!
//! Differs: the URL path, the headers, the request body, and the decoder. All
//! four are settled by [`crate::request::ProtocolFamily`] before a socket
//! exists, and all four live in [`crate::routes`].
//!
//! Does not differ: TLS construction, the two injected deadlines, redirect and
//! proxy refusal, account-scoped connection reuse, the capacity slot and its `Drop`
//! release, the zero-byte staging window, and the single point at which bytes
//! leave. Those properties have one shared implementation to check.
//!
//! # Zero bytes before the first poll
//!
//! [`ProviderAdapter::stage`] is synchronous and touches no socket. A runtime
//! that must make the attempt durable BEFORE the provider opens therefore has a
//! guaranteed window, and [`StagedAttempt::send`] is the single line in this
//! crate at which bytes leave the process for an attempt.

use std::time::Duration;

use crate::codex::{CodexDeadlines, CodexError};
use crate::credential::{CredentialLease, CredentialPlacement};
use crate::fragment::{AttemptFragment, ProviderFailureClass};
use crate::request::{ProtocolFamily, ProviderRequest};
use crate::route::ProviderEndpoint;
use crate::transport::capture::CaptureExchange;
use crate::transport::classify::{classify_error, failure, parse_retry_after};
use crate::transport::client_pool::{ClientFactory, ScopedClients};
use crate::transport::slot::{ActiveAttempts, AttemptSlot};
use crate::transport::stream::{ProviderStream, RouteDecoder};

/// The Anthropic API version header value.
///
/// Required by the Messages API on every request; a request without it is
/// refused by the provider. Pinned as a constant rather than made configurable
/// because it selects the wire shape this crate's decoder was written against —
/// a caller who could change it could silently pair a new wire shape with an
/// old decoder.
///
/// Provenance: the oracle's `anthropic.ts` in the frozen
/// oracle at `42e4888cc0`.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The `originator` header the oracle sends on the ChatGPT/Codex route
/// (`openai-codex.ts`). Held at its pre-rename value until the issuer and
/// backend are verified to accept a new originator.
const ORIGINATOR: &str = "secode"; // grapt-rename: keep

/// A session-routed gateway: its compatible chat route routes and caches a
/// conversation by its Session and refuses a request that names none.
const SESSION_ROUTED_HOST: &str = "opencode.ai";
const SESSION_ROUTING_HEADER: &str = "x-opencode-session";

/// The Session to send as [`SESSION_ROUTING_HEADER`]: only on a compatible
/// request to the session-routed gateway, so no other provider learns it.
fn routing_session(input: &AttemptInput) -> Option<&str> {
    let ProviderRequest::OpenAiCompatible(request) = &input.request else {
        return None;
    };
    let session = request.routing_session.as_ref()?;
    session_routed(input.endpoint.base()).then(|| session.session_header())
}

/// Same host rule as the key check's in `grapt-login`; neither crate depends
/// on the other, so a new gateway host changes both.
fn session_routed(base: &str) -> bool {
    reqwest::Url::parse(base).ok().is_some_and(|url| {
        url.host_str().is_some_and(|host| {
            host.strip_suffix(SESSION_ROUTED_HOST)
                .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with('.'))
        })
    })
}

/// Everything one attempt needs, for any family.
#[allow(
    missing_debug_implementations,
    reason = "it owns a CredentialLease; a derived formatter would print the request and reach the lease"
)]
pub struct AttemptInput {
    pub(crate) attempt: grapt_types::PhysicalAttemptId,
    pub(crate) endpoint: ProviderEndpoint,
    pub(crate) lease: CredentialLease,
    pub(crate) request: ProviderRequest,
    pub(crate) user_agent: String,
}

impl AttemptInput {
    pub fn attempt(&self) -> &grapt_types::PhysicalAttemptId {
        &self.attempt
    }
}

/// What opening one attempt produced.
#[derive(Debug)]
pub enum ProviderOpen {
    /// A live stream. The caller drives it to a terminal fragment.
    ///
    /// Boxed: a live stream owns the response, the SSE buffer and the route
    /// decoder, so an unboxed variant would make every pre-stream FAILURE carry
    /// a streaming-sized value.
    Streaming(Box<ProviderStream>),
    /// The attempt failed before any fragment could be produced. The payload is
    /// always an [`AttemptFragment::ProviderFailure`], so a pre-stream failure
    /// travels through the same vocabulary — and therefore the same retry
    /// classification — as a mid-stream one.
    Failed(Box<AttemptFragment>),
    /// A volatile local diagnostic beside a failure, never inside its fragment.
    FailedWithDetail {
        fragment: Box<AttemptFragment>,
        detail: crate::ProviderErrorDetail,
    },
}

/// The provider adapter. One lazy transport pool per account and route origin;
/// credentials are checked and placed independently for every attempt.
pub struct ProviderAdapter {
    clients: ScopedClients,
    compatible_protocol: crate::CompatibleProtocol,
    deadlines: CodexDeadlines,
    active: ActiveAttempts,
    children: ActiveAttempts,
    /// The deployment's capture exchange, if it configured one. Each attempt
    /// announces itself on it and receives its own bounded feed, so capture is
    /// attributable per attempt rather than interleaved across them.
    capture: Option<CaptureExchange>,
    dispatch: Option<crate::DispatchObserver>,
    error_text: Option<crate::ProviderErrorText>,
    credential_owner: Option<std::sync::Arc<dyn crate::CredentialOwner>>,
}

impl std::fmt::Debug for ProviderAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderAdapter")
            .field("deadlines", &self.deadlines)
            .finish_non_exhaustive()
    }
}

impl ProviderAdapter {
    pub(crate) fn new(factory: ClientFactory, deadlines: CodexDeadlines, capacity: usize) -> Self {
        let children = ActiveAttempts::new(capacity);
        let mut active = ActiveAttempts::new(capacity);
        active.share_account(&children);
        Self {
            clients: ScopedClients::new(factory, deadlines),
            compatible_protocol: crate::CompatibleProtocol::OpenAi,
            deadlines,
            active,
            children,
            capture: None,
            dispatch: None,
            error_text: None,
            credential_owner: None,
        }
    }

    pub(crate) fn set_compatible_protocol(&mut self, protocol: crate::CompatibleProtocol) {
        self.compatible_protocol = protocol;
    }

    pub(crate) fn with_child_capacity(mut self, capacity: std::num::NonZeroUsize) -> Self {
        self.children = ActiveAttempts::new(capacity.get());
        self.active.share_account(&self.children);
        self
    }

    pub(crate) fn reserve_child<'a>(
        &self,
        limit: std::num::NonZeroUsize,
        observer: Option<Box<dyn FnMut(usize, bool) + Send + 'a>>,
    ) -> super::slot::Reserve<'a> {
        self.children.reserve_limited(limit.get(), observer)
    }

    /// Attach the deployment's capture exchange.
    ///
    /// Injected, never constructed here: raw-byte storage belongs to the
    /// capture consumer.
    /// Capture cannot change what the attempt does and cannot stall it — see
    /// [`crate::CaptureExchange`], which is a bounded non-blocking channel
    /// rather than a trait this crate calls into.
    pub fn with_capture(mut self, capture: CaptureExchange) -> Self {
        self.capture = Some(capture);
        self
    }

    pub(crate) fn with_dispatch(mut self, observer: crate::DispatchObserver) -> Self {
        self.dispatch = Some(observer);
        self
    }

    pub(crate) fn with_error_text(mut self, policy: crate::ProviderErrorText) -> Self {
        self.error_text = Some(policy);
        self
    }

    pub(crate) fn with_credential_owner(
        mut self,
        owner: std::sync::Arc<dyn crate::CredentialOwner>,
    ) -> Self {
        self.credential_owner = Some(owner);
        self
    }

    /// Open the one provider stream for one attempt.
    ///
    /// `stage` then `send` — same work, same order, split so a caller that must
    /// make the attempt durable before bytes leave can hold the staged half.
    pub async fn open(&self, input: AttemptInput) -> Result<ProviderOpen, CodexError> {
        Ok(self.stage(input)?.send().await)
    }

    /// How many attempts are streaming right now. Test-only: production has no
    /// business reading a live capacity count, and exposing one would invite a
    /// caller to race it.
    #[cfg(test)]
    pub(crate) fn active_len(&self) -> usize {
        self.active.len()
    }

    /// Every fallible pre-byte step: claim the capacity slot, serialize the
    /// body, assemble the request. Nothing here touches a socket.
    ///
    /// The slot claimed here is released by the private `AttemptSlot` guard's
    /// `Drop`: if the staged attempt is dropped unsent, or the stream it becomes
    /// terminates or is dropped, the slot frees itself. There is no release call
    /// a caller can forget, which is what makes cancellation safe.
    pub(crate) fn stage(&self, input: AttemptInput) -> Result<StagedAttempt, CodexError> {
        self.stage_reserved(input, None)
    }

    /// Wait for a free slot on this adapter and hold it for one attempt that
    /// [`Self::stage_reserved`] then stages.
    pub(crate) fn reserve<'a>(
        &self,
        observer: Option<Box<dyn FnMut(usize, bool) + Send + 'a>>,
    ) -> super::slot::Reserve<'a> {
        self.active.reserve_observed(observer)
    }

    /// [`Self::stage`], with the attempt's slot already reserved.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(crate) fn stage_reserved(
        &self,
        mut input: AttemptInput,
        reservation: Option<super::slot::SlotReservation>,
    ) -> Result<StagedAttempt, CodexError> {
        crate::replay::drop_unreplayable_reasoning(&mut input);
        crate::replay::refuse_unbound_compaction(&input).map_err(|reason| {
            CodexError::RequestBuild {
                reason: reason.to_owned(),
            }
        })?;
        crate::routes::request_validation::validate(&input.request).map_err(|reason| {
            CodexError::RequestBuild {
                reason: reason.to_owned(),
            }
        })?;
        let registry = if reservation
            .as_ref()
            .is_some_and(|held| held.belongs_to(&self.children))
        {
            &self.children
        } else {
            &self.active
        };
        let slot = registry
            .claim(&input.attempt, reservation)?
            .with_feedback(self.children.clone());
        let family = input.request.family();

        // The endpoint's family and the request's family must agree. They are
        // set independently — the endpoint by configuration, the request by the
        // runtime — so a disagreement means an Anthropic body is about to be
        // posted to an OpenAI path, and it is refused before a socket exists
        // rather than diagnosed later from a 404.
        if input.lease.cloudflare() != input.endpoint.cloudflare() {
            return Err(CodexError::RequestBuild {
                reason: "credential gateway scope differs from the endpoint; reconnect Cloudflare and pick its model".to_owned(),
            });
        }
        if input.endpoint.family() != family {
            return Err(CodexError::RequestBuild {
                reason: "the endpoint's protocol family does not match the request's".to_owned(),
            });
        }

        if input.lease.messages_auth() != input.endpoint.messages_auth()
            || !crate::credential_can_authenticate(
                family,
                input.endpoint.cloudflare(),
                input.endpoint.messages_auth(),
                input.lease.placement(),
            )
        {
            return Err(CodexError::RequestBuild {
                reason: "Messages authentication differs from its captured credential; reconnect and pick its model".into(),
            });
        }
        let projected = request_body(&input.request, &input.endpoint, self.compatible_protocol)
            .map_err(|reason| CodexError::RequestBuild {
                reason: reason.to_owned(),
            })?;
        let body = match serde_json::to_string(&projected) {
            Ok(body) => body,
            Err(error) => {
                return Err(CodexError::RequestBuild {
                    reason: error.to_string(),
                })
            }
        };

        // Snapshot sticky routing once: diagnostics and the actual HTTP request
        // must describe the same header, even if a response captures a token.
        let (session_header, turn_state) = match &input.request {
            ProviderRequest::ChatGptCodex(codex) => (
                codex
                    .prompt_cache_key
                    .as_ref()
                    .map(|key| key.session_header()),
                codex.turn.header_for(input.lease.account()),
            ),
            _ => (None, None),
        };
        let target_url = request_url(&input).map_err(|reason| CodexError::RequestBuild {
            reason: reason.into(),
        })?;
        let request_fingerprint = (|| {
            crate::RequestFingerprint::of(
                &projected,
                &[
                    &format!("{family:?}"),
                    &target_url,
                    input.lease.account().as_str(),
                    input.lease.credential_version().as_str(),
                ],
                body.len(),
            )?
            .with_routing(session_header, turn_state.as_deref())
        })();
        let decoder = decoder_for(&input)?;
        let client = self.clients.for_attempt(&input.lease, &input.endpoint)?;
        let request = self
            .headers(&client, &input, family, turn_state.as_deref())?
            .body(body);

        let turn = match &input.request {
            ProviderRequest::ChatGptCodex(codex) => {
                Some((codex.turn.clone(), input.lease.account().clone()))
            }
            _ => None,
        };
        let error_lease = Some(input.lease);
        Ok(StagedAttempt {
            clients: self.clients.clone(),
            request_fingerprint,
            diagnostic_model: input.request.model().clone(),
            diagnostic_session: None,
            request,
            error_text: self.error_text.clone(),
            error_lease,
            credential_owner: self.credential_owner.clone(),
            dispatch: self
                .dispatch
                .as_ref()
                .map(|observer| (observer.clone(), input.attempt.clone())),
            response_head: self.deadlines.response_head(),
            decoder,
            slot,
            // Opened HERE, at staging, so the consumer is told the attempt
            // exists before any byte leaves — and so a declined capture costs
            // nothing on the read path.
            capture: self
                .capture
                .as_ref()
                .and_then(|exchange| exchange.open(&input.attempt)),
            turn,
        })
    }

    /// Assemble the family's headers.
    ///
    /// The credential places itself ([`CredentialLease::place`]); this function
    /// never sees the material. The placement is the capability's, so an
    /// Anthropic key cannot arrive as a bearer token because a caller picked
    /// the wrong adapter.
    fn headers(
        &self,
        client: &reqwest::Client,
        input: &AttemptInput,
        family: ProtocolFamily,
        turn_state: Option<&str>,
    ) -> Result<reqwest::RequestBuilder, CodexError> {
        let request = input
            .lease
            .place(client.post(
                request_url(input).map_err(|reason| CodexError::RequestBuild {
                    reason: reason.into(),
                })?,
            ))
            .map_err(|error| CodexError::RequestBuild {
                reason: error.to_string(),
            })?;
        let request = request
            .header("user-agent", input.user_agent.as_str())
            .header(
                "accept",
                if family == ProtocolFamily::BedrockConverse {
                    "application/vnd.amazon.eventstream"
                } else {
                    "text/event-stream"
                },
            )
            .header("content-type", "application/json");

        let request = match input.endpoint.cloudflare() {
            Some(scope) if scope.wire() == grapt_types::cloudflare::CloudflareWire::Chat => {
                request.header("cf-aig-gateway-id", scope.gateway().as_str())
            }
            _ => request,
        };
        Ok(match family {
            ProtocolFamily::ChatGptCodex => {
                let request = request
                    .header("originator", ORIGINATOR)
                    .header("version", grapt_types::CODEX_COMPATIBILITY_VERSION)
                    .header("chatgpt-account-id", input.lease.account().as_str());
                // Copyright 2025 OpenAI, Apache-2.0. Modified by Grapt:
                // keep our own originator and typed Session identity. Cache
                // affinity may share a bucket; thread identity never does.
                let request = match &input.request {
                    ProviderRequest::ChatGptCodex(codex) => {
                        let request = match &codex.prompt_cache_key {
                            Some(key) => request.header("session-id", key.session_header()),
                            None => request,
                        };
                        match codex.turn.thread_id() {
                            Some(thread) => request
                                .header("thread-id", thread)
                                .header("x-client-request-id", thread),
                            None => request,
                        }
                    }
                    _ => request,
                };
                // Within one execution, the turn's sticky-routing token goes
                // back on every later request (Codex's x-codex-turn-state).
                match turn_state {
                    Some(token) => request.header(TURN_STATE_HEADER, token),
                    None => request,
                }
            }
            ProtocolFamily::AnthropicMessages | ProtocolFamily::VertexAnthropic => {
                let request = request.header("anthropic-version", ANTHROPIC_VERSION);
                if matches!(&input.request, ProviderRequest::AnthropicMessages(native) | ProviderRequest::VertexAnthropic(native) if native.effort.updates())
                {
                    request.header(
                        "anthropic-beta",
                        "mid-conversation-output-config-2026-07-01",
                    )
                } else {
                    request
                }
            }
            ProtocolFamily::OpenAiCompatible => match routing_session(input) {
                Some(session) => request.header(SESSION_ROUTING_HEADER, session),
                None => request,
            },
            // The public Responses route and these families send no
            // vendor-specific extra header. Named rather than caught by a
            // wildcard so that adding a family is a compile error here.
            ProtocolFamily::OpenAiResponses
            | ProtocolFamily::BedrockConverse
            | ProtocolFamily::GoogleGemini
            | ProtocolFamily::CohereChat
            | ProtocolFamily::WatsonxChat
            | ProtocolFamily::VertexGemini => request,
        })
    }
}

/// The credential placement each family requires.
///
/// Exposed so an opener can check, before it accepts a credential, that the
/// capability it was given can actually authenticate the family it is being
/// configured for — rather than discovering it from a 401 after bytes left.
pub fn placement_for(family: ProtocolFamily) -> CredentialPlacement {
    match family {
        ProtocolFamily::OpenAiResponses
        | ProtocolFamily::ChatGptCodex
        | ProtocolFamily::OpenAiCompatible
        | ProtocolFamily::BedrockConverse
        | ProtocolFamily::CohereChat
        | ProtocolFamily::WatsonxChat => CredentialPlacement::BearerAuthorization,
        ProtocolFamily::AnthropicMessages => CredentialPlacement::AnthropicApiKey,
        ProtocolFamily::VertexAnthropic => CredentialPlacement::BearerAuthorization,
        ProtocolFamily::GoogleGemini => CredentialPlacement::GoogleApiKey,
        ProtocolFamily::VertexGemini => CredentialPlacement::BearerAuthorization,
    }
}

fn request_body(
    request: &ProviderRequest,
    endpoint: &ProviderEndpoint,
    compatible: crate::CompatibleProtocol,
) -> Result<serde_json::Value, &'static str> {
    crate::RequestEncoding::new(
        compatible,
        endpoint.cloudflare().cloned(),
        endpoint.watsonx_project().cloned(),
    )
    .body(request)
}

/// Whether the request asks for a provider compaction.
fn triggers_compaction(request: &ProviderRequest) -> bool {
    let input = match request {
        ProviderRequest::OpenAiResponses(request) => &request.input,
        ProviderRequest::ChatGptCodex(request) => &request.input,
        ProviderRequest::AnthropicMessages(_)
        | ProviderRequest::VertexAnthropic(_)
        | ProviderRequest::OpenAiCompatible(_)
        | ProviderRequest::BedrockConverse(_)
        | ProviderRequest::GoogleGemini(_)
        | ProviderRequest::CohereChat(_)
        | ProviderRequest::WatsonxChat(_)
        | ProviderRequest::VertexGemini(_) => return false,
    };
    input
        .iter()
        .any(|item| matches!(item, crate::CodexInputItem::CompactionTrigger))
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn decoder_for(input: &AttemptInput) -> Result<RouteDecoder, CodexError> {
    Ok(match input.request.family() {
        ProtocolFamily::BedrockConverse => {
            let binding = crate::replay::ReplayBinding::for_bedrock(input).map_err(|reason| {
                CodexError::RequestBuild {
                    reason: reason.into(),
                }
            })?;
            let ProviderRequest::BedrockConverse(request) = &input.request else {
                return Err(CodexError::RequestBuild {
                    reason: "replay protocol mismatch".into(),
                });
            };
            for item in &request.messages {
                if let crate::CodexInputItem::ProviderReplay(replay) = item {
                    if !replay.bound_to(&binding) {
                        return Err(CodexError::RequestBuild {
                            reason: "Bedrock replay routing or credential binding changed".into(),
                        });
                    }
                }
            }
            RouteDecoder::Bedrock(Box::new(
                crate::routes::bedrock::BedrockDecoder::with_binding(binding),
            ))
        }
        // One decoder for both, because they are one protocol. See
        // `routes::responses`.
        ProtocolFamily::OpenAiResponses => RouteDecoder::Responses(Box::new(
            crate::routes::responses::ResponsesDecoder::for_route(
                crate::usage::UsageRoute::OpenAiResponses,
            )
            .replaying(crate::replay::ReplayBinding::for_reasoning(input))
            .compacting(crate::replay::ReplayBinding::for_compaction(input))
            .triggered(triggers_compaction(&input.request))
            .searching(match &input.request {
                ProviderRequest::OpenAiResponses(request) if request.web_search.is_some() => {
                    crate::replay::ReplayBinding::for_search(input)
                }
                ProviderRequest::ChatGptCodex(request) if request.web_search.is_some() => {
                    crate::replay::ReplayBinding::for_search(input)
                }
                _ => None,
            }),
        )),
        ProtocolFamily::ChatGptCodex => RouteDecoder::Responses(Box::new(
            crate::routes::responses::ResponsesDecoder::for_route(
                crate::usage::UsageRoute::ChatGptCodex,
            )
            .replaying(crate::replay::ReplayBinding::for_reasoning(input))
            .compacting(crate::replay::ReplayBinding::for_compaction(input))
            .triggered(triggers_compaction(&input.request))
            .searching(match &input.request {
                ProviderRequest::OpenAiResponses(request) if request.web_search.is_some() => {
                    crate::replay::ReplayBinding::for_search(input)
                }
                ProviderRequest::ChatGptCodex(request) if request.web_search.is_some() => {
                    crate::replay::ReplayBinding::for_search(input)
                }
                _ => None,
            }),
        )),
        ProtocolFamily::AnthropicMessages | ProtocolFamily::VertexAnthropic => {
            let binding = crate::replay::ReplayBinding::from_input(input).map_err(|reason| {
                CodexError::RequestBuild {
                    reason: reason.into(),
                }
            })?;
            let (ProviderRequest::AnthropicMessages(request)
            | ProviderRequest::VertexAnthropic(request)) = &input.request
            else {
                return Err(CodexError::RequestBuild {
                    reason: "replay protocol mismatch".into(),
                });
            };
            for item in &request.messages {
                if let crate::CodexInputItem::ProviderReplay(replay) = item {
                    if !replay.bound_to(&binding) {
                        return Err(CodexError::RequestBuild {
                            reason: "provider replay routing or credential binding changed".into(),
                        });
                    }
                }
            }
            RouteDecoder::Anthropic(Box::new(
                crate::routes::anthropic::AnthropicDecoder::with_binding(binding)
                    .searching(request.web_search.as_ref().map(|search| search.max_uses()))
                    .instruction_mode(request.instruction_mode),
            ))
        }
        ProtocolFamily::GoogleGemini | ProtocolFamily::VertexGemini => {
            let binding = crate::replay::ReplayBinding::from_input(input).map_err(|reason| {
                CodexError::RequestBuild {
                    reason: reason.into(),
                }
            })?;
            let (ProviderRequest::GoogleGemini(request) | ProviderRequest::VertexGemini(request)) =
                &input.request
            else {
                return Err(CodexError::RequestBuild {
                    reason: "Gemini protocol mismatch".into(),
                });
            };
            for item in &request.messages {
                if let crate::CodexInputItem::ProviderReplay(replay) = item {
                    if !replay.bound_to(&binding) {
                        return Err(CodexError::RequestBuild {
                            reason: "Gemini replay routing or credential binding changed".into(),
                        });
                    }
                }
            }
            RouteDecoder::Gemini(Box::new(crate::gemini::Decoder::new(
                binding,
                input.attempt.as_str().into(),
                &request.tools,
            )))
        }
        ProtocolFamily::CohereChat => RouteDecoder::Cohere(Box::new(crate::cohere::Decoder::new(
            input.request.tools(),
            input.request.tool_choice(),
        ))),
        ProtocolFamily::WatsonxChat => RouteDecoder::Compatible(Box::new(
            crate::routes::compat::CompatibleDecoder::watsonx(&input.attempt),
        )),
        ProtocolFamily::OpenAiCompatible => {
            RouteDecoder::Compatible(Box::new(crate::routes::compat::CompatibleDecoder::new()))
        }
    })
}

/// One staged attempt: request assembled, slot held, no byte sent yet.
///
/// Not `Debug`: the assembled request carries the credential header.
#[allow(
    missing_debug_implementations,
    reason = "the staged request carries the credential header; a formatter would print it"
)]
pub(crate) struct StagedAttempt {
    clients: ScopedClients,
    request_fingerprint: Option<crate::RequestFingerprint>,
    diagnostic_session: Option<grapt_types::SessionId>,
    diagnostic_model: grapt_types::ModelRef,
    request: reqwest::RequestBuilder,
    error_text: Option<crate::ProviderErrorText>,
    error_lease: Option<CredentialLease>,
    credential_owner: Option<std::sync::Arc<dyn crate::CredentialOwner>>,
    dispatch: Option<(crate::DispatchObserver, grapt_types::PhysicalAttemptId)>,
    response_head: Duration,
    decoder: RouteDecoder,
    slot: AttemptSlot,
    capture: Option<crate::transport::capture::CaptureTap>,
    /// Where a Codex response's sticky-routing token is kept, for whom.
    turn: Option<(crate::CodexTurn, grapt_types::AccountId)>,
}

/// The Codex route's sticky-routing header, in both directions.
const TURN_STATE_HEADER: &str = "x-codex-turn-state";

impl StagedAttempt {
    pub(crate) fn cache_fingerprint(
        &self,
    ) -> Option<grapt_types::request_cache::RequestCacheFingerprint> {
        self.request_fingerprint.as_ref()?.durable()
    }

    pub(crate) fn diagnostic_session(&mut self, session: &grapt_types::SessionId) {
        // Diagnostic attribution cannot reject an otherwise valid attempt.
        self.diagnostic_session = (session.as_str().len() <= 128).then(|| session.clone());
    }

    /// Send the staged request. The single point at which bytes leave the
    /// process for this attempt.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(crate) async fn send(self) -> ProviderOpen {
        let StagedAttempt {
            clients,
            request_fingerprint,
            diagnostic_session,
            diagnostic_model,
            request,
            error_text,
            error_lease,
            credential_owner,
            dispatch,
            response_head,
            decoder,
            slot,
            capture,
            turn,
        } = self;

        let prepared =
            match super::renewal::prepare(credential_owner, request, error_lease, response_head)
                .await
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    clients.invalidate();
                    return ProviderOpen::Failed(Box::new(failure(
                        ProviderFailureClass::Authentication,
                        error.to_string(),
                    )));
                }
            };
        let super::renewal::Prepared {
            request,
            lease: error_lease,
            renewal,
        } = prepared;

        // Reservations may predate a different request's 429. Recheck at the
        // dispatch boundary, including callers that stage without reserving.
        if let Err(error) = slot.wait_for_cooldown().await {
            return ProviderOpen::Failed(Box::new(failure(
                ProviderFailureClass::Transport,
                error.to_string(),
            )));
        }

        // Witness seam: the last point before any request byte leaves. It is
        // gated on the explicit testkit feature and is a no-op in every other
        // build, so the race between a Step cancel and the outbound send can be
        // forced deterministically instead of caught by luck.
        #[cfg(any(test, feature = "loopback-testkit"))]
        if let Err(reason) = crate::transport::send_seam::hold(
            dispatch.as_ref().map(|(_, attempt)| attempt),
            diagnostic_session.as_ref(),
            response_head,
        )
        .await
        {
            // An enabled hold that cannot establish its ordering refuses the
            // attempt instead of sending untested.
            return ProviderOpen::Failed(Box::new(failure(
                ProviderFailureClass::Transport,
                reason,
            )));
        }

        // The response-head deadline. `send` resolves when the response
        // headers arrive, so this bounds exactly the head phase and nothing of
        // the body stream, which `StepPolicy::no_progress_timeout` owns.
        let mut timing = dispatch.map(|(observer, attempt)| observer.dispatched(attempt));
        if let Some(timing) = &mut timing {
            timing.request(diagnostic_session, request_fingerprint, diagnostic_model);
        }
        let sent = tokio::time::timeout(response_head, request.send()).await;

        let mut response = match sent {
            Err(_elapsed) => {
                return ProviderOpen::Failed(Box::new(failure(
                    ProviderFailureClass::Transport,
                    "the response-head deadline elapsed before the provider replied",
                )))
            }
            Ok(Err(_error)) => {
                // The reqwest error is deliberately not rendered into the
                // message: it can contain the request URL and, on some paths,
                // header context.
                return ProviderOpen::Failed(Box::new(failure(
                    ProviderFailureClass::Transport,
                    "the provider connection failed before a response was received",
                )));
            }
            Ok(Ok(response)) => response,
        };

        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            clients.invalidate();
        }
        if let Some(timing) = timing.as_mut() {
            timing.response_head(status.as_u16());
        }
        if !status.is_success() {
            // Read at most 16 KiB under a deadline to recognize the exact
            // overflow condition and the vendor's machine words. A client
            // error's failure message carries only the masked, 512-byte
            // preview of that body; unknown, oversized or unreadable bodies
            // keep the ordinary status classification.
            let retry_after = parse_retry_after(
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok()),
            );
            let details = tokio::time::timeout(
                response_head.min(Duration::from_secs(5)),
                crate::transport::classify::error_details(
                    &mut response,
                    overflow_route(&decoder),
                    // The excerpt is shown for every failed status, so the body
                    // is read for a 5xx as well as a 4xx. Classification still
                    // parses only a client-error body, so a 5xx gains no typed
                    // verdict from its own bytes.
                    error_lease.as_ref(),
                ),
            )
            .await
            .unwrap_or_default();
            let class = classify_error(status.as_u16(), retry_after, &details);
            if let crate::ProviderFailureClass::RateLimit { retry_after } = &class {
                slot.rate_limited(*retry_after);
            }
            // The volatile diagnostic gets the vendor's machine words as typed
            // fields; the failure message gets only the masked preview.
            if let Some(timing) = &timing {
                timing.refused(
                    status.as_u16(),
                    details.error_type.clone(),
                    details.error_code.clone(),
                );
            }
            // The failure row states the status, the vendor's machine code and a
            // bounded, already-redacted excerpt of the vendor's own message, so
            // a user sees why the provider refused without opting in. The
            // opt-in only ever added a copy of the FULL body to a file; it no
            // longer gates what the row says.
            let message = crate::transport::error_text::refusal_message(
                status.as_u16(),
                details.error_code,
                details.text.as_ref(),
            );
            let detail = if status.is_client_error() {
                error_text.map(|policy| policy.report(status.as_u16(), details.text))
            } else {
                None
            };
            let classification = match &renewal {
                Some(renewal) if renewal.recover(status.as_u16()).await => {
                    Some(crate::FailureClassification::CredentialRefreshed)
                }
                _ => details.classification,
            };
            let fragment = Box::new(AttemptFragment::ProviderFailure {
                class,
                classification,
                diagnostic: None,
                message,
                state: crate::fragment::AdapterState::none(),
            });
            return match detail {
                Some(detail) => ProviderOpen::FailedWithDetail { fragment, detail },
                None => ProviderOpen::Failed(fragment),
            };
        }

        if let Some(renewal) = renewal {
            renewal.accepted().await;
        }

        if let Some((turn, account)) = &turn {
            if let Some(token) = response
                .headers()
                .get(TURN_STATE_HEADER)
                .and_then(|value| value.to_str().ok())
            {
                turn.capture(account, token);
            }
        }
        ProviderOpen::Streaming(Box::new(
            ProviderStream::new(response, decoder, slot, capture, timing).redacting(error_lease),
        ))
    }
}

/// The overflow vocabulary this decoder's route speaks.
fn overflow_route(decoder: &RouteDecoder) -> crate::transport::classify::OverflowRoute {
    use crate::transport::classify::OverflowRoute;
    match decoder {
        RouteDecoder::Anthropic(_) => OverflowRoute::Anthropic,
        RouteDecoder::Responses(_) => OverflowRoute::Responses,
        RouteDecoder::Compatible(_) => OverflowRoute::Compatible,
        RouteDecoder::Gemini(_) | RouteDecoder::Cohere(_) | RouteDecoder::Bedrock(_) => {
            OverflowRoute::Other
        }
    }
}

fn request_url(input: &AttemptInput) -> Result<String, &'static str> {
    match input.request.family() {
        ProtocolFamily::GoogleGemini | ProtocolFamily::VertexGemini => crate::gemini::stream_url(
            input.endpoint.base(),
            input.request.model().model().as_str(),
        ),
        ProtocolFamily::VertexAnthropic => crate::vertex_anthropic::url(
            input.endpoint.base(),
            input.request.model().model().as_str(),
        ),
        ProtocolFamily::WatsonxChat => Ok(crate::watsonx::stream_url(input.endpoint.base())),
        _ => input.endpoint.url_for(input.request.model()),
    }
}

#[cfg(test)]
mod tests;
