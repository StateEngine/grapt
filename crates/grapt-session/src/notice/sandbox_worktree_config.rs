//! Session-scoped sandbox compatibility warnings, never provider context or grants.
use super::*;

pub(super) const SANDBOX_WORKTREE_CONFIG: &str = "sandbox-worktree-config";
const MAX_TEXT: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxWorktreeConfigNotice {
    pub position: CommittedPosition,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    kind: String,
    text: String,
}

fn validate(data: &Data, session: &SessionId) -> Result<(), String> {
    if data.session != session.as_str()
        || data.kind != SANDBOX_WORKTREE_CONFIG
        || data.text.is_empty()
        || data.text.len() > MAX_TEXT
        || data.text.chars().any(char::is_control)
    {
        return Err("sandbox worktree config notice is invalid".into());
    }
    Ok(())
}

/// Record only a changed warning or its recovery. An empty initial state is silent.
pub fn record_sandbox_worktree_config(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    text: Option<&str>,
) -> Result<Option<SandboxWorktreeConfigNotice>, String> {
    let last = at(store.connection(), session, None)?.pop();
    let text = match (text, &last) {
        (None, None) => return Ok(None),
        (None, Some(_)) => "Windows sandbox Git worktree config warning cleared after re-check; the deny policy is unchanged.",
        (Some(text), _) => text,
    };
    if last.as_ref().is_some_and(|last| last.text == text) {
        return Ok(None);
    }
    let data = Data {
        session: session.as_str().into(),
        kind: SANDBOX_WORKTREE_CONFIG.into(),
        text: text.into(),
    };
    validate(&data, session)?;
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate).map_err(|e| e.to_string())?;
    let tx = store.governed_transaction().map_err(|e| e.to_string())?;
    SessionStore::new(&tx)
        .record(session)
        .map_err(|e| e.to_string())?;
    let position = append_in(
        &tx,
        aggregate,
        writer,
        authorities,
        &serde_json::to_value(&data).map_err(|e| e.to_string())?,
    )?;
    tx.commit().map_err(|e| e.to_string())?.discard();
    Ok(Some(SandboxWorktreeConfigNotice {
        position,
        text: text.into(),
    }))
}

pub(super) fn at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    seq: Option<u64>,
) -> Result<Vec<SandboxWorktreeConfigNotice>, String> {
    kind_rows_at::<Data>(connection, session, SANDBOX_WORKTREE_CONFIG, seq)?
        .into_iter()
        .map(|(position, data)| {
            validate(&data, session)?;
            Ok(SandboxWorktreeConfigNotice {
                position,
                text: data.text,
            })
        })
        .collect()
}
