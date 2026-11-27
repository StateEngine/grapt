//! Native process observations are distinct from an already-settled tool call.
use crate::session_tool_terminal::{
    invalid, object, reject_unknown, required, required_str, PayloadError,
};
use grapt_types::{OperationId, SessionId};
use serde_json::Value;

pub const KIND: &str = "session.terminal.observed";
const AT: &str = "session.terminal.observed.1";
pub const MAX_ID_BYTES: usize = 512;
const MAX_REASON: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalState {
    Starting,
    Exited { code: i32 },
    TimedOut,
    Cancelled,
    NotStarted { reason: String },
    Lost { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFact {
    session: SessionId,
    operation: OperationId,
    state: TerminalState,
    cleanup_error: Option<String>,
}
impl TerminalFact {
    pub fn new(
        session: SessionId,
        operation: OperationId,
        state: TerminalState,
        cleanup_error: Option<String>,
    ) -> Result<Self, PayloadError> {
        if session.as_str().len() > MAX_ID_BYTES || operation.as_str().len() > MAX_ID_BYTES {
            return Err(invalid(AT, "identity", "identities of at most 512 bytes"));
        }
        let reason = match &state {
            TerminalState::NotStarted { reason } | TerminalState::Lost { reason } => Some(reason),
            _ => None,
        };
        for reason in reason.into_iter().chain(cleanup_error.as_ref()) {
            if reason.trim().is_empty() || reason.len() > MAX_REASON {
                return Err(invalid(AT, "reason", "nonblank text within 8 KiB"));
            }
        }
        if state == TerminalState::Starting && cleanup_error.is_some() {
            return Err(invalid(
                AT,
                "cleanupError",
                "absent before process creation",
            ));
        }
        Ok(Self {
            session,
            operation,
            state,
            cleanup_error,
        })
    }
    pub fn session(&self) -> &SessionId {
        &self.session
    }
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }
    pub fn state(&self) -> &TerminalState {
        &self.state
    }
    pub fn cleanup_error(&self) -> Option<&str> {
        self.cleanup_error.as_deref()
    }
}

fn encode_state(state: &TerminalState) -> Value {
    match state {
        TerminalState::Starting => serde_json::json!({"kind":"starting"}),
        TerminalState::Exited { code } => serde_json::json!({"kind":"exited", "code":code}),
        TerminalState::TimedOut => serde_json::json!({"kind":"timed-out"}),
        TerminalState::Cancelled => serde_json::json!({"kind":"cancelled"}),
        TerminalState::NotStarted { reason } => {
            serde_json::json!({"kind":"not-started", "reason":reason})
        }
        TerminalState::Lost { reason } => serde_json::json!({"kind":"lost", "reason":reason}),
    }
}

pub fn encode(fact: &TerminalFact) -> Value {
    let mut value = serde_json::json!({
        "sessionID":fact.session.as_str(), "operationID":fact.operation.as_str(),
        "state":encode_state(&fact.state)
    });
    if let Some(error) = &fact.cleanup_error {
        value["cleanupError"] = error.clone().into();
    }
    value
}

fn decode_state(value: &Value) -> Result<TerminalState, PayloadError> {
    let root = object(value, AT)?;
    let kind = required_str(root, AT, "kind")?;
    let fields: &[&str] = match kind {
        "starting" | "timed-out" | "cancelled" => &["kind"],
        "exited" => &["kind", "code"],
        "not-started" | "lost" => &["kind", "reason"],
        _ => return Err(invalid(AT, "state", "a known process state")),
    };
    reject_unknown(root, AT, fields)?;
    Ok(match kind {
        "starting" => TerminalState::Starting,
        "timed-out" => TerminalState::TimedOut,
        "cancelled" => TerminalState::Cancelled,
        "exited" => TerminalState::Exited {
            code: required(root, AT, "code")?
                .as_i64()
                .and_then(|code| i32::try_from(code).ok())
                .ok_or_else(|| invalid(AT, "code", "a signed 32-bit process exit status"))?,
        },
        "not-started" => TerminalState::NotStarted {
            reason: required_str(root, AT, "reason")?.into(),
        },
        "lost" => TerminalState::Lost {
            reason: required_str(root, AT, "reason")?.into(),
        },
        _ => return Err(invalid(AT, "state", "a known process state")),
    })
}

pub fn decode(value: &Value) -> Result<TerminalFact, PayloadError> {
    let root = object(value, AT)?;
    reject_unknown(
        root,
        AT,
        &["sessionID", "operationID", "state", "cleanupError"],
    )?;
    let session = required_str(root, AT, "sessionID")?
        .parse::<SessionId>()
        .map_err(|_| invalid(AT, "sessionID", "a session identity"))?;
    let operation = OperationId::new(required_str(root, AT, "operationID")?)
        .ok_or_else(|| invalid(AT, "operationID", "an operation identity"))?;
    let cleanup_error = root
        .get("cleanupError")
        .map(|value| {
            value
                .as_str()
                .map(String::from)
                .ok_or_else(|| invalid(AT, "cleanupError", "absent or nonblank text"))
        })
        .transpose()?;
    TerminalFact::new(
        session,
        operation,
        decode_state(required(root, AT, "state")?)?,
        cleanup_error,
    )
}
