//! The public provider-owned attempt opener. It performs issue/bind/from_carrier
//! internally and returns an attempt reader plus custody; the five internal
//! seals and `into_parts` remain `pub(crate)`.
//!
//! THE ONLY WAY IN: [`AttemptOpener::open`] runs the sealed
//! issue/bind/from_carrier sequence in-crate, so an outside caller ends up
//! holding exactly an [`AttemptCustody`] and an [`AttemptReader`]. Requests go
//! IN; no endpoint, lease or bound request ever comes back out.
//!
//! ZERO BYTES BEFORE THE FIRST POLL: `open` is synchronous and performs no I/O,
//! so a runtime that must make the attempt durable BEFORE the provider opens
//! has a guaranteed window. ONE BINDING LAW: the deterministic and Codex arms
//! run the SAME sequence, so a deterministic-arm witness exercises the wire's
//! authority path; only the byte source differs, after binding completes.
//!
//! The full boundary argument, the refusal inventory and the two-transport
//! rationale are recorded in this module's introducing commit rather than
//! carried here.

use std::sync::Arc;
#[cfg(any(test, feature = "deterministic-testkit"))]
use std::sync::Mutex;

use grapt_policy::PhysicalAttemptAuthoritySnapshot;
// The opener no longer holds raw credential material: it holds the injected
// capability, and `AccountId`/`CredentialVersion`/`Secret` moved there with it.
use grapt_types::{ModelRouteId, PhysicalAttemptId};

use crate::authority::{AttemptCustody, CarrierRefusal, ProviderAttemptCarrier};
use crate::codex::{CodexConfigError, CodexDeadlines, CodexError, CodexLimits};
use crate::credential::{CredentialError, CredentialLease};
#[cfg(any(test, feature = "deterministic-testkit"))]
use crate::deterministic::{AttemptStream, DeterministicAdapter, Script};
use crate::fragment::AttemptFragment;
use crate::request::{ProtocolFamily, ProviderRequest};
use crate::route::{EndpointConfig, ProviderEndpoint, RouteConfigError};
use crate::transport::adapter::{AttemptInput, ProviderAdapter, ProviderOpen, StagedAttempt};
use crate::transport::client::build_client;
use crate::transport::stream::ProviderStream;

/// The opener's injected credential capability.
///
/// [`crate::credential::ProviderCredential`] is one opaque
/// injected capability shared by every route — because four route families
/// each carrying their own credential struct would be four places for the
/// "derives nothing" property to be weakened by one convenient `Clone`.
pub use crate::credential::ProviderCredential as OpenerCredential;

/// Maximum UTF-8 bytes in a physical-attempt identity accepted by the public
/// opener boundary.
///
/// `PhysicalAttemptId` itself is a cross-crate identity and guarantees only
/// non-emptiness. The provider owns several count-bounded structures that
/// clone and retain it, including the capture announcement queue, so this
/// boundary supplies the missing value bound before any of those clones can
/// occur.
pub const MAX_PHYSICAL_ATTEMPT_ID_BYTES: usize = 256;

/// Why the opener could not produce a reader. No variant implies any byte left
/// the process. Custody exists from `from_carrier` onward, so a later staging
/// or transport failure RETURNS the custody inside its refusal: a consumer that
/// already made the attempt durable must hold exact custody through the error
/// terminal it is about to write. Earlier refusals carry none.
#[derive(Debug, thiserror::Error)]
pub enum AttemptOpenRefusal {
    /// The attempt identity would make a count-bounded provider structure hold
    /// an unbounded value. Checked first, before carrier/custody construction,
    /// active-slot insertion, or capture announcement.
    #[error(
        "the physical attempt identity is {observed_bytes} bytes; the provider limit is {max_bytes}"
    )]
    AttemptIdentityTooLarge {
        observed_bytes: usize,
        max_bytes: usize,
    },
    /// The four coordinates do not belong to one current issuance. Carries the
    /// exact disagreement, including [`CarrierRefusal::StaleIssuance`] and
    /// [`CarrierRefusal::RequestFromAnotherCatalog`]. Zero bytes, pre-custody.
    #[error("the carrier refused to bind this attempt: {0:?}")]
    Carrier(CarrierRefusal),
    /// A configuration input was malformed (user agent, deadlines).
    /// Pre-custody.
    #[error(transparent)]
    Config(#[from] CodexConfigError),
    /// The endpoint configuration did not resolve, or resolved to something
    /// this crate refuses. Pre-custody, zero bytes.
    #[error(transparent)]
    Route(#[from] RouteConfigError),
    /// The injected credential could not be leased. Pre-custody.
    #[error(transparent)]
    Credential(#[from] CredentialError),
    /// The adapter itself could not be built (TLS, client construction).
    /// Refused at opener construction — before any attempt, so pre-custody.
    #[error(transparent)]
    Build(#[from] CodexError),
    /// The adapter refused to stage: capacity, duplicate attempt, or an
    /// unserializable request. A harness condition, not a provider fault, still
    /// zero bytes — and the custody that already existed comes back with it.
    #[error("staging refused: {error}")]
    Staging {
        custody: Box<AttemptCustody>,
        error: CodexError,
    },
    /// The injected credential's placement cannot authenticate this family —
    /// a bearer token configured for Anthropic, or an api-key configured for an
    /// OpenAI-shaped route. Refused at opener construction, so no attempt can
    /// exist that would have failed 401 for a reason the operator could not see.
    #[error("the injected credential cannot authenticate the {family:?} family")]
    CredentialCannotAuthenticateFamily { family: ProtocolFamily },
    /// The request belongs to a different protocol family than this opener
    /// was configured for. Zero bytes, pre-custody.
    #[error("this opener serves {configured:?} but the request is {request:?}")]
    RequestFromAnotherFamily {
        configured: ProtocolFamily,
        request: ProtocolFamily,
    },
    /// The deterministic transport had no script left, or was asked to open one
    /// attempt twice. The deterministic analogue of [`Self::Staging`], custody
    /// included.
    #[cfg(any(test, feature = "deterministic-testkit"))]
    #[error("the deterministic test transport refused: {reason}")]
    Deterministic {
        custody: Box<AttemptCustody>,
        reason: String,
    },
}

impl AttemptOpenRefusal {
    /// The adapter's active-attempt capacity was full: an ordinary operating
    /// condition that passes, never a fault in the request or the provider.
    pub fn is_saturated(&self) -> bool {
        matches!(
            self,
            Self::Staging {
                error: CodexError::ActiveAttemptCapacity { .. },
                ..
            }
        )
    }
}

/// What one successful `open` yields: the custody the aggregate retains and the
/// reader the supervisor polls, split by [`Self::into_parts`] because the two
/// halves have different owners.
pub struct OpenedAttempt {
    custody: AttemptCustody,
    reader: AttemptReader,
}

/// Hand-written: the custody's own bounded view, and `<sealed>` for the reader
/// — its staged arm owns the assembled request, authorization header included.
impl core::fmt::Debug for OpenedAttempt {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OpenedAttempt")
            .field("custody", &self.custody)
            .field("reader", &"<sealed>")
            .finish()
    }
}

impl OpenedAttempt {
    /// The final staged request, before polling can send any bytes.
    pub fn cache_fingerprint(&self) -> Option<grapt_types::request_cache::RequestCacheFingerprint> {
        match &self.reader.state {
            ReaderState::Staged(staged) => staged.cache_fingerprint(),
            _ => None,
        }
    }

    /// Process-local attribution only; never changes sealed authority or bytes.
    pub fn with_diagnostic_session(mut self, session: &grapt_types::SessionId) -> Self {
        if let ReaderState::Staged(staged) = &mut self.reader.state {
            staged.diagnostic_session(session);
        }
        self
    }

    /// Split into the custody the attempt aggregate holds through its durable
    /// terminal, and the reader the provider task consumes.
    pub fn into_parts(self) -> (AttemptCustody, AttemptReader) {
        (self.custody, self.reader)
    }
}

/// One attempt's place on the opener's transport, held from
/// [`AttemptOpener::reserve_capacity`] until [`AttemptOpener::open_reserved`]
/// stages the attempt in it. Dropped unused, the place is freed.
#[derive(Debug, Default)]
pub struct AttemptCapacity(Option<crate::transport::slot::SlotReservation>);

/// The one byte source of one attempt. Yields the closed [`AttemptFragment`]
/// alphabet both transports normalize into, ALWAYS starting with
/// `AttemptOpened` — even when the send itself failed, because the consumer's
/// opening law is positional. For Codex the FIRST poll sends the staged
/// request; everything earlier happened inside [`AttemptOpener::open`].
#[allow(
    missing_debug_implementations,
    reason = "the staged arm owns the assembled request, authorization header included"
)]
pub struct AttemptReader {
    error_detail: Option<crate::ProviderErrorDetail>,
    state: ReaderState,
}

enum ReaderState {
    /// A real route: staged, unsent. Consumed by the first poll.
    Staged(Box<StagedAttempt>),
    /// A real route: live stream.
    Streaming(Box<ProviderStream>),
    /// Test-only deterministic scripted fragments.
    #[cfg(any(test, feature = "deterministic-testkit"))]
    Scripted(AttemptStream),
    /// A pre-stream failure fragment waiting to be yielded, then end.
    Failing(Box<AttemptFragment>),
    /// Terminal. Every subsequent poll returns `None`.
    Finished,
}

impl AttemptReader {
    /// Consume the attempt-owned sidecar. It never enters normalized history.
    pub fn take_error_detail(&mut self) -> Option<crate::ProviderErrorDetail> {
        self.error_detail.take()
    }

    /// The next normalized fragment, or `None` once the stream is over.
    /// Cancellation is dropping the reader: request, response and capacity slot
    /// are owned here and die with it.
    pub async fn next_fragment(&mut self) -> Option<AttemptFragment> {
        loop {
            match std::mem::replace(&mut self.state, ReaderState::Finished) {
                ReaderState::Staged(staged) => match staged.send().await {
                    ProviderOpen::Streaming(stream) => {
                        self.state = ReaderState::Streaming(stream);
                    }
                    ProviderOpen::FailedWithDetail { fragment, detail } => {
                        self.error_detail = Some(detail);
                        self.state = ReaderState::Failing(fragment);
                        return Some(AttemptFragment::AttemptOpened);
                    }
                    ProviderOpen::Failed(fragment) => {
                        // Open FIRST, then hand the typed failure
                        // over unchanged. A failure at position one is refused
                        // as InvalidProviderOutput by the positional opening
                        // law, which would overwrite the adapter's own class
                        // and make a retryable 429 or drop arrive terminal.
                        self.state = ReaderState::Failing(fragment);
                        return Some(AttemptFragment::AttemptOpened);
                    }
                },
                ReaderState::Streaming(mut stream) => {
                    let next = stream.next_fragment().await;
                    self.error_detail = stream.take_error_detail();
                    if next.is_some() {
                        self.state = ReaderState::Streaming(stream);
                    }
                    return next;
                }
                #[cfg(any(test, feature = "deterministic-testkit"))]
                ReaderState::Scripted(mut script) => {
                    let next = script.next();
                    if next.is_some() {
                        self.state = ReaderState::Scripted(script);
                    }
                    return next;
                }
                ReaderState::Failing(fragment) => return Some(*fragment),
                ReaderState::Finished => return None,
            }
        }
    }
}

enum Transport {
    /// One adapter serves every real route family. A per-family variant would
    /// be four clients, four capacity registries and four staging paths for one law.
    Real(ProviderAdapter),
    /// Test-only interior mutability because `open` takes `&self` and the deterministic
    /// adapter consumes a script per open; a poisoned lock refuses, never panics.
    #[cfg(any(test, feature = "deterministic-testkit"))]
    Deterministic(Mutex<DeterministicAdapter>),
}

/// The public provider-owned opener: one call performs endpoint issuance,
/// lease issuance, carrier binding and carrier decomposition — the sealed
/// sequence — and returns the reader-plus-custody pair. See the module note.
#[allow(
    missing_debug_implementations,
    reason = "it owns an OpenerCredential; a derived formatter is a read channel toward it"
)]
pub struct AttemptOpener {
    transport: Transport,
    /// The family this opener serves. Independent of the request it is later
    /// handed: `open` refuses a request from another family rather than posting
    /// its body to this family's path.
    family: ProtocolFamily,
    base: OpenerBase,
    /// The route this opener's endpoint was configured FOR — a real, independent
    /// input, never copied from a target: deriving it from
    /// `authority.model().route()` made `EndpointServesAnotherRoute`
    /// unfireable, a value compared with itself.
    route: ModelRouteId,
    user_agent: String,
    credential: OpenerCredential,
    watsonx_project: Option<grapt_types::WatsonxProjectId>,
}

enum OpenerBase {
    Configured(EndpointConfig),
    #[cfg(any(test, feature = "loopback-testkit"))]
    Loopback(crate::route::LoopbackBase),
}

impl AttemptOpener {
    /// Keep a named provider's compatible format through endpoint overrides.
    pub fn with_compatible_protocol(mut self, protocol: crate::CompatibleProtocol) -> Self {
        match &mut self.transport {
            Transport::Real(adapter) => adapter.set_compatible_protocol(protocol),
            #[cfg(any(test, feature = "deterministic-testkit"))]
            Transport::Deterministic(_) => {}
        }
        self
    }

    /// Bind the verified connection's IBM project before any attempt is staged.
    pub fn with_watsonx_project(
        mut self,
        project: grapt_types::WatsonxProjectId,
    ) -> Result<Self, AttemptOpenRefusal> {
        if self.family != ProtocolFamily::WatsonxChat {
            return Err(RouteConfigError::WatsonxProjectRequired.into());
        }
        self.watsonx_project = Some(project);
        Ok(self)
    }
    /// The credential owner supplies material only; the existing carrier keeps
    /// ownership of route, account, version and placement authorization.
    pub fn with_credential_owner(mut self, owner: Arc<dyn crate::CredentialOwner>) -> Self {
        self.transport = match self.transport {
            Transport::Real(adapter) => Transport::Real(adapter.with_credential_owner(owner)),
            #[cfg(any(test, feature = "deterministic-testkit"))]
            Transport::Deterministic(adapter) => Transport::Deterministic(adapter),
        };
        self
    }

    /// Observe the real transport send boundary without retaining request data.
    /// Scripted transports emit no dispatch observations.
    pub fn with_dispatch_observer(mut self, observer: crate::DispatchObserver) -> Self {
        self.transport = match self.transport {
            Transport::Real(adapter) => Transport::Real(adapter.with_dispatch(observer)),
            #[cfg(any(test, feature = "deterministic-testkit"))]
            Transport::Deterministic(adapter) => Transport::Deterministic(adapter),
        };
        self
    }
    /// Configure local 4xx error-text capture and its discoverability hint.
    pub fn with_error_text(mut self, policy: crate::ProviderErrorText) -> Self {
        self.transport = match self.transport {
            Transport::Real(adapter) => Transport::Real(adapter.with_error_text(policy)),
            #[cfg(any(test, feature = "deterministic-testkit"))]
            Transport::Deterministic(adapter) => Transport::Deterministic(adapter),
        };
        self
    }

    /// The production ChatGPT/Codex opener. Deadlines and capacity are injected
    /// with no defaults; the base URL must use HTTPS or literal loopback HTTP,
    /// validated here once. The route names what this base serves — a deployment property,
    /// checked per attempt against the issued one.
    /// The production ChatGPT/Codex opener.
    ///
    /// Kept as a named constructor rather than folded into [`Self::route`] so
    /// every existing caller and witness is unchanged; it is now one line over
    /// the general path.
    pub fn codex(
        deadlines: CodexDeadlines,
        limits: CodexLimits,
        base: &str,
        route: ModelRouteId,
        user_agent: &str,
        credential: OpenerCredential,
    ) -> Result<Self, AttemptOpenRefusal> {
        Self::route(
            ProtocolFamily::ChatGptCodex,
            deadlines,
            limits,
            EndpointConfig::new().with_attempt_override(base),
            route,
            user_agent,
            credential,
        )
    }

    /// The production opener for ANY real route family.
    ///
    /// Deadlines and capacity are injected with no defaults. The endpoint
    /// arrives as an [`EndpointConfig`] rather than a string, so a deployment
    /// gets the ordered precedence and the family default for free, and the
    /// winning level is recorded rather than inferred.
    ///
    /// # The credential must be able to authenticate this family
    ///
    /// Checked before constructing a transport: the captured credential mode,
    /// endpoint mode and placement agree. A bearer Messages token also retains
    /// its approved full base, so changing the path cannot repurpose it.
    pub fn route(
        family: ProtocolFamily,
        deadlines: CodexDeadlines,
        limits: CodexLimits,
        endpoint: EndpointConfig,
        route: ModelRouteId,
        user_agent: &str,
        credential: OpenerCredential,
    ) -> Result<Self, AttemptOpenRefusal> {
        let resolved = endpoint.resolve(family)?;
        let factory = if resolved.is_plaintext_loopback() {
            crate::transport::client::build_loopback_client
        } else {
            build_client
        };
        if user_agent.trim().is_empty() {
            return Err(AttemptOpenRefusal::Config(CodexConfigError::UserAgentEmpty));
        }
        if credential.messages_auth() != resolved.messages_auth() {
            return Err(CredentialError::DestinationMismatch.into());
        }
        credential.check_messages_base(resolved.base())?;
        if credential.cloudflare() != resolved.cloudflare() {
            return Err(CredentialError::DestinationMismatch.into());
        }
        if !crate::credential_can_authenticate(
            family,
            resolved.cloudflare(),
            resolved.messages_auth(),
            credential.placement(),
        ) {
            return Err(AttemptOpenRefusal::CredentialCannotAuthenticateFamily { family });
        }
        Ok(Self {
            transport: Transport::Real(ProviderAdapter::new(
                factory,
                deadlines,
                limits.active_attempts(),
            )),
            family,
            base: OpenerBase::Configured(endpoint),
            route,
            user_agent: user_agent.to_owned(),
            credential,
            watsonx_project: None,
        })
    }

    /// The explicitly feature-gated deterministic witness adapter behind the
    /// same binding law, one script per opened attempt in order.
    #[cfg(any(test, feature = "deterministic-testkit"))]
    pub fn deterministic(
        scripts: impl IntoIterator<Item = Script>,
        route: ModelRouteId,
        credential: OpenerCredential,
    ) -> Self {
        Self {
            transport: Transport::Deterministic(Mutex::new(DeterministicAdapter::new(scripts))),
            family: ProtocolFamily::ChatGptCodex,
            // Never contacted: the deterministic arm reads fragments from its
            // script. The name is reserved and unresolvable by construction.
            base: OpenerBase::Configured(
                EndpointConfig::new().with_attempt_override("https://deterministic.invalid"),
            ),
            route,
            user_agent: "grapt-deterministic".to_owned(),
            credential,
            watsonx_project: None,
        }
    }

    /// An opener whose client accepts parsed plaintext loopback authority for
    /// this crate's internal wire fixtures.
    #[cfg(test)]
    fn loopback(
        family: ProtocolFamily,
        deadlines: CodexDeadlines,
        limits: CodexLimits,
        base: &str,
        route: ModelRouteId,
        credential: OpenerCredential,
    ) -> Result<Self, AttemptOpenRefusal> {
        let base = crate::route::LoopbackBase::parse(base).map_err(AttemptOpenRefusal::Route)?;
        Ok(Self {
            transport: Transport::Real(ProviderAdapter::new(
                crate::transport::client::build_loopback_client,
                deadlines,
                limits.active_attempts(),
            )),
            family,
            base: OpenerBase::Loopback(base),
            route,
            user_agent: "grapt-loopback".to_owned(),
            credential,
            watsonx_project: None,
        })
    }

    /// Controlled plaintext loopback constructor for cross-crate product
    /// acceptance. This symbol is absent unless the explicit testkit feature is
    /// selected; the production constructor remains HTTPS-only.
    #[cfg(feature = "loopback-testkit")]
    pub fn loopback_testkit(
        family: ProtocolFamily,
        deadlines: CodexDeadlines,
        limits: CodexLimits,
        base: crate::route::LoopbackBase,
        route: ModelRouteId,
        credential: OpenerCredential,
    ) -> Result<Self, AttemptOpenRefusal> {
        Ok(Self {
            transport: Transport::Real(ProviderAdapter::new(
                crate::transport::client::build_loopback_client,
                deadlines,
                limits.active_attempts(),
            )),
            family,
            base: OpenerBase::Loopback(base),
            route,
            user_agent: "grapt-loopback".to_owned(),
            credential,
            watsonx_project: None,
        })
    }

    #[cfg(test)]
    fn codex_loopback(
        deadlines: CodexDeadlines,
        limits: CodexLimits,
        base: &str,
        route: ModelRouteId,
        credential: OpenerCredential,
    ) -> Result<Self, AttemptOpenRefusal> {
        Self::loopback(
            ProtocolFamily::ChatGptCodex,
            deadlines,
            limits,
            base,
            route,
            credential,
        )
    }

    /// Bind one attempt and stage its byte source. Synchronous; zero bytes.
    ///
    /// The sealed sequence, refusals surfaced: `CodexEndpoint::issue` for
    /// exactly the issued route, making the carrier's route check an
    /// issuance-fact comparison; `CredentialLease::issue` from the held
    /// credential; `ProviderAttemptCarrier::bind`, staleness first then every
    /// coordinate and tool check, whose zero-byte refusal is witnessed through
    /// the public opener; `CodexAttemptInput::from_carrier`, where custody comes
    /// out; then staging. The authority snapshot is consumed BY VALUE because
    /// the issuance inside it is: afterwards the only holder of those
    /// coordinates is the custody, which answers bounded questions only.
    pub fn open(
        &self,
        attempt: PhysicalAttemptId,
        authority: PhysicalAttemptAuthoritySnapshot,
        request: ProviderRequest,
    ) -> Result<OpenedAttempt, AttemptOpenRefusal> {
        self.open_reserved(attempt, authority, request, AttemptCapacity::default())
    }

    /// Configure the child pool before sharing this opener. Credentials and
    /// transport clients remain shared; child streams cannot consume root slots.
    pub fn with_child_capacity(mut self, capacity: std::num::NonZeroUsize) -> Self {
        self.transport = match self.transport {
            Transport::Real(adapter) => Transport::Real(adapter.with_child_capacity(capacity)),
            #[cfg(any(test, feature = "deterministic-testkit"))]
            Transport::Deterministic(adapter) => Transport::Deterministic(adapter),
        };
        self
    }

    /// FIFO child admission, narrowed by this execution's project policy.
    pub async fn reserve_child_capacity_observed(
        &self,
        limit: std::num::NonZeroUsize,
        observer: impl FnMut(usize, bool) + Send,
    ) -> Result<AttemptCapacity, CodexError> {
        match &self.transport {
            Transport::Real(adapter) => adapter
                .reserve_child(limit, Some(Box::new(observer)))
                .await
                .map(|held| AttemptCapacity(Some(held))),
            #[cfg(any(test, feature = "deterministic-testkit"))]
            Transport::Deterministic(_) => Ok(AttemptCapacity::default()),
        }
    }

    /// Wait until the transport has room for one more attempt and hold it.
    ///
    /// The adapter's active-attempt capacity is shared by every Session in the
    /// process. Without this wait an automatic compaction met whatever stream
    /// was open at that instant, a parent's or another child's, and lost its
    /// turn to a refusal. The caller bounds the wait; a refusal
    /// that remains after it is genuine saturation. The deterministic
    /// transport has no capacity and holds nothing.
    pub async fn reserve_capacity(&self) -> Result<AttemptCapacity, CodexError> {
        self.reserve_capacity_observed(|_, _| {}).await
    }

    /// Observe FIFO position changes and normalized progress by active holders.
    /// The first observation establishes a baseline; enqueue/cancel is not progress.
    pub async fn reserve_capacity_observed(
        &self,
        observer: impl FnMut(usize, bool) + Send,
    ) -> Result<AttemptCapacity, CodexError> {
        match &self.transport {
            Transport::Real(adapter) => adapter
                .reserve(Some(Box::new(observer)))
                .await
                .map(|held| AttemptCapacity(Some(held))),
            #[cfg(any(test, feature = "deterministic-testkit"))]
            Transport::Deterministic(_) => Ok(AttemptCapacity::default()),
        }
    }

    /// [`Self::open`], staging the attempt in a place `capacity` holds.
    pub fn open_reserved(
        &self,
        attempt: PhysicalAttemptId,
        authority: PhysicalAttemptAuthoritySnapshot,
        request: ProviderRequest,
        capacity: AttemptCapacity,
    ) -> Result<OpenedAttempt, AttemptOpenRefusal> {
        let attempt_bytes = attempt.as_str().len();
        if attempt_bytes > MAX_PHYSICAL_ATTEMPT_ID_BYTES {
            return Err(AttemptOpenRefusal::AttemptIdentityTooLarge {
                observed_bytes: attempt_bytes,
                max_bytes: MAX_PHYSICAL_ATTEMPT_ID_BYTES,
            });
        }
        // The opener's configured family is an INDEPENDENT input, exactly as
        // its route is. A request from another family would otherwise have its
        // body posted to this family's path with this family's headers, and the
        // first evidence of it would be a provider error.
        if request.family() != self.family {
            return Err(AttemptOpenRefusal::RequestFromAnotherFamily {
                configured: self.family,
                request: request.family(),
            });
        }
        // Issued from the opener's own CONFIGURED route — the independent
        // identity the carrier's route check compares against the target's.
        let endpoint = self
            .issue_endpoint()?
            .with_watsonx_project(self.watsonx_project.clone());
        let lease = self.credential.lease()?;
        let carrier = ProviderAttemptCarrier::bind(attempt, authority, endpoint, lease, request)
            .map_err(AttemptOpenRefusal::Carrier)?;
        let (input, custody) = attempt_input_from_carrier(carrier, &self.user_agent)?;

        // Custody exists from here on. Every failure below returns it inside
        // its refusal rather than destroying it.
        let reader = match &self.transport {
            Transport::Real(adapter) => match adapter.stage_reserved(input, capacity.0) {
                Ok(staged) => AttemptReader {
                    error_detail: None,
                    state: ReaderState::Staged(Box::new(staged)),
                },
                Err(error) => {
                    return Err(AttemptOpenRefusal::Staging {
                        custody: Box::new(custody),
                        error,
                    })
                }
            },
            #[cfg(any(test, feature = "deterministic-testkit"))]
            Transport::Deterministic(adapter) => {
                let stream = {
                    let mut adapter = match adapter.lock() {
                        Ok(adapter) => adapter,
                        Err(_poisoned) => {
                            return Err(AttemptOpenRefusal::Deterministic {
                                custody: Box::new(custody),
                                reason: "the deterministic transport lock is poisoned".to_owned(),
                            })
                        }
                    };
                    match adapter.open(input.attempt().clone()) {
                        Ok(stream) => stream,
                        Err(error) => {
                            return Err(AttemptOpenRefusal::Deterministic {
                                custody: Box::new(custody),
                                reason: error.to_string(),
                            })
                        }
                    }
                };
                AttemptReader {
                    error_detail: None,
                    state: ReaderState::Scripted(stream),
                }
            }
        };

        Ok(OpenedAttempt { custody, reader })
    }

    /// Test-only composition of the public opener with the capture seam. It
    /// proves that the public `open` boundary refuses an oversized identity
    /// before either an empty or full announcement queue can observe it.
    #[cfg(test)]
    fn with_capture(self, capture: crate::transport::capture::CaptureExchange) -> Self {
        let Self {
            transport,
            family,
            base,
            route,
            user_agent,
            credential,
            watsonx_project,
        } = self;
        let transport = match transport {
            Transport::Real(adapter) => Transport::Real(adapter.with_capture(capture)),
            #[cfg(any(test, feature = "deterministic-testkit"))]
            Transport::Deterministic(adapter) => Transport::Deterministic(adapter),
        };
        Self {
            transport,
            family,
            base,
            route,
            user_agent,
            credential,
            watsonx_project,
        }
    }

    fn issue_endpoint(&self) -> Result<ProviderEndpoint, AttemptOpenRefusal> {
        match &self.base {
            OpenerBase::Configured(endpoint) => Ok(endpoint
                .resolve(self.family)?
                .bind(self.family, self.route.clone())),
            #[cfg(any(test, feature = "loopback-testkit"))]
            OpenerBase::Loopback(base) => Ok(ProviderEndpoint::loopback(
                base,
                self.family,
                self.route.clone(),
            )),
        }
    }
}

/// Decompose a bound carrier into the adapter's input.
///
/// The ONLY route from a bound carrier to a transport. The four coordinates
/// arrive already proven against one issuance — the carrier refused every
/// disagreement before this is reachable — and the custody comes out here, so
/// every failure after this point returns it rather than destroying it.
///
/// There is no constructor taking loose parts. That is the "no alternate raw
/// provider opening path" half of the rule: an adapter cannot be handed an
/// endpoint, a lease and a request that were never checked against a registry
/// issuance, because no such function exists.
fn attempt_input_from_carrier(
    carrier: ProviderAttemptCarrier<ProviderEndpoint, CredentialLease, ProviderRequest>,
    user_agent: &str,
) -> Result<(AttemptInput, AttemptCustody), CodexConfigError> {
    if user_agent.trim().is_empty() {
        return Err(CodexConfigError::UserAgentEmpty);
    }
    let (custody, endpoint, lease, request) = carrier.into_parts();
    let attempt = custody.attempt().clone();
    Ok((
        AttemptInput {
            attempt,
            endpoint,
            lease,
            request,
            user_agent: user_agent.to_owned(),
        },
        custody,
    ))
}

#[cfg(test)]
mod tests;
