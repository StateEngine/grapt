//! The application boundary's typed outcomes, one variant per answer a route owes.
//!
//! These sat inside the port module until they pushed it past its hard ceiling.
//! They are a vocabulary, not behaviour: naming each outcome here keeps the port
//! file about the contract, and makes the set of things a route may answer
//! visible in one place. A busy reader is `Busy`, not a bad request, and that
//! distinction is only legible where the whole vocabulary sits together.

use super::ApplicationUnavailable;
use crate::u3a::{
    ConflictErrorDto, DtoString, FormAlreadySettledErrorDto, FormInvalidAnswerErrorDto,
    FormNotFoundErrorDto, InvalidCursorErrorDto, InvalidRequestErrorDto, InvalidRequestErrorDtoTag,
    MessageNotFoundErrorDto, PermissionNotFoundErrorDto, PermissionNotLiveErrorDto,
    SessionNotFoundErrorDto, UnknownErrorDto,
};

#[derive(Debug)]
pub enum CatalogApplicationError {
    InvalidRequest(InvalidRequestErrorDto),
    Unavailable(ApplicationUnavailable),
}

#[derive(Debug)]
pub enum SessionListApplicationError {
    Unavailable,
    InvalidRequest(InvalidRequestErrorDto),
    InvalidCursor(InvalidCursorErrorDto),
}

#[derive(Debug)]
pub enum SessionLookupApplicationError {
    Unavailable,
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
}

/// The active-session read's typed outcomes.
///
/// A busy reader is not a bad request: it is a transient capacity wait, and it is
/// a distinct variant so no caller can turn it into a 400 by falling back to the
/// only error type it had. Phase 1's typed-outcome move names this shape.
#[derive(Debug)]
pub enum SessionActiveApplicationError {
    /// Reading the active session did not finish inside its reader-wait bound.
    Busy,
    /// The request itself was unacceptable.
    InvalidRequest(InvalidRequestErrorDto),
}

pub(crate) fn sandbox_worktree_config_unsupported() -> SessionLookupApplicationError {
    SessionLookupApplicationError::InvalidRequest(InvalidRequestErrorDto::new(
        grapt_schema::Required::new(InvalidRequestErrorDtoTag::InvalidRequestError),
        grapt_schema::Required::new(
            DtoString::try_new("sandbox worktree configuration is unsupported").unwrap_or_default(),
        ),
        grapt_schema::OptionalNullable::absent(),
        grapt_schema::OptionalNullable::absent(),
    ))
}

#[derive(Debug)]
pub enum SessionMessagesApplicationError {
    Unavailable,
    InvalidRequest(InvalidRequestErrorDto),
    InvalidCursor(InvalidCursorErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    Unknown(UnknownErrorDto),
}

#[derive(Debug)]
pub enum SessionMessageApplicationError {
    Unavailable,
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    MessageNotFound(MessageNotFoundErrorDto),
    Unknown(UnknownErrorDto),
}

#[derive(Debug)]
pub enum SessionPromptApplicationError {
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    Conflict(ConflictErrorDto),
}

/// `session.compact` admits exactly this refusal set: a body that does
/// not decode, an unknown Session, and a request that conflicts (an id
/// already used for another input, or a compaction that already ran).
#[derive(Debug)]
pub enum SessionCompactApplicationError {
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    Conflict(ConflictErrorDto),
}

#[derive(Debug)]
pub enum SessionRewindApplicationError {
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    Conflict(ConflictErrorDto),
    Unknown(UnknownErrorDto),
}
#[derive(Debug)]
pub enum SessionUndoApplicationError {
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    Conflict(ConflictErrorDto),
    Unknown(UnknownErrorDto),
}
/// `session.reconcile` refuses a body that does not decode, an unknown
/// Session, and a Session that is not idle; a failed commit is unknown.
#[derive(Debug)]
pub enum SessionReconcileApplicationError {
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    Conflict(ConflictErrorDto),
    Unknown(UnknownErrorDto),
}

// Four form error families rather than one, because the frozen contract
// gives each route a DIFFERENT refusal set and a shared enum would let a
// route answer with a refusal its own contract does not admit — cancel
// cannot reject an answer it was never given, and create cannot 404 a form
// it is about to mint.

#[derive(Debug)]
pub enum FormCreateApplicationError {
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    Conflict(ConflictErrorDto),
}

#[derive(Debug)]
pub enum FormLookupApplicationError {
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    FormNotFound(FormNotFoundErrorDto),
}

#[derive(Debug)]
pub enum FormReplyApplicationError {
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    FormNotFound(FormNotFoundErrorDto),
    FormAlreadySettled(FormAlreadySettledErrorDto),
    FormInvalidAnswer(FormInvalidAnswerErrorDto),
}

#[derive(Debug)]
pub enum FormCancelApplicationError {
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    FormNotFound(FormNotFoundErrorDto),
    FormAlreadySettled(FormAlreadySettledErrorDto),
}

#[derive(Debug)]
pub enum PermissionApplicationError {
    InvalidRequest(InvalidRequestErrorDto),
    SessionNotFound(SessionNotFoundErrorDto),
    PermissionNotFound(PermissionNotFoundErrorDto),
    PermissionNotLive(PermissionNotLiveErrorDto),
    Unavailable(ApplicationUnavailable),
}
