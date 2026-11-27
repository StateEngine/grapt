//! Durable user-facing notices about a Session's turns. Three kinds: the
//! MCP servers (or the MCP configuration) a turn could not use; a prompt
//! that was admitted but not run yet because something blocked its turn
//! before it began (it stays queued for the Session's next turn); an
//! execution that stopped because the model kept repeating what its context
//! already summarized until the context was full; and a config key the
//! Session's configuration sets that SE accepts but does not apply, named
//! once per Session per key.
//!
//! A notice is recorded only when the set CHANGES from the Session's last
//! one, read back from the log, so an unchanged breakage is announced once
//! however many turns or restarts follow, and a fix is announced once as an
//! empty set. Reasons are product-authored text; a notice is never provider
//! history.
//!
//! A notice of a kind this build does not know (a later build wrote it) is
//! informational, so the reader skips it instead of refusing the Session's
//! notices, and reports it as skipped: counted by kind, never silently lost.
//! Only a string kind can be unknown: a row with no kind, a kind of any
//! other JSON type, or a malformed row still refuses.
use crate::{CommittedPosition, SessionStore};
use grapt_store::{
    AggregateId, AppendInput, Appended, EventStore, FencedWriter, GovernedTransaction,
};
use grapt_types::{
    DeterministicAuthorities, EventId, EventVersion, ExecutionId, MessageId, SessionId,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::interrupted_turn::INTERRUPTED_TURN;

mod provider_error_detail;
use provider_error_detail::PROVIDER_ERROR_DETAIL;
pub use provider_error_detail::{
    provider_error_detail_notices, record_provider_error_detail, ProviderErrorDetailNotice,
    ProviderErrorOutcome,
};
mod lifecycle_hook;
mod page;
use lifecycle_hook::LIFECYCLE_HOOK;
pub use lifecycle_hook::{
    lifecycle_hook_notices, record_input_lifecycle_hook, record_lifecycle_hook,
    record_session_lifecycle_hook, LifecycleEvent, LifecycleHookNotice, LifecycleHookScope,
    MAX_HOOK_RESULT_BYTES,
};
mod context_trim;
mod sandbox_worktree_config;
mod tool_excluded;
use context_trim::CONTEXT_TRIMMED;
pub use context_trim::{record_context_trimmed, ContextEstimate, ContextTrimNotice};
pub use tool_excluded::{record_tool_excluded, ExcludedTool, ToolExcludedNotice};
mod step_failure;
pub use page::{public_notice_predicate, read_notice_at, skipped_kinds};
use sandbox_worktree_config::SANDBOX_WORKTREE_CONFIG;
pub use sandbox_worktree_config::{record_sandbox_worktree_config, SandboxWorktreeConfigNotice};
use step_failure::STEP_FAILURE;
pub use step_failure::{record_step_failure, step_failure_notices, StepFailureNotice};

const KIND: &str = "session.notice.recorded";
const MCP_UNAVAILABLE: &str = "mcp-unavailable";
const NOT_RUN_YET: &str = "not-run-yet";
const CONTEXT_REPEATED: &str = "context-repeated";
const CONFIG_IGNORED: &str = "config-ignored";
const COMPACTION_FAILED: &str = "compaction-failed";
/// The most sources one notice names.
pub const MAX_UNAVAILABLE: usize = 64;
/// The most bytes of one source or reason.
pub const MAX_NOTICE_TEXT_BYTES: usize = 256;
/// The most notices a transcript reader is given: the newest ones.
pub const MAX_NOTICES: usize = 128;
/// The most distinct unknown kinds one read reports, and the most bytes of
/// each kind name it keeps.
pub const MAX_SKIPPED_KINDS: usize = 8;
pub const MAX_SKIPPED_KIND_BYTES: usize = 64;
/// The most bytes one notice's stored data may have, at write and at read.
/// Above the worst case: 64 sources of two 256-byte parts, every byte a
/// quote or backslash escaped to two.
pub const MAX_NOTICE_DATA_BYTES: usize = 128 * 1024;

/// One source a turn could not use, and why, in product words.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unavailable {
    pub source: String,
    pub reason: String,
}

impl Unavailable {
    /// Both parts cut to their byte bound on a character boundary, with
    /// control characters (which only escape larger and display as
    /// nothing) replaced by spaces.
    pub fn new(source: &str, reason: &str) -> Self {
        Self {
            source: bounded(source),
            reason: bounded(reason),
        }
    }
}

/// `text` cut to at most `bytes` on a character boundary.
fn cut_to_bytes(text: &str, bytes: usize) -> String {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn bounded(text: &str) -> String {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut end = text.len().min(MAX_NOTICE_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub position: CommittedPosition,
    pub execution: ExecutionId,
    /// Sorted; empty when it announces that every source is usable again.
    pub unavailable: Vec<Unavailable>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    kind: String,
    unavailable: Vec<Unavailable>,
}

/// Record the MCP sources `execution`'s turn could not use, unless that is
/// what the Session's last notice already says. `None` when nothing changed.
pub fn record_mcp_unavailable(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    unavailable: Vec<Unavailable>,
) -> Result<Option<Notice>, String> {
    // The fields are public, so every entry is bounded here, at the one
    // write, however it was built.
    let mut unavailable: Vec<Unavailable> = unavailable
        .iter()
        .map(|entry| Unavailable::new(&entry.source, &entry.reason))
        .collect();
    unavailable.sort();
    unavailable.dedup();
    unavailable.truncate(MAX_UNAVAILABLE);
    if unavailable
        .iter()
        .any(|entry| entry.source.is_empty() || entry.reason.is_empty())
    {
        return Err("a notice names every source and its reason".into());
    }
    let aggregate = AggregateId::Session(session.clone());
    // Consume the already elected writer; a notice cannot elect ownership.
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    SessionStore::new(&tx)
        .record(session)
        .map_err(|error| error.to_string())?;
    let last = latest(&tx, session)?;
    let unchanged = match &last {
        Some(last) => last.unavailable == unavailable,
        // Nothing to clear, so silence.
        None => unavailable.is_empty(),
    };
    if unchanged {
        return Ok(None);
    }
    let data = serde_json::to_value(Data {
        session: session.as_str().to_owned(),
        execution: execution.as_str().to_owned(),
        kind: MCP_UNAVAILABLE.to_owned(),
        unavailable: unavailable.clone(),
    })
    .map_err(|error| error.to_string())?;
    let position = append_in(&tx, aggregate, writer, authorities, &data)?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(Some(Notice {
        position,
        execution: execution.clone(),
        unavailable,
    }))
}

/// Append one notice row of any kind inside `tx`: the one bounded write
/// every kind shares.
pub(crate) fn append_in(
    tx: &GovernedTransaction<'_>,
    aggregate: AggregateId,
    writer: FencedWriter,
    authorities: &DeterministicAuthorities<'_>,
    data: &serde_json::Value,
) -> Result<CommittedPosition, String> {
    if data.to_string().len() > MAX_NOTICE_DATA_BYTES {
        return Err("notice data exceeds its byte bound".into());
    }
    let event: EventId = format!("evt_{}", authorities.mint().as_str())
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    let created_ms = authorities.now().as_millis();
    if created_ms < 0 {
        return Err("notice time is negative".into());
    }
    let appended = EventStore::append_within(
        tx,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: KIND,
            version: EventVersion::new(1).ok_or("notice version refused")?,
            created_ms,
            data,
        },
        &mut [],
    )
    .map_err(|error| error.to_string())?;
    let Appended::Committed { seq } = appended else {
        return Err("notice event identity repeated".into());
    };
    Ok(CommittedPosition {
        event,
        seq,
        created_ms,
    })
}

/// A prompt admitted but not run yet: its turn was blocked before it began,
/// and it stays queued for the Session's next turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotRunYet {
    pub position: CommittedPosition,
    pub execution: ExecutionId,
    /// The admitted prompt that stays queued: a client that never sees it
    /// in history (it is promoted only when it runs) finds its notice by it.
    pub message: MessageId,
    /// Product-authored, cut to [`MAX_NOTICE_TEXT_BYTES`].
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NotRunYetData {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    #[serde(rename = "messageID")]
    message: String,
    kind: String,
    reason: String,
}

/// Record that `execution`'s turn was blocked before it began, and why.
/// Every block is recorded: each prompt that waits is told so.
pub fn record_not_run_yet(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    message: &MessageId,
    reason: &str,
) -> Result<NotRunYet, String> {
    let reason = bounded(reason);
    if reason.is_empty() {
        return Err("a not-run-yet notice names its reason".into());
    }
    let aggregate = AggregateId::Session(session.clone());
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    SessionStore::new(&tx)
        .record(session)
        .map_err(|error| error.to_string())?;
    let data = serde_json::to_value(NotRunYetData {
        session: session.as_str().to_owned(),
        execution: execution.as_str().to_owned(),
        message: message.as_str().to_owned(),
        kind: NOT_RUN_YET.to_owned(),
        reason: reason.clone(),
    })
    .map_err(|error| error.to_string())?;
    let position = append_in(&tx, aggregate, writer, authorities, &data)?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(NotRunYet {
        position,
        execution: execution.clone(),
        message: message.clone(),
        reason,
    })
}

/// The Session's newest not-run-yet notices, at most [`MAX_NOTICES`],
/// oldest first. A malformed row refuses.
pub fn not_run_yet_notices(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<NotRunYet>, String> {
    not_run_yet_notices_at(connection, session, None)
}

fn not_run_yet_notices_at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    at: Option<u64>,
) -> Result<Vec<NotRunYet>, String> {
    kind_rows_at::<NotRunYetData>(connection, session, NOT_RUN_YET, at)?
        .into_iter()
        .map(|(position, data)| {
            if data.session != session.as_str() || !valid_text(&data.reason) {
                return Err("not-run-yet notice identity or payload is invalid".into());
            }
            Ok(NotRunYet {
                position,
                execution: ExecutionId::new(data.execution).ok_or("invalid notice execution id")?,
                message: data
                    .message
                    .parse()
                    .map_err(|_| "invalid not-run-yet message id")?,
                reason: data.reason,
            })
        })
        .collect()
}

/// An execution that stopped because every call since the Session's last
/// compaction repeated what it summarized and the context was full: the
/// user is told why, and what to do next. The execution's own failure keeps
/// the standard context-limit class, which every build reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRepeated {
    pub position: CommittedPosition,
    pub execution: ExecutionId,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionNoticeData {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    kind: String,
}

/// Record that `execution` stopped on a repeating, full context.
pub fn record_context_repeated(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
) -> Result<ContextRepeated, String> {
    let position =
        record_execution_notice(store, authorities, session, execution, CONTEXT_REPEATED)?;
    Ok(ContextRepeated {
        position,
        execution: execution.clone(),
    })
}

/// The Session's newest context-repeated notices, at most [`MAX_NOTICES`],
/// oldest first. A malformed row refuses.
pub fn context_repeated_notices(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<ContextRepeated>, String> {
    context_repeated_notices_at(connection, session, None)
}

fn context_repeated_notices_at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    at: Option<u64>,
) -> Result<Vec<ContextRepeated>, String> {
    Ok(
        execution_notices(connection, session, CONTEXT_REPEATED, at)?
            .into_iter()
            .map(|(position, execution)| ContextRepeated {
                position,
                execution,
            })
            .collect(),
    )
}

/// An execution that stopped because the model's summary of the
/// conversation was unusable twice, so nothing freed its context: the user
/// is told that the history is intact and what to do next. The
/// execution's own failure keeps the standard
/// context-limit class every build reads; the notice is advisory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionFailed {
    pub position: CommittedPosition,
    pub execution: ExecutionId,
}

/// Record that `execution` stopped on an unusable summary.
pub fn record_compaction_failed(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
) -> Result<CompactionFailed, String> {
    let position =
        record_execution_notice(store, authorities, session, execution, COMPACTION_FAILED)?;
    Ok(CompactionFailed {
        position,
        execution: execution.clone(),
    })
}

/// The Session's newest compaction-failed notices, at most [`MAX_NOTICES`],
/// oldest first. A malformed row refuses.
pub fn compaction_failed_notices(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<CompactionFailed>, String> {
    compaction_failed_notices_at(connection, session, None)
}

fn compaction_failed_notices_at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    at: Option<u64>,
) -> Result<Vec<CompactionFailed>, String> {
    Ok(
        execution_notices(connection, session, COMPACTION_FAILED, at)?
            .into_iter()
            .map(|(position, execution)| CompactionFailed {
                position,
                execution,
            })
            .collect(),
    )
}

/// Append one notice of `kind` that names only its execution.
fn record_execution_notice(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    kind: &str,
) -> Result<CommittedPosition, String> {
    let data = serde_json::to_value(ExecutionNoticeData {
        session: session.as_str().to_owned(),
        execution: execution.as_str().to_owned(),
        kind: kind.to_owned(),
    })
    .map_err(|error| error.to_string())?;
    record_data_notice(store, authorities, session, &data)
}

fn record_data_notice(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    data: &serde_json::Value,
) -> Result<CommittedPosition, String> {
    let aggregate = AggregateId::Session(session.clone());
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    SessionStore::new(&tx)
        .record(session)
        .map_err(|error| error.to_string())?;
    let position = append_in(&tx, aggregate, writer, authorities, data)?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(position)
}

/// The Session's newest notices of `kind` that name only their execution,
/// validated.
fn execution_notices(
    connection: &rusqlite::Connection,
    session: &SessionId,
    kind: &str,
    at: Option<u64>,
) -> Result<Vec<(CommittedPosition, ExecutionId)>, String> {
    kind_rows_at::<ExecutionNoticeData>(connection, session, kind, at)?
        .into_iter()
        .map(|(position, data)| {
            if data.session != session.as_str() {
                return Err(format!("{kind} notice identity is invalid"));
            }
            let execution =
                ExecutionId::new(data.execution).ok_or("invalid notice execution id")?;
            Ok((position, execution))
        })
        .collect()
}

/// A config key the Session's configuration sets that SE accepts, so the
/// configuration loads, but does not apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIgnored {
    pub position: CommittedPosition,
    pub execution: ExecutionId,
    /// The dotted key, as the configuration spells it.
    pub key: String,
    /// What SE does instead, product-authored.
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigIgnoredData {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    kind: String,
    key: String,
    reason: String,
}

/// Record that the Session's configuration sets `key`, which SE accepts but
/// does not apply, unless the Session was already told. `None` when it was:
/// a key is named once per Session, however many turns or restarts follow.
pub fn record_config_ignored(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    key: &str,
    reason: &str,
) -> Result<Option<ConfigIgnored>, String> {
    let (key, reason) = (bounded(key), bounded(reason));
    if key.is_empty() || reason.is_empty() {
        return Err("a config-ignored notice names its key and reason".into());
    }
    let aggregate = AggregateId::Session(session.clone());
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    SessionStore::new(&tx)
        .record(session)
        .map_err(|error| error.to_string())?;
    // The prior notice for this key is read whole and validated as every
    // reader validates it: a corrupt row refuses rather than passing as
    // "already told" and silencing the notice.
    let named = matching_rows::<ConfigIgnoredData>(&tx, session, CONFIG_IGNORED, Some(&key), 1)?
        .into_iter()
        .map(|(position, data)| config_ignored(session, position, data))
        .next()
        .transpose()?;
    if named.is_some() {
        return Ok(None);
    }
    let data = serde_json::to_value(ConfigIgnoredData {
        session: session.as_str().to_owned(),
        execution: execution.as_str().to_owned(),
        kind: CONFIG_IGNORED.to_owned(),
        key: key.clone(),
        reason: reason.clone(),
    })
    .map_err(|error| error.to_string())?;
    let position = append_in(&tx, aggregate, writer, authorities, &data)?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(Some(ConfigIgnored {
        position,
        execution: execution.clone(),
        key,
        reason,
    }))
}

/// The Session's newest config-ignored notices, at most [`MAX_NOTICES`],
/// oldest first. A malformed row refuses.
pub fn config_ignored_notices(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<ConfigIgnored>, String> {
    config_ignored_notices_at(connection, session, None)
}

fn config_ignored_notices_at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    at: Option<u64>,
) -> Result<Vec<ConfigIgnored>, String> {
    kind_rows_at::<ConfigIgnoredData>(connection, session, CONFIG_IGNORED, at)?
        .into_iter()
        .map(|(position, data)| config_ignored(session, position, data))
        .collect()
}

/// One stored config-ignored notice, validated.
fn config_ignored(
    session: &SessionId,
    position: CommittedPosition,
    data: ConfigIgnoredData,
) -> Result<ConfigIgnored, String> {
    if data.session != session.as_str() || !valid_text(&data.key) || !valid_text(&data.reason) {
        return Err("config-ignored notice identity or payload is invalid".into());
    }
    Ok(ConfigIgnored {
        position,
        execution: ExecutionId::new(data.execution).ok_or("invalid notice execution id")?,
        key: data.key,
        reason: data.reason,
    })
}

/// Product text as every notice stores it: present, bounded, no controls.
fn valid_text(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_NOTICE_TEXT_BYTES && !text.chars().any(char::is_control)
}

/// The Session's newest notice rows of one `kind`, at most [`MAX_NOTICES`],
/// oldest first, each decoded whole: an oversized or malformed row refuses.
fn kind_rows<T: DeserializeOwned>(
    connection: &rusqlite::Connection,
    session: &SessionId,
    kind: &str,
) -> Result<Vec<(CommittedPosition, T)>, String> {
    matching_rows(connection, session, kind, None, MAX_NOTICES)
}

/// As [`kind_rows`], at most `limit` rows, only those naming `key` when it
/// is given.
pub(crate) fn matching_rows<T: DeserializeOwned>(
    connection: &rusqlite::Connection,
    session: &SessionId,
    kind: &str,
    key: Option<&str>,
    limit: usize,
) -> Result<Vec<(CommittedPosition, T)>, String> {
    matching_rows_at(connection, session, kind, key, limit, None)
}

pub(crate) fn kind_rows_at<T: DeserializeOwned>(
    connection: &rusqlite::Connection,
    session: &SessionId,
    kind: &str,
    at: Option<u64>,
) -> Result<Vec<(CommittedPosition, T)>, String> {
    matching_rows_at(
        connection,
        session,
        kind,
        None,
        if at.is_some() { 1 } else { MAX_NOTICES },
        at,
    )
}

fn matching_rows_at<T: DeserializeOwned>(
    connection: &rusqlite::Connection,
    session: &SessionId,
    kind: &str,
    key: Option<&str>,
    limit: usize,
    at: Option<u64>,
) -> Result<Vec<(CommittedPosition, T)>, String> {
    let at = at
        .map(i64::try_from)
        .transpose()
        .map_err(|error| error.to_string())?;
    let visible = conversation_rows(connection, session)?;
    let mut query = connection
        .prepare(&format!(
            "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 256 THEN id END, seq, created,
             CASE WHEN length(CAST(data AS BLOB)) <= ?3 THEN data END
             FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type = 'session.notice.recorded.1' AND json_extract(data, '$.kind') = ?4
             AND (?5 IS NULL OR json_extract(data, '$.key') = ?5) AND (?6 IS NULL OR seq = ?6)
             AND {visible} ORDER BY seq DESC LIMIT ?2"
        ))
        .map_err(|error| error.to_string())?;
    let limit = i64::try_from(limit).map_err(|error| error.to_string())?;
    let mut rows = query
        .query((
            session.as_str(),
            limit,
            MAX_NOTICE_DATA_BYTES as i64,
            kind,
            key,
            at,
        ))
        .map_err(|error| error.to_string())?;
    let mut found = Vec::new();
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        let id: String = row.get(0).map_err(|error| error.to_string())?;
        let seq: i64 = row.get(1).map_err(|error| error.to_string())?;
        let created_ms: i64 = row.get(2).map_err(|error| error.to_string())?;
        let raw: String = row.get(3).map_err(|error| error.to_string())?;
        if created_ms < 0 {
            return Err(format!("{kind} notice time is negative"));
        }
        let data: T = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        found.push((
            CommittedPosition {
                event: id
                    .parse()
                    .map_err(|error: grapt_types::IdError| error.to_string())?,
                seq: u64::try_from(seq).map_err(|error| error.to_string())?,
                created_ms,
            },
            data,
        ));
    }
    found.reverse();
    Ok(found)
}

fn latest(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Option<Notice>, String> {
    Ok(read(connection, session, 1)?.pop())
}

/// The Session's newest notices, at most [`MAX_NOTICES`], oldest first.
/// Every row is validated; a malformed one refuses rather than vanishing.
pub fn notices(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<Notice>, String> {
    read(connection, session, MAX_NOTICES)
}

/// Notices of one kind this build does not know, skipped by a read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedKind {
    /// The kind name, cut to [`MAX_SKIPPED_KIND_BYTES`].
    pub kind: String,
    pub count: u64,
}

/// Every notice a transcript shows, and the unknown kinds a read skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoticeRead {
    pub mcp: Vec<Notice>,
    pub not_run_yet: Vec<NotRunYet>,
    pub context_repeated: Vec<ContextRepeated>,
    pub config_ignored: Vec<ConfigIgnored>,
    pub compaction_failed: Vec<CompactionFailed>,
    pub context_trimmed: Vec<ContextTrimNotice>,
    pub provider_error_detail: Vec<ProviderErrorDetailNotice>,
    /// At most [`MAX_SKIPPED_KINDS`], by name.
    pub skipped: Vec<SkippedKind>,
    pub step_failure: Vec<StepFailureNotice>,
    pub lifecycle_hook: Vec<LifecycleHookNotice>,
    pub sandbox_worktree_config: Vec<SandboxWorktreeConfigNotice>,
    /// Turns a service stop cut short, newest last.
    pub interrupted_turn: Vec<crate::interrupted_turn::InterruptedTurn>,
    /// Definitions a request had to leave out, newest last.
    pub tool_excluded: Vec<ToolExcludedNotice>,
}

/// [`notices`], [`not_run_yet_notices`], [`context_repeated_notices`],
/// [`config_ignored_notices`] and [`compaction_failed_notices`], with the
/// kinds the read skipped.
pub fn read_notices(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<NoticeRead, String> {
    Ok(NoticeRead {
        mcp: read(connection, session, MAX_NOTICES)?,
        not_run_yet: not_run_yet_notices(connection, session)?,
        context_repeated: context_repeated_notices(connection, session)?,
        config_ignored: config_ignored_notices(connection, session)?,
        compaction_failed: compaction_failed_notices(connection, session)?,
        context_trimmed: context_trim::at(connection, session, None)?,
        provider_error_detail: provider_error_detail_notices(connection, session)?,
        skipped: skipped(connection, session)?,
        step_failure: step_failure_notices(connection, session)?,
        lifecycle_hook: lifecycle_hook_notices(connection, session)?,
        sandbox_worktree_config: sandbox_worktree_config::at(connection, session, None)?,
        interrupted_turn: crate::interrupted_turn::interrupted_turns(connection, session)?,
        tool_excluded: tool_excluded::at(connection, session, None)?,
    })
}

fn conversation_rows(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<String, String> {
    grapt_store::history_visibility::HistoryVisibility::read(connection, session)
        .map(|visible| visible.sql("seq"))
        .map_err(|error| error.to_string())
}

fn skipped(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<SkippedKind>, String> {
    let visible = conversation_rows(connection, session)?;
    let mut query = connection
        .prepare(&format!(
            "SELECT substr(kind, 1, ?3), COUNT(*) FROM (
               SELECT json_extract(safe_data, '$.kind') AS kind,
                      json_type(safe_data, '$.kind') AS shape FROM (
                 SELECT CASE WHEN length(CAST(data AS BLOB)) <= {MAX_NOTICE_DATA_BYTES}
                        THEN CASE WHEN json_valid(data) THEN data END END AS safe_data
                 FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
                 AND type = 'session.notice.recorded.1' AND {visible}))
             WHERE shape = 'text' AND kind NOT IN (?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             GROUP BY kind ORDER BY kind LIMIT ?2"
        ))
        .map_err(|error| error.to_string())?;
    let limit = i64::try_from(MAX_SKIPPED_KINDS).map_err(|error| error.to_string())?;
    let bytes = i64::try_from(MAX_SKIPPED_KIND_BYTES).map_err(|error| error.to_string())?;
    let rows = query
        .query_map(
            (
                session.as_str(),
                limit,
                bytes,
                MCP_UNAVAILABLE,
                NOT_RUN_YET,
                CONTEXT_REPEATED,
                CONFIG_IGNORED,
                COMPACTION_FAILED,
                PROVIDER_ERROR_DETAIL,
                STEP_FAILURE,
                LIFECYCLE_HOOK,
                SANDBOX_WORKTREE_CONFIG,
                CONTEXT_TRIMMED,
                INTERRUPTED_TURN,
            ),
            |row| {
                Ok(SkippedKind {
                    // SQL measures characters; the bound is bytes, cut whole.
                    kind: cut_to_bytes(&row.get::<_, String>(0)?, MAX_SKIPPED_KIND_BYTES),
                    count: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                })
            },
        )
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn read(
    connection: &rusqlite::Connection,
    session: &SessionId,
    limit: usize,
) -> Result<Vec<Notice>, String> {
    read_at(connection, session, limit, None)
}

fn read_at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    limit: usize,
    at: Option<u64>,
) -> Result<Vec<Notice>, String> {
    let at = at
        .map(i64::try_from)
        .transpose()
        .map_err(|error| error.to_string())?;
    let visible = conversation_rows(connection, session)?;
    let mut query = connection
        .prepare(&format!(
            "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 256 THEN id END, seq, created,
             CASE WHEN length(CAST(data AS BLOB)) <= ?3 THEN data END
             FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type = 'session.notice.recorded.1'
             AND NOT (coalesce(json_type(data, '$.kind'), '') = 'text'
                      AND json_extract(data, '$.kind') <> ?4)
             AND {visible} AND (?5 IS NULL OR seq = ?5) ORDER BY seq DESC LIMIT ?2"
        ))
        .map_err(|error| error.to_string())?;
    let limit = i64::try_from(limit).map_err(|error| error.to_string())?;
    let mut rows = query
        .query((
            session.as_str(),
            limit,
            MAX_NOTICE_DATA_BYTES as i64,
            MCP_UNAVAILABLE,
            at,
        ))
        .map_err(|error| error.to_string())?;
    let mut notices = Vec::new();
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        let id: String = row.get(0).map_err(|error| error.to_string())?;
        let seq: i64 = row.get(1).map_err(|error| error.to_string())?;
        let created_ms: i64 = row.get(2).map_err(|error| error.to_string())?;
        let raw: String = row.get(3).map_err(|error| error.to_string())?;
        let data: Data = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        if data.session != session.as_str()
            || data.kind != MCP_UNAVAILABLE
            || created_ms < 0
            || data.unavailable.len() > MAX_UNAVAILABLE
            || !data.unavailable.windows(2).all(|pair| pair[0] < pair[1])
            || !data
                .unavailable
                .iter()
                .all(|entry| valid_text(&entry.source) && valid_text(&entry.reason))
        {
            return Err("notice identity or payload is invalid".into());
        }
        notices.push(Notice {
            position: CommittedPosition {
                event: id
                    .parse()
                    .map_err(|error: grapt_types::IdError| error.to_string())?,
                seq: u64::try_from(seq).map_err(|error| error.to_string())?,
                created_ms,
            },
            execution: ExecutionId::new(data.execution).ok_or("invalid notice execution id")?,
            unavailable: data.unavailable,
        });
    }
    notices.reverse();
    Ok(notices)
}
