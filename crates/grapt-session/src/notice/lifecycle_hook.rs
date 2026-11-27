//! Bounded advisory lifecycle-hook results; never permission grants.
use super::*;

pub(super) const LIFECYCLE_HOOK: &str = "lifecycle-hook";
pub const MAX_HOOK_RESULT_BYTES: usize = 16 * 1024;

/// Closed lifecycle vocabulary shared by configuration, execution and history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LifecycleEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
    Notification,
    SessionEnd,
    PreCompact,
    PostCompact,
    SubagentStart,
    SubagentStop,
    PermissionRequest,
    WaitingForInput,
}
impl LifecycleEvent {
    pub const ALL: [Self; 13] = [
        Self::SessionStart,
        Self::UserPromptSubmit,
        Self::PreToolUse,
        Self::PostToolUse,
        Self::Stop,
        Self::Notification,
        Self::SessionEnd,
        Self::PreCompact,
        Self::PostCompact,
        Self::SubagentStart,
        Self::SubagentStop,
        Self::PermissionRequest,
        Self::WaitingForInput,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
            Self::Notification => "Notification",
            Self::SessionEnd => "SessionEnd",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::SubagentStart => "SubagentStart",
            Self::SubagentStop => "SubagentStop",
            Self::PermissionRequest => "PermissionRequest",
            Self::WaitingForInput => "WaitingForInput",
        }
    }
    pub fn parse(name: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|event| event.as_str() == name)
            .ok_or_else(|| "hooks contains an unknown lifecycle event".to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleHookScope {
    Session,
    Execution(ExecutionId),
    Input(MessageId),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleHookNotice {
    pub position: CommittedPosition,
    pub scope: LifecycleHookScope,
    pub event: LifecycleEvent,
    pub text: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID", skip_serializing_if = "Option::is_none")]
    execution: Option<String>,
    #[serde(rename = "messageID", skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(rename = "sessionOnly", default, skip_serializing_if = "is_false")]
    session_only: bool,
    kind: String,
    event: LifecycleEvent,
    text: String,
}
fn is_false(value: &bool) -> bool {
    !*value
}

fn scope(data: &Data) -> Result<LifecycleHookScope, String> {
    match (&data.execution, &data.message, data.session_only) {
        (None, None, true) => Ok(LifecycleHookScope::Session),
        (Some(execution), None, false) => ExecutionId::new(execution)
            .map(LifecycleHookScope::Execution)
            .ok_or("invalid hook notice execution".into()),
        (None, Some(message), false) => message
            .parse::<MessageId>()
            .map(LifecycleHookScope::Input)
            .map_err(|_| "invalid hook notice input".into()),
        _ => Err("hook notice requires an explicit Session scope or exactly one execution/input coordinate".into()),
    }
}
fn validate(data: &Data, session: &SessionId) -> Result<(), String> {
    scope(data)?;
    if data.session != session.as_str()
        || data.kind != LIFECYCLE_HOOK
        || data.text.is_empty()
        || data.text.len() > MAX_HOOK_RESULT_BYTES
        || data.text.contains('\0')
    {
        return Err("lifecycle hook notice identity or payload is invalid".into());
    }
    Ok(())
}

/// Uses the already elected Session writer. A result cannot grant authority or
/// alter execution outcomes. Empty successful hook output needs no notice.
pub fn record_lifecycle_hook(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    event: LifecycleEvent,
    text: &str,
) -> Result<LifecycleHookNotice, String> {
    record(
        store,
        authorities,
        session,
        LifecycleHookScope::Execution(execution.clone()),
        event,
        text,
    )
}

/// A pre-prompt plugin operation is attributed to the input that requested it,
/// not to a fabricated execution or an unrelated historical turn.
pub fn record_input_lifecycle_hook(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    input: &MessageId,
    event: LifecycleEvent,
    text: &str,
) -> Result<LifecycleHookNotice, String> {
    record(
        store,
        authorities,
        session,
        LifecycleHookScope::Input(input.clone()),
        event,
        text,
    )
}

/// An actual Session wait without a prompt or execution keeps its own scope.
pub fn record_session_lifecycle_hook(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    event: LifecycleEvent,
    text: &str,
) -> Result<LifecycleHookNotice, String> {
    record(
        store,
        authorities,
        session,
        LifecycleHookScope::Session,
        event,
        text,
    )
}

fn record(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    scope: LifecycleHookScope,
    event: LifecycleEvent,
    text: &str,
) -> Result<LifecycleHookNotice, String> {
    let (execution, message) = match &scope {
        LifecycleHookScope::Session => (None, None),
        LifecycleHookScope::Execution(id) => (Some(id.as_str().into()), None),
        LifecycleHookScope::Input(id) => (None, Some(id.as_str().into())),
    };
    let data = Data {
        session: session.as_str().into(),
        execution,
        message,
        session_only: scope == LifecycleHookScope::Session,
        kind: LIFECYCLE_HOOK.into(),
        event,
        text: text.into(),
    };
    validate(&data, session)?;
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate).map_err(|e| e.to_string())?;
    let tx = store.governed_transaction().map_err(|e| e.to_string())?;
    SessionStore::new(&tx)
        .record(session)
        .map_err(|e| e.to_string())?;
    let value = serde_json::to_value(&data).map_err(|e| e.to_string())?;
    let position = append_in(&tx, aggregate, writer, authorities, &value)?;
    tx.commit().map_err(|e| e.to_string())?.discard();
    Ok(LifecycleHookNotice {
        position,
        scope,
        event,
        text: text.into(),
    })
}

pub fn lifecycle_hook_notices(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<LifecycleHookNotice>, String> {
    at(connection, session, None)
}
pub(super) fn at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    seq: Option<u64>,
) -> Result<Vec<LifecycleHookNotice>, String> {
    kind_rows_at::<Data>(connection, session, LIFECYCLE_HOOK, seq)?
        .into_iter()
        .map(|(position, data)| {
            validate(&data, session)?;
            Ok(LifecycleHookNotice {
                position,
                scope: scope(&data)?,
                event: data.event,
                text: data.text,
            })
        })
        .collect()
}
