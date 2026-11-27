//! Effective-authority foundation (`grapt-policy`).
//!
//! Root correction #4: config, credential, model/provider, plugin/MCP,
//! environment, and tool visibility produce ONE immutable, receipted graph per
//! physical attempt. This crate owns:
//!
//! - a versioned, domain-separated, length-delimited canonical encoding
//!   (crate-private) whose adversary is hash collision, not oracle byte-parity;
//! - a [`Secret`] type that yields NEITHER material nor a fingerprint. This
//!   line used to promise "only a fingerprint, never material", which had
//!   become exactly backwards: `fingerprint()` was a public raw SHA-256 of the
//!   secret — a stable offline equality oracle — and amendment 8.4 forbids it.
//!   It was removed; the doc had not caught up;
//! - an injected, owner-held [`ContributionAuthority`] (see [`authority`]) that
//!   mints opaque [`ContributionId`]s for the two secret-bearing contributor
//!   classes. It replaces the public unkeyed digests those classes used to
//!   carry, which were the same offline oracle one level down: `Digest` no
//!   longer leaves this crate at all;
//! - the typed [`contributor`]s (model, config provenance, credential
//!   record/version, plugin/MCP generations, permission decisions,
//!   capability-scoped tool visibility, keyed environment exposure);
//! - an immutable [`PhysicalAttemptAuthoritySnapshot`] and its content-addressed
//!   [`EffectiveGraphReceipt`], keyed by the existing
//!   [`grapt_types::EffectiveGraphId`]. The receipt exposes the WHOLE identity
//!   and digest and nothing beneath them.
//!
//! It is a foundation crate: it depends only on `grapt-types` (and, when it
//! exists, `grapt-schema`) — never upward on store/event/provider/runtime. The
//! receipt is built to be sufficient for a later `AttemptSource` integration; no
//! runtime wiring lives here.

pub mod authority;
pub mod contributor;
mod encoding;
mod graph;
mod prefix_identity;
mod secret;

pub use authority::{AuthorityRealm, ContributionAuthority, ContributionId, KeyVersion};
pub use contributor::{
    ConfigLayer, ConfigSource, CredentialFingerprint, CustomSourceName, EnvExposure, EnvVarName,
    PermissionDecision, PermissionScope, ServiceActivation, ServiceKind, ToolName, ToolVisibility,
};
pub use graph::{
    EffectiveGraphReceipt, PhysicalAttemptAuthoritySnapshot, PolicyError, SnapshotBuilder,
};
pub use prefix_identity::{
    PrefixIdentityAuthority, PrefixIdentityError, PreparedPrefixPart, VerifiedPrefixComparison,
};
pub use secret::Secret;

// Re-export the model coordinate type so a caller building a graph does not
// need a direct grapt-types import for the model contributor.
pub use grapt_types::ModelRef;
