//! The execution registry confirms whether a journaled active owner still exists.
use crate::dogfood::execution::LiveActiveExecutions;
use crate::dogfood::session_status::ConfirmationError;
use grapt_types::{ExecutionId, SessionId};
use serde_json::{Map, Value};

// Absence, unreadable storage and a changed live owner remain separate facts.
enum ReadError {
    Unreadable(String),
    Unconfirmed(ConfirmationError),
    Encoding(String),
}

fn session_state(
    active: &LiveActiveExecutions,
    connection: &rusqlite::Connection,
    session: &SessionId,
    expected: Option<&ExecutionId>,
) -> Result<grapt_api::u3a::SessionStateDto, ReadError> {
    let snapshot = grapt_service_state::read(connection, session).map_err(ReadError::Unreadable)?;
    active
        .validate_state(session, &snapshot)
        .map_err(ReadError::Unconfirmed)?;
    if expected.is_some_and(|expected| snapshot.execution.as_ref() != Some(expected)) {
        return Err(ReadError::Unconfirmed(ConfirmationError::OwnerChanged));
    }
    crate::dogfood::session_status::state_wire(snapshot).map_err(ReadError::Encoding)
}

pub(crate) fn fields(
    active: &LiveActiveExecutions,
    connection: &rusqlite::Connection,
    session: &SessionId,
    expected: Option<&ExecutionId>,
) -> Map<String, Value> {
    let result = session_state(active, connection, session, expected).and_then(|state| {
        serde_json::to_value(state).map_err(|error| ReadError::Encoding(error.to_string()))
    });
    let (key, value) = match result {
        Ok(state) => ("state", state),
        Err(error) => ("stateError", error_value(session, error)),
    };
    [(key.to_owned(), value)].into_iter().collect()
}

fn error_value(session: &SessionId, error: ReadError) -> Value {
    use grapt_api::u3a::SessionStateErrorCode as Code;
    let (code, message) = match error {
        ReadError::Unreadable(detail) => {
            crate::server_log::warn(format_args!(
                "session {session}: status read unavailable: {detail}"
            ));
            (Code::Unreadable, "The committed session status is unreadable; reconnect to retry after the service recovers.")
        }
        ReadError::Encoding(detail) => {
            crate::server_log::warn(format_args!(
                "session {session}: status encoding unavailable: {detail}"
            ));
            (
                Code::Encoding,
                "Session status could not be encoded; reconnect and retry.",
            )
        }
        ReadError::Unconfirmed(ConfirmationError::OwnerChanged) => (
            Code::OwnerChanged,
            "The current execution owner changed; refresh session status.",
        ),
        ReadError::Unconfirmed(ConfirmationError::OwnerGone) => (
            Code::OwnerGone,
            "The recorded execution has no live owner; reconnect to reconcile session status.",
        ),
        ReadError::Unconfirmed(ConfirmationError::CommitUnavailable) => (
            Code::CommitUnavailable,
            "Session status could not be committed; reconnect to retry after the service recovers.",
        ),
    };
    serde_json::json!({"code":code, "message":message})
}
