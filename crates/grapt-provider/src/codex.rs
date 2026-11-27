//! The ChatGPT/Codex route adapter — the first real provider transport.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Provider request boundary" ("The selected adapter alone may serialize its
//! request into HTTP bytes. It also owns request validation, auth/header
//! placement, stream decoding, raw finish mapping, usage accounting, and
//! provider-specific retry classification") and §"Initial rulings" ("the first
//! adapter sequence is the deterministic test adapter, then the ChatGPT/Codex
//! route that reproduces the `max_output_tokens` regression").
//!
//! # What this module owns
//!
//! One route family, per the master's crate-graph amendment "no adapter may
//! exceed one route family": request bytes, headers, the two injected
//! deadlines, TLS construction, SSE decoding into the existing
//! [`crate::fragment::AttemptFragment`] alphabet, and provider-specific failure
//! classification.
//!
//! # What it deliberately does not own
//!
//! - **Retry.** It classifies; it never sleeps, never retries, and never issues
//!   a second request. `grapt-runtime`'s `StepPolicy` owns the five-attempt
//!   cap, the backoff and the no-progress deadline.
//! - **Credential refresh.** Refresh is single-flight in the credential
//!   owner, outside this crate. A 401/403 is terminal here precisely because an
//!   in-attempt refresh would change an in-flight attempt's credential version,
//!   which the master's mutation table fails ("Change provider/model/route/
//!   account/credential version without minting a new PhysicalAttempt and exact
//!   resolved-target receipt → identity/receipt assertion fails").
//! - **Routing.** The adapter receives its coordinates and cannot select a
//!   different provider, model, account, credential version or route.
//! - **Durable state.** Nothing here writes SQLite or emits an event.
//!
//! # Deadlines
//!
//! Exactly two, both injected and both non-zero by construction:
//!
//! 1. a **connect deadline**, passed to `reqwest`'s `connect_timeout`;
//! 2. a **response-head deadline**, a `tokio::time::timeout` around the send
//!    future, which resolves when response headers arrive.
//!
//! They are named for what they bound and nothing more. `reqwest` 0.13.4
//! documents no DNS timeout, no TLS-handshake timeout and no header timeout,
//! and its `connect_timeout` doc does not state whether DNS or the handshake is
//! inside its window — so this module never calls either deadline a DNS or TLS
//! deadline. There is no total timeout and no read timeout: stream progress is
//! already owned by `StepPolicy::no_progress_timeout`, and a second per-read
//! bound here would be a competing carrier of one law.
//!
//! **No production numeric default is ratified.** [`CodexDeadlines`] has no
//! `Default`; a caller must supply both values, and the production numbers are
//! a deployment decision that must be measured rather than guessed into a
//! constant here.
//!
//! # Wire-shape provenance, stated exactly
//!
//! Every endpoint, header and event name below is read from the frozen
//! oracle at `42e4888cc0`, not from a live request:
//! The oracle's `openai-codex.ts` (base URL, the four
//! headers, OAuth-only credential shape),
//! The oracle's `model-resolver.ts:276-293` (the Codex route's
//! `supportsMaxOutputTokens: false`), the oracle's ``
//! open-responses.ts` (path `/responses`, the SSE event vocabulary, the
//! `incomplete_details.reason` finish mapping, the usage field names).
//!
//! **No live ChatGPT request has been made and no credential store has been
//! read**. The fixtures in this module are loopback bytes this module
//! authored; they are never described as a capture. Validating the mapping
//! against a real credential-backed transcript is a separately human-gated
//! pre-production residual.

use std::time::Duration;

// The included test modules construct leases, routes and endpoints directly;
// production code in this module names none of these.
#[cfg(test)]
use grapt_types::{AccountId, CredentialVersion, ModelRouteId, Secret};

#[cfg(test)]
use crate::fragment::{AdapterState, FinishReason};
pub use crate::transport::slot::CodexLimits;

// The Codex ROUTE is now one arm of the general transport: its body builder is
// `routes::body::codex_body`, its decoder is `routes::responses` (the Responses
// wire shape it shares with the public OpenAI route), its headers are assembled
// by `transport::adapter`, and its stream is `transport::stream`. What remains
// in this module is the configuration vocabulary those share.
//
// The included test modules below still drive this route end to end; they are
// the evidence that folding four adapters into one changed none of its
// behaviour.
#[cfg(test)]
use crate::routes::responses::{finish_reason_for, usage_from};
#[cfg(test)]
use crate::transport::sse::{SseDecodeError, SseDecoder};

/// A misconfiguration refused before any socket exists.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodexConfigError {
    #[error("the {which} deadline must be non-zero")]
    ZeroDeadline { which: &'static str },
    #[error("the user agent must not be empty")]
    UserAgentEmpty,
}

/// A fault in the harness, not in the provider.
///
/// Provider and transport faults are **not** here: they become a
/// [`crate::fragment::AttemptFragment::ProviderFailure`] so the runtime
/// classifies them through the one vocabulary that already owns retry
/// eligibility.
#[derive(Debug, thiserror::Error)]
pub enum CodexError {
    #[error(transparent)]
    Config(#[from] CodexConfigError),
    #[error("TLS configuration failed: {reason}")]
    Tls { reason: String },
    #[error("HTTP client construction failed: {reason}")]
    ClientBuild { reason: String },
    #[error("the request could not be serialized: {reason}")]
    RequestBuild { reason: String },
    /// Contract rule 1: one PhysicalAttempt performs exactly one provider
    /// stream call. An id that is **currently streaming** is refused rather
    /// than silently opening a second socket under one identity.
    #[error("attempt {attempt} is already streaming on this adapter")]
    DuplicateActiveAttempt { attempt: String },
    /// The adapter's injected active-attempt capacity is full. Typed, because
    /// a saturated adapter is an ordinary operating condition the caller must
    /// be able to distinguish from a provider fault.
    #[error("the adapter already holds its capacity of {capacity} active attempts")]
    ActiveAttemptCapacity { capacity: usize },
    /// The bounded FIFO cannot register another unresolved reservation.
    #[error("the provider slot wait queue already holds its limit of {max_waiters} waiters")]
    AttemptWaiterCapacity { max_waiters: usize },
    /// The active-attempt registry lock was poisoned by a panicking holder.
    /// Reported rather than unwrapped: this crate has no panicking path outside
    /// `#[cfg(test)]`.
    #[error("the active-attempt registry is poisoned")]
    RegistryPoisoned,
}

/// The two injected deadlines. No `Default`: there is no ratified production
/// number; callers must supply a measured limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexDeadlines {
    connect: Duration,
    response_head: Duration,
}

impl CodexDeadlines {
    /// Both bounds must be non-zero. A zero deadline is not "no deadline"; it
    /// is a request that can never succeed, and accepting it would turn a
    /// configuration mistake into an unexplained transport failure.
    pub fn new(connect: Duration, response_head: Duration) -> Result<Self, CodexConfigError> {
        if connect.is_zero() {
            return Err(CodexConfigError::ZeroDeadline { which: "connect" });
        }
        if response_head.is_zero() {
            return Err(CodexConfigError::ZeroDeadline {
                which: "response head",
            });
        }
        Ok(Self {
            connect,
            response_head,
        })
    }

    pub fn connect(self) -> Duration {
        self.connect
    }

    pub fn response_head(self) -> Duration {
        self.response_head
    }
}

#[cfg(test)]
mod tests {
    // These witnesses drove the Codex route when it had its own adapter. They
    // now drive the SAME route through the general transport, unchanged in what
    // they assert — which is what makes them the evidence that folding four
    // adapters into one changed none of this route's behaviour.
    use super::*;
    use crate::authority::{AttemptCustody, ProviderAttemptCarrier};
    use crate::credential::CredentialLease;
    use crate::fragment::{AttemptFragment, ProviderFailureClass};
    use crate::request::{ChatGptCodexRequest, ProviderRequest};
    use crate::route::ProviderEndpoint;
    use crate::routes::body::codex_body as codex_request_body;
    use crate::transport::adapter::{AttemptInput, ProviderAdapter, ProviderOpen};
    use crate::transport::classify::{classify_status, parse_retry_after};
    use crate::transport::client::{build_client, build_loopback_client};
    use crate::transport::stream::ProviderStream;
    use grapt_types::PhysicalAttemptId;

    /// The general adapter, built on a loopback client — the shape
    /// `CodexAdapter::loopback` had.
    fn loopback_adapter(
        deadlines: CodexDeadlines,
        limits: CodexLimits,
    ) -> Result<ProviderAdapter, CodexError> {
        Ok(ProviderAdapter::new(
            crate::transport::client::build_loopback_client,
            deadlines,
            limits.active_attempts(),
        ))
    }

    /// Decompose a bound carrier into the general adapter's input, the way
    /// `CodexAttemptInput::from_carrier` did.
    fn input_from_carrier(
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

    include!("codex/tests/support_and_units.rs");
    include!("codex/tests/transport_stream.rs");
    include!("codex/tests/utf8_and_framing.rs");
    include!("codex/tests/ordinals_capacity_and_eof.rs");
}
