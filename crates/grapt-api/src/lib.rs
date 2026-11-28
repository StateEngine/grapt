//! The transport trust boundary: sealed principal, server-owned mapping,
//! per-action authorization, bounded takeover, deterministic route composition.
//!
//! Law source: `specs/secode-rust-reconstruction-master-r1-2026-08-08.md` line-zero row
//! L245-247 (`grapt-api` owns "versioned HTTP/SSE compatibility DTOs, auth, cursors and
//! server"), C2 (L2942 — "the answering transport supplies an authenticated client
//! identity; callers cannot self-assert it"), C3 (L2943 — the principal vocabulary is
//! typed and closed) and F4 (L2991 — send/wake/interrupt requires a capability naming
//! allowed recipients and actions). The API policy closes these boundaries.
//!
//! # The trust flow
//!
//! ```text
//! untrusted request bytes
//!   -> bounded DTO decode                  (no identity read)
//!   -> credential adapter                  (crate-private; NO backend in this lane)
//!   -> VerifiedSubject                     (sealed proof; no public mint)
//!   -> SeatPrincipalDirectory::resolve
//!        -> (SeatRecipientId, AuthenticatedClientId, DirectoryGeneration)
//!   -> AuthenticatedSeatPrincipal          (sealed; no public mint)
//!   -> AuthorizationDecision               for THIS action on THIS resource
//!   -> application call
//! ```
//!
//! Every arrow is a place a request can stop, and the DTO does not appear after
//! the first one.
//!
//! # What this crate is not
//!
//! Bind intent and listener acquisition remain owned by `grapt-location`; this
//! crate consumes the transferred listener and owns the bounded HTTP/1 host.
//! IA0-1R adds exactly one product route, authenticated `GET /api/health`, with
//! no `/global/health` alias and no second product module. It writes no durable
//! row, emits no event, and touches no store, mailbox, permission or capability
//! settlement.
//!
//! It also **mints no permission authority**. Ruling R4HA-04 and answer Q1: A
//! is the sole issuer of [`grapt_types::AuthenticatedClientId`], embeds it only
//! inside [`auth::AuthenticatedSeatPrincipal`], and ships no conversion toward
//! `LivePermissionDecision`, `CapabilityGrant`, `SessionGrant` or
//! `AuthorityLease`. Under answer Q2 the two principal namespaces are
//! permanently separate: there is no conversion in either direction between
//! [`auth::AuthenticatedSeatPrincipal`] and
//! [`grapt_types::AuthorityPrincipal`], and a compile-fail probe holds it.
//!
//! # Nothing here persists
//!
//! No lease, no takeover, no directory entry and no principal survives the
//! process. That is what makes "process-local" a property rather than a wish.

pub mod admission;
pub mod auth;
pub mod authorization;
pub mod deadline;
pub mod directory;
pub mod dto;
pub mod error;
pub mod host;
pub mod http1;
pub mod limits;
pub mod product_health;
pub mod router;
mod stream;
pub mod takeover;
pub mod u3a;

#[cfg(test)]
mod witness;

/// The R4H-H host witnesses that need a server-derived principal.
///
/// In `src/` rather than `tests/` for the reason `witness` is: A's fixture
/// verification route is `#[cfg(test)]`, so it compiles into this crate's own
/// library test binary and an integration test — a separate crate — cannot
/// reach it. A host witness that must show a sealed principal arriving at a
/// route therefore has to live here. The host witnesses that need no principal
/// live in `tests/`, where they belong.
#[cfg(test)]
mod host_witness;

pub use auth::{
    AuthenticatedSeatPrincipal, AuthenticationPolicyVersion, PresentedCredential,
    SeatTrustBoundary, SubjectId, SubjectReceipt, VerifiedSubject, MAX_SUBJECT_ID_BYTES,
};
pub use authorization::{
    authorize, govern, AuthorizationDecision, AuthorizationRequest, SeatApplication,
    SeatCapabilityProof,
};
pub use directory::{
    DirectoryGeneration, MaxPrincipalMappings, SeatDirectoryEntry, SeatPrincipalDirectory,
};
pub use dto::{decode, decode_with_recipient, MaxRequestDtoBytes, RequestEnvelope};
pub use error::{ApiRefusal, Redacted, RoutePathDefect};
pub use router::{
    ApiRouter, ApiRouterBuilder, MaxRouterModules, RouteKey, RouteMethod, RoutePath,
    MAX_ROUTE_PATH_BYTES, MAX_ROUTE_SEGMENTS,
};
pub use takeover::{
    Acquisition, Displacement, LeaseGeneration, MaxTakeoverSlots, RecipientConnectionRegistry,
    RecipientLease,
};

// ---------------------------------------------------------------------------
// R4H-H: the bounded HTTP/1 host.
//
// Narrow on purpose. The host, its limits and its terminal receipt are public
// because an owner has to configure and stop one; the connection permits, the
// per-connection budget, the counters, the accept source and the served body
// composition are not, because nothing outside this crate composes them and a
// public seam is a second way to build a host. Under I1/I2 none of these is a
// generated DTO: they are private transport types, and a later versioned client
// surface will describe routes, not these.
// ---------------------------------------------------------------------------

pub use admission::{PermitBody, RequestRefusal};
pub use deadline::{BoxedDeadline, DeadlineDriver, HostTimer, TokioDeadlineDriver};
pub use host::{
    AcceptFailure, ApiHostExit, ApiHostReady, ConnectionFailureClass, HostFailureCause,
    HostFailureStage, PreparedApiHost, RootLoss, RunningApiHost,
};
pub use http1::{AuthenticatedApiService, SealedPrincipal, SEAT_CREDENTIAL_HEADER};
pub use limits::{
    ApiHostConfigError, ApiHostLimits, HeaderReadAllowance, MaxConnections, MaxHeaders,
    MaxInflightRequests, MaxInflightStreams, MaxRequestBodyBytes, MaxRequestHeadBytes,
    MaxRequestsPerConnection, ShutdownDrainAllowance, StreamWriteAllowance,
    HYPER_MINIMUM_MAX_BUFFER_SIZE,
};
pub use product_health::{
    register_product_health, register_product_health_with_reconciles, ProductHealth,
    ProductHealthError, PRODUCT_HEALTH_PATH,
};
pub use u3a::{
    decode_page, decode_request, encode_page, register_capture_event_route,
    register_dogfood_event_route, register_dogfood_routes, register_ephemeral_dogfood_routes,
    ApplicationFuture, ApplicationPort, ApplicationRequest, ApplicationUnavailable, ApprovalWatch,
    CaptureApplicationPort, CapturePage, CapturePageError, CapturePageRow, CaptureStreamTerminal,
    CatalogApplicationError, DogfoodEvent, DogfoodEventHub, DogfoodEventSubscription,
    DogfoodRouteRegistrationError, DtoCodecError, InterruptRequestDto, MessageListQueryDto,
    PageOrderDto, PermissionListResponseDto, PromptRequestDto, PromptResponseDto, PromptText,
    ProvisionalBlock, ProvisionalChange, ProvisionalKind, ProvisionalText, PublishReceipt,
    ReconcileCounts, ReconcileReason, SandboxWorktreeConfigDataDto,
    SandboxWorktreeConfigRequestDto, SandboxWorktreeConfigResponseDto, SessionActiveResponseDto,
    SessionCreateRequestDto, SessionInfoDto, SessionListApplicationError, SessionListQueryDto,
    SessionLookupApplicationError, SessionMessagesApplicationError, SessionMessagesResponseDto,
    SessionPromptApplicationError, SessionResponseDto, SessionsResponseDto, SseError,
    SseLimitValues, SseLimits, StreamEnd, StreamEndCause, StreamEndObserver, U3aLimitValues,
    U3aLimits, U3aLimitsError, WindowWatch, WireId, CAPTURE_EVENT_PATH, CAPTURE_PROTOCOL_VERSION,
    DOGFOOD_EVENT_PATH, DOGFOOD_NON_SSE_ROUTE_COUNT, FROZEN_OPENAPI_SHA256, FROZEN_SCHEMA_BYTES,
    FROZEN_SCHEMA_COUNT, MAX_SSE_FRAME_BYTES, NAMED_SCHEMA_CODECS, NAMED_SCHEMA_MANIFEST,
    OPTIONAL_NULLABLE_CARRIERS, U3A_DTO_VERSION, U3A_PRODUCTION_LIMITS,
};
