//! The real U3a DTOs, encoded and decoded through the real codecs.
//!
//! ## Why this module exists
//!
//! The previous revision of this crate depended on `grapt-api` and then never
//! referenced it. It hand-rolled a parallel JSON vocabulary — `{"sessions": …}`
//! for the session list, `{"messages": …}` for the re-list — and because the
//! contract double agreed with the client on that invention, **every in-crate
//! test passed while the client was incompatible with the admitted U3a
//! surface**. A dependency-direction gate cannot catch that: it proves an edge
//! exists in the manifest, not that anything crosses it.
//!
//! The real shapes are `SessionsResponseDto { data, cursor }` and
//! `SessionMessagesResponseDto { data, cursor }`. Both are `closed_object!`s
//! whose fields are `grapt_schema` presence types, so nothing here can drift
//! from the contract without failing to compile.
//!
//! Every encode goes through [`grapt_api::u3a::encode_page`] and every decode
//! through [`grapt_api::u3a::decode_page`] for responses and
//! [`grapt_api::u3a::decode_request`] for requests, under limits from
//! [`grapt_api::u3a::U3aLimits::production`]. Both directions are exercised in
//! this crate's witnesses, so an invented key cannot survive: it fails to
//! decode as the real DTO.
//!
//! ## Identity is typed
//!
//! Session identity is `WireId<grapt_types::SessionId>`, parsed once at the
//! wire edge, never a raw `String` in a map key. A client that keyed on
//! `String` could hold two spellings of one session and render them as two.

use grapt_api::u3a::{
    decode_page, decode_request, encode_page, BoundedVec, DtoCodecError, DtoString, FiniteF64,
    U3aLimits, WireId,
};
use grapt_schema::{Optional, OptionalNullable, Required};

use thiserror::Error;

mod claude_plugin;
pub use claude_plugin::*;
mod cache;
mod integration;
mod status;
pub use status::decode_provider_account;
mod workspace_diff;
pub use workspace_diff::*;
mod worktree;
pub use worktree::*;
mod sandbox_worktree_config;
pub use sandbox_worktree_config::*;

mod mcp_inspect;
pub use mcp_inspect::*;

/// The session identity this client uses everywhere.
///
/// This alias is the whole of review blocker 1. It is not a convenience: it is
/// the only spelling of a session identity anywhere in this crate. There is no
/// `String` session id in a struct field, a map key, an `Option`, a function
/// parameter or a projection, so a client CANNOT hold two spellings of one
/// session and render them as two, and cannot admit an identity the domain
/// refuses. `tests/typed_identity.rs` is the structural gate that keeps it that
/// way after this commit, because a type discipline nobody mechanically checks
/// is a type discipline that decays at the next edit.
pub type SessionWireId = WireId<grapt_types::SessionId>;

/// The event identity carried by every frame's `id`.
pub type EventWireId = WireId<grapt_types::EventId>;

/// The execution identity carried by the `session.execution.*` members.
pub type ExecutionWireId = WireId<grapt_types::ExecutionId>;

/// The permission identity carried from list through reply without flattening.
pub type PermissionWireId = WireId<grapt_types::PermissionId>;
/// One tool call's identity, kept TYPED so reconnect deduplication compares
/// identities rather than strings.
pub type ToolCallWireId = WireId<grapt_types::ToolCallId>;
/// A durable message's identity. Kept for user rows: the newest one names the
/// turn a history read ends on, which a count of folded rows cannot once the
/// read covers only a window.
pub type MessageWireId = WireId<grapt_types::MessageId>;

/// The wire spelling of a typed identity, for RENDERING only.
///
/// ## Why this is a trait and not a `Display` impl
///
/// `WireId<T>` is a foreign type, so this crate cannot give it `Display` — but
/// that constraint turns out to be the right answer rather than a workaround.
/// A `Display` impl would make `format!("{id}")` compile everywhere, and
/// "everywhere" includes the places that should be comparing identities rather
/// than printing them. That is exactly how the previous revision flattened:
/// once an identity renders as easily as a string, it starts being carried as
/// one.
///
/// So spelling an identity is a deliberate call with a name that says what it
/// is for. It appears in `view.rs` — which paints characters on a terminal and
/// legitimately needs text — in the double's JSON encoding, and nowhere that
/// makes a decision. `tests/typed_identity.rs` gates that.
pub trait Spelled {
    /// The identity's exact wire spelling.
    fn spelled(&self) -> &str;
}

/// The typed identity behind a wire value, for the seams that HAND IT ON
/// rather than render it.
///
/// `Spelled` is the route to text and is confined to the modules that paint or
/// encode. A seam that speaks the domain's own type — a subscription naming its
/// Session — needs the identity, not its spelling, so it uses this instead of
/// reaching through `as_inner` and building a `String` nobody asked for. Like
/// `Spelled`, it lives here because this module owns the seam; unlike it, the
/// result is still typed, so nothing downstream can flatten it by accident.
pub trait Typed {
    /// The domain identity this wire value carries.
    type Id: Clone;

    /// The identity itself, not a spelling of it.
    fn typed(&self) -> Self::Id;
}

impl Typed for SessionWireId {
    type Id = grapt_types::SessionId;

    fn typed(&self) -> Self::Id {
        self.as_inner().clone()
    }
}

impl Spelled for SessionWireId {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

impl Spelled for EventWireId {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

impl Spelled for ExecutionWireId {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

impl Spelled for ToolCallWireId {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

impl Spelled for PermissionWireId {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

impl Spelled for WireId<grapt_types::OperationId> {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}

/// Why a wire value could not be produced or read.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WireError {
    #[error("codec refused the payload: {reason}")]
    Codec { reason: String },
    #[error("value `{value}` is not a valid {what}")]
    Invalid { what: &'static str, value: String },
}

impl From<DtoCodecError> for WireError {
    fn from(error: DtoCodecError) -> Self {
        WireError::Codec {
            reason: format!("{error:?}"),
        }
    }
}

/// The production limit set, constructed once per call site.
pub fn limits() -> Result<U3aLimits, WireError> {
    U3aLimits::production().map_err(|error| WireError::Codec {
        reason: format!("{error:?}"),
    })
}

fn dto_string(value: &str) -> Result<DtoString, WireError> {
    DtoString::try_new(value).map_err(|reason| WireError::Invalid {
        what: "bounded string",
        value: format!("{value}: {reason}"),
    })
}

fn finite(value: f64) -> Result<FiniteF64, WireError> {
    FiniteF64::try_new(value).map_err(|reason| WireError::Invalid {
        what: "finite number",
        value: format!("{value}: {reason}"),
    })
}

/// Parse a session id at the wire edge, once.
///
/// `WireId::parse` delegates to `grapt_types::SessionId`'s own `FromStr`, so
/// the law applied here is the RATIFIED domain identity — `ses_` followed by a
/// non-empty `[A-Za-z0-9_]` suffix — and not a prefix test written out again in
/// this crate.
///
/// ## Why the authoritative newtype outranks the frozen pattern
///
/// The frozen compatibility record spells the session id pattern `^ses`: three
/// characters, no underscore. The previous revision honoured exactly that with
/// `starts_with("ses")` and even carried a positive test asserting that `sesABC`
/// is accepted. That was the defect review named, and it is worth being precise
/// about WHY, because the two authorities really do disagree and the disagreement
/// is not symmetric.
///
/// `^ses` is the WEAKER of the two. Every id the domain can represent matches
/// it, but it also matches ids the domain cannot represent at all — `sesABC` is
/// one, and `grapt_types::SessionId` refuses it. So honouring the looser
/// pattern does not buy compatibility; it buys a client that accepts an
/// identity it can never key, compare or hand back, and that fails later and
/// somewhere else. Parsing through the newtype refuses it at the edge instead,
/// which is the only place the refusal is cheap and legible.
pub fn session_id(value: &str) -> Result<SessionWireId, WireError> {
    WireId::parse(value.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "session id",
        value: value.to_owned(),
    })
}

/// Parse one tool call identity at the edge, like every other identity here.
pub fn tool_call_id(value: &str) -> Result<ToolCallWireId, WireError> {
    WireId::parse(value.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "tool call id",
        value: value.to_owned(),
    })
}

pub fn permission_id(value: &str) -> Result<PermissionWireId, WireError> {
    WireId::parse(value.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "permission id",
        value: value.to_owned(),
    })
}

mod catalogs;
mod commands;
pub use commands::*;
mod forms;
mod skills_changed;
pub use skills_changed::skills_changed;
mod history_page;
mod messages;
pub use history_page::MessagePage;
mod permissions;
mod presets;
mod rewind;
mod undo;
pub use undo::{
    decode_undo_conflict_message, decode_undo_preview, decode_undone, encode_undo, UndoReceipt,
    UndoSelection,
};
mod sessions;
pub use rewind::*;

pub use catalogs::*;
pub use forms::*;
pub use messages::*;
pub use permissions::*;
pub use presets::*;
pub use sessions::*;

#[cfg(test)]
mod tests;

mod submission;
pub use submission::queued_inputs;
pub use submission::QueuedInputPreview;
pub use submission::{history_holds_input, pending_holds_input, SubmissionOrigin};
