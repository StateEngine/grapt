//! Route endpoints, their configuration precedence, and catalog-backed model
//! selection.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Provider request boundary" — "Routing is resolved above this boundary...
//! The adapter receives the resolved target but cannot select a different
//! provider, model family, account, credential version or route." This module
//! is what turns *configuration* into that resolved endpoint, and it does so
//! before any adapter exists.
//!
//! # Precedence is data, not an `unwrap_or` chain
//!
//! Three sources can name a base URL, and they are ordered. The oracle
//! expresses that ordering as nested `??` fallbacks, which has two costs: the
//! order is re-derived at every call site, and the *provenance* of the winning
//! value is lost the moment it is chosen. A receipt that says "base
//! <https://api.openai.com/v1>" cannot then answer whether an operator set it
//! or whether it is the built-in default — and those are very different
//! facts during an incident.
//!
//! So resolution returns a [`ResolvedEndpoint`] carrying both the value and the
//! [`EndpointSource`] that supplied it. The precedence itself is one ordered
//! list in one function, stated once.
//!
//! # What this module refuses
//!
//! - Remote plaintext bases, at every level of precedence. Literal loopback
//!   IP addresses may use HTTP for local providers. DNS names, userinfo,
//!   queries and fragments cannot widen that exception.
//! - A model the catalog does not carry. Selection consults
//!   [`ModelCatalog::entry`] and refuses `None` rather than constructing a
//!   `ModelRef` from asserted coordinates — the catalog is the authority on
//!   base model and family, and a target it never declared has neither.

use grapt_types::{ModelCatalog, ModelId, ModelRef, ModelRouteId, ModelVariantId, ProviderId};

use crate::request::ProtocolFamily;
mod cloudflare;
pub use cloudflare::{credential_can_authenticate, credential_placement};

/// Which configuration level supplied the endpoint that was used.
///
/// Recorded rather than inferred. See the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSource {
    /// An explicit per-attempt override. Highest precedence: a caller that
    /// names a base for one attempt means it.
    AttemptOverride,
    /// The deployment's configured base for this route family.
    ConfiguredProfile,
    /// The family's built-in default.
    FamilyDefault,
}

/// A base URL that failed validation, or a model the catalog does not carry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteConfigError {
    #[error("bearer Messages authentication requires an explicit Messages endpoint; reconnect and select its model")]
    MessagesAuthMismatch,
    #[error("the {level:?} endpoint must not be empty")]
    EndpointEmpty { level: EndpointSource },
    #[error("the {level:?} endpoint must use HTTPS or literal loopback HTTP without userinfo, query or fragment")]
    EndpointNotHttps { level: EndpointSource },
    #[error("this protocol family has no built-in default endpoint")]
    NoDefaultEndpoint,
    #[error(
        "the Cloudflare endpoint or protocol does not match its account, gateway and provider"
    )]
    CloudflareScopeMismatch,
    #[error("Connect watsonx with its IBM project before starting a request")]
    WatsonxProjectRequired,
    #[error("the catalog does not carry {provider}/{model}")]
    ModelNotInCatalog { provider: String, model: String },
}

/// A parsed, canonical plaintext endpoint admitted only by the loopback
/// testkit feature.
///
/// Keeping this as a type prevents any later provider layer from re-deciding
/// loopback authority with string-prefix logic. In particular,
/// `http://127.0.0.1:1@example.com/` has the tempting prefix but its parsed
/// authority is `example.com`, so construction refuses it.
///
/// The base may carry a path prefix: `/v1`, for example, is the root a
/// standard vLLM or llama.cpp server serves its API under. The prefix is
/// preserved apart from trailing-slash trimming, because the family path is
/// appended to the base and a kept slash would double; a query or fragment
/// is still refused.
#[cfg(any(test, feature = "loopback-testkit"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopbackBase(String);

#[cfg(any(test, feature = "loopback-testkit"))]
impl LoopbackBase {
    pub const MAX_BYTES: usize = 2_048;

    pub fn parse(raw: &str) -> Result<Self, RouteConfigError> {
        if raw.len() > Self::MAX_BYTES {
            return Err(RouteConfigError::EndpointNotHttps {
                level: EndpointSource::AttemptOverride,
            });
        }
        let parsed = reqwest::Url::parse(raw).map_err(|_| RouteConfigError::EndpointNotHttps {
            level: EndpointSource::AttemptOverride,
        })?;
        // The path is carried as a bounded prefix. Trailing slashes trim the
        // way the root path trims today, so the family path never doubles at
        // the join; an http URL's path is absolute, so a non-empty prefix
        // always starts with '/'.
        let prefix = parsed.path().trim_end_matches('/');
        let exact_authority = parsed.scheme() == "http"
            && parsed.host_str() == Some("127.0.0.1")
            && parsed.port().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none();
        if !exact_authority {
            return Err(RouteConfigError::EndpointNotHttps {
                level: EndpointSource::AttemptOverride,
            });
        }
        let port = parsed.port().ok_or(RouteConfigError::EndpointNotHttps {
            level: EndpointSource::AttemptOverride,
        })?;
        Ok(Self(format!("http://127.0.0.1:{port}{prefix}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The built-in default base for a family, if it has one.
///
/// # Provenance
///
/// Each URL below is read from the frozen oracle at `42e4888cc0`,
/// not from a live request or from memory:
/// The oracle's `openai.ts` (the OpenAI v1 base),
/// The oracle's `anthropic.ts` (the Anthropic v1 base),
/// The oracle's `openai-codex.ts` (the ChatGPT backend
/// base).
///
/// [`ProtocolFamily::OpenAiCompatible`] deliberately has **no** default. It is
/// the family whose whole purpose is a caller-supplied endpoint — Grok, a
/// self-hosted server, a gateway — and inventing a default for it would mean
/// picking one vendor to be the silent winner when configuration is missing.
/// A missing endpoint there is a configuration error with a name.
pub fn family_default_endpoint(family: ProtocolFamily) -> Option<&'static str> {
    match family {
        ProtocolFamily::OpenAiResponses => Some("https://api.openai.com/v1"),
        ProtocolFamily::ChatGptCodex => Some("https://chatgpt.com/backend-api/codex"),
        ProtocolFamily::AnthropicMessages => Some("https://api.anthropic.com/v1"),
        ProtocolFamily::OpenAiCompatible | ProtocolFamily::BedrockConverse => None,
        ProtocolFamily::GoogleGemini => Some("https://generativelanguage.googleapis.com/v1beta"),
        ProtocolFamily::CohereChat => Some("https://api.cohere.com"),
        ProtocolFamily::VertexAnthropic
        | ProtocolFamily::VertexGemini
        | ProtocolFamily::WatsonxChat => None,
    }
}

/// The endpoint configuration for one route family, before resolution.
///
/// Both optional levels are `Option<String>` and neither is validated at
/// construction: they are validated *when they win*, so an unused malformed
/// override does not fail an attempt that never consults it, and a used one
/// fails with its own source named.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointConfig {
    attempt_override: Option<String>,
    configured_profile: Option<String>,
    messages_auth: crate::MessagesAuthMode,
    cloudflare: Option<grapt_types::cloudflare::CloudflareGatewayScope>,
}

impl EndpointConfig {
    pub fn with_messages_auth(mut self, auth: crate::MessagesAuthMode) -> Self {
        self.messages_auth = auth;
        self
    }

    pub fn with_cloudflare(
        mut self,
        scope: grapt_types::cloudflare::CloudflareGatewayScope,
    ) -> Self {
        self.cloudflare = Some(scope);
        self
    }
    pub fn new() -> Self {
        Self::default()
    }

    /// The deployment's configured base for this family.
    pub fn with_configured_profile(mut self, base: impl Into<String>) -> Self {
        self.configured_profile = Some(base.into());
        self
    }

    /// An explicit base for one attempt. Outranks the profile.
    pub fn with_attempt_override(mut self, base: impl Into<String>) -> Self {
        self.attempt_override = Some(base.into());
        self
    }

    /// Resolve the base for `family`, recording which level supplied it.
    ///
    /// The precedence is this ordered list and nothing else. A reader who wants
    /// to know what beats what reads four lines, not a chain of `??` spread
    /// across call sites.
    pub fn resolve(&self, family: ProtocolFamily) -> Result<ResolvedEndpoint, RouteConfigError> {
        let candidates: [(EndpointSource, Option<&str>); 3] = [
            (
                EndpointSource::AttemptOverride,
                self.attempt_override.as_deref(),
            ),
            (
                EndpointSource::ConfiguredProfile,
                self.configured_profile.as_deref(),
            ),
            (
                EndpointSource::FamilyDefault,
                family_default_endpoint(family),
            ),
        ];

        for (source, candidate) in candidates {
            let Some(raw) = candidate else { continue };
            if self.messages_auth == crate::MessagesAuthMode::Bearer
                && (family != ProtocolFamily::AnthropicMessages
                    || source == EndpointSource::FamilyDefault)
            {
                return Err(RouteConfigError::MessagesAuthMismatch);
            }
            let base = validate(raw, source)?;
            if let Some(scope) = &self.cloudflare {
                cloudflare::check(scope, &base, family)?;
            }
            return Ok(ResolvedEndpoint {
                base,
                source,
                cloudflare: self.cloudflare.clone(),
                messages_auth: self.messages_auth,
            });
        }
        Err(RouteConfigError::NoDefaultEndpoint)
    }
}

/// A validated base URL and the level that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    base: String,
    source: EndpointSource,
    messages_auth: crate::MessagesAuthMode,
    cloudflare: Option<grapt_types::cloudflare::CloudflareGatewayScope>,
}

impl ResolvedEndpoint {
    pub fn messages_auth(&self) -> crate::MessagesAuthMode {
        self.messages_auth
    }

    pub fn cloudflare(&self) -> Option<&grapt_types::cloudflare::CloudflareGatewayScope> {
        self.cloudflare.as_ref()
    }
    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn source(&self) -> EndpointSource {
        self.source
    }

    pub(crate) fn is_plaintext_loopback(&self) -> bool {
        // Construction already parsed and validated the complete authority.
        self.base.starts_with("http://")
    }

    /// Bind this base to the route it serves, producing the endpoint an
    /// adapter is allowed to hold.
    ///
    /// The route is an **independent input**, never derived from the target the
    /// attempt later names: deriving it from `authority.model().route()` made the
    /// carrier's `EndpointServesAnotherRoute` refusal unfireable, a value
    /// compared with itself.
    pub fn bind(self, family: ProtocolFamily, route: ModelRouteId) -> ProviderEndpoint {
        ProviderEndpoint {
            base: self.base,
            source: self.source,
            messages_auth: self.messages_auth,
            family,
            route,
            cloudflare: self.cloudflare,
            watsonx_project: None,
        }
    }
}

/// A validated, route-bound endpoint for one protocol family.
///
/// Replaces the Codex-local `CodexEndpoint`: the https validation, the trailing
/// slash trim and the route binding were never Codex-specific, and three copies
/// of them would be three chances for one to be relaxed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEndpoint {
    messages_auth: crate::MessagesAuthMode,
    base: String,
    source: EndpointSource,
    family: ProtocolFamily,
    route: ModelRouteId,
    watsonx_project: Option<grapt_types::WatsonxProjectId>,
    cloudflare: Option<grapt_types::cloudflare::CloudflareGatewayScope>,
}

impl ProviderEndpoint {
    pub fn messages_auth(&self) -> crate::MessagesAuthMode {
        self.messages_auth
    }

    pub fn cloudflare(&self) -> Option<&grapt_types::cloudflare::CloudflareGatewayScope> {
        self.cloudflare.as_ref()
    }
    pub(crate) fn with_watsonx_project(
        mut self,
        project: Option<grapt_types::WatsonxProjectId>,
    ) -> Self {
        self.watsonx_project = project;
        self
    }
    pub(crate) fn watsonx_project(&self) -> Option<&grapt_types::WatsonxProjectId> {
        self.watsonx_project.as_ref()
    }
    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn source(&self) -> EndpointSource {
        self.source
    }

    pub fn family(&self) -> ProtocolFamily {
        self.family
    }

    /// The full URL for this family's streaming path.
    pub(crate) fn url(&self) -> String {
        format!("{}{}", self.base, self.family.path())
    }

    pub(crate) fn url_for(&self, model: &ModelRef) -> Result<String, &'static str> {
        if self.family == ProtocolFamily::GoogleGemini {
            return crate::gemini::stream_url(&self.base, model.model().as_str());
        }
        if self.family != ProtocolFamily::BedrockConverse {
            return Ok(self.url());
        }
        let mut url = reqwest::Url::parse(&self.base).map_err(|_| "invalid Bedrock endpoint")?;
        url.path_segments_mut()
            .map_err(|_| "invalid Bedrock endpoint path")?
            .pop_if_empty()
            .push("model")
            .push(model.model().as_str())
            .push("converse-stream");
        Ok(url.into())
    }

    /// A loopback endpoint for this crate's deterministic wire fixtures and
    /// explicitly feature-gated cross-crate acceptance fixtures.
    ///
    /// The caller must carry a parsed [`LoopbackBase`], so userinfo, a remote
    /// host, a query or fragment cannot masquerade behind a loopback string
    /// prefix; a path prefix is carried by the base itself. Production
    /// construction remains HTTPS-only.
    #[cfg(any(test, feature = "loopback-testkit"))]
    pub(crate) fn loopback(
        base: &LoopbackBase,
        family: ProtocolFamily,
        route: ModelRouteId,
    ) -> Self {
        Self {
            base: base.as_str().to_owned(),
            source: EndpointSource::AttemptOverride,
            messages_auth: crate::MessagesAuthMode::ApiKey,
            family,
            route,
            cloudflare: None,
            watsonx_project: None,
        }
    }
}

/// The endpoint was minted knowing its route, so the carrier's route check is a
/// comparison of issuance facts rather than of URL text.
impl crate::authority::RouteBoundEndpoint for ProviderEndpoint {
    fn route(&self) -> &ModelRouteId {
        &self.route
    }
}

impl ProtocolFamily {
    /// The streaming path this family appends to its base.
    ///
    /// # Provenance
    ///
    /// The oracle's `open-responses.ts` (`export const PATH =
    /// "/responses"`, shared by the OpenAI Responses route and the ChatGPT
    /// backend), the oracle's `anthropic-messages.ts`
    /// (`/messages`), and the OpenAI chat-completions path every
    /// OpenAI-compatible server implements.
    pub(crate) fn path(self) -> &'static str {
        match self {
            Self::OpenAiResponses | Self::ChatGptCodex => "/responses",
            Self::AnthropicMessages => "/messages",
            Self::VertexAnthropic => "",
            Self::OpenAiCompatible => "/chat/completions",
            Self::BedrockConverse => "/converse-stream",
            Self::GoogleGemini => "/models",
            Self::CohereChat => "/v2/chat",
            Self::VertexGemini => "/models",
            Self::WatsonxChat => "",
        }
    }
}

fn validate(raw: &str, level: EndpointSource) -> Result<String, RouteConfigError> {
    if raw.len() > 2048 {
        return Err(RouteConfigError::EndpointNotHttps { level });
    }
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(RouteConfigError::EndpointEmpty { level });
    }
    let parsed =
        reqwest::Url::parse(trimmed).map_err(|_| RouteConfigError::EndpointNotHttps { level })?;
    let loopback = parsed
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|address| address.is_loopback());
    let allowed = parsed.scheme() == "https" || parsed.scheme() == "http" && loopback;
    if !allowed
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(RouteConfigError::EndpointNotHttps { level });
    }
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

/// Select a model from a catalog release.
///
/// The catalog is the authority on base model and family, so a target it does
/// not carry is refused here — before an adapter exists and therefore before
/// any byte could leave. Constructing a [`ModelRef`] from coordinates a caller
/// asserted would be the alternative, and it would let a typo reach the wire as
/// a request for a model nobody has ever declared.
pub fn select_model(
    catalog: &ModelCatalog,
    provider: &ProviderId,
    model: &ModelId,
    variant: Option<&ModelVariantId>,
) -> Result<ModelRef, RouteConfigError> {
    match catalog.entry(provider, model, variant) {
        Some(entry) => Ok(ModelRef::from_catalog(entry)),
        None => Err(RouteConfigError::ModelNotInCatalog {
            provider: provider.as_str().to_owned(),
            model: model.as_str().to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests;
