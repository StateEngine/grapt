//! Exact-row notice reads for keyset-paged public history.
use super::*;
use crate::interrupted_turn::INTERRUPTED_TURN;

/// `data` is a fixed caller-owned SQL column, never user input. Unknown textual
/// kinds are advisory; malformed/oversized kinds still enter the strict decoder.
pub fn public_notice_predicate(data: &str) -> String {
    let kinds = [
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
        super::tool_excluded::TOOL_EXCLUDED,
    ]
    .map(|kind| format!("'{kind}'"))
    .join(",");
    format!("CASE WHEN length(CAST({data} AS BLOB)) > {MAX_NOTICE_DATA_BYTES} THEN 1 WHEN NOT json_valid({data}) THEN 1 WHEN json_type({data}, '$.kind') IS NOT 'text' THEN 1 ELSE json_extract({data}, '$.kind') IN ({kinds}) END")
}

pub fn skipped_kinds(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<SkippedKind>, String> {
    skipped(connection, session)
}

/// Every payload continues through its notice owner's existing decoder.
pub fn read_notice_at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    sequence: grapt_types::EventSeq,
) -> Result<NoticeRead, String> {
    let sequence = sequence.value();
    let seq = i64::try_from(sequence).map_err(|error| error.to_string())?;
    let kind: Option<String> = connection.query_row(
        &format!("SELECT CASE WHEN length(CAST(data AS BLOB)) <= {MAX_NOTICE_DATA_BYTES} THEN CASE WHEN length(CAST(json_extract(data, '$.kind') AS BLOB)) <= 256 THEN json_extract(data, '$.kind') END END FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND seq=?2 AND type='session.notice.recorded.1' LIMIT 1"),
        (session.as_str(), seq), |row| row.get(0)).map_err(|error| error.to_string())?;
    let mut notices = NoticeRead::default();
    match kind.as_deref() {
        Some(NOT_RUN_YET) => {
            notices.not_run_yet = not_run_yet_notices_at(connection, session, Some(sequence))?
        }
        Some(CONTEXT_REPEATED) => {
            notices.context_repeated =
                context_repeated_notices_at(connection, session, Some(sequence))?
        }
        Some(COMPACTION_FAILED) => {
            notices.compaction_failed =
                compaction_failed_notices_at(connection, session, Some(sequence))?
        }
        Some(CONTEXT_TRIMMED) => {
            notices.context_trimmed = context_trim::at(connection, session, Some(sequence))?;
        }
        Some(CONFIG_IGNORED) => {
            notices.config_ignored = config_ignored_notices_at(connection, session, Some(sequence))?
        }
        Some(PROVIDER_ERROR_DETAIL) => {
            notices.provider_error_detail =
                provider_error_detail::at(connection, session, sequence)?
        }
        Some(STEP_FAILURE) => {
            notices.step_failure = step_failure::at(connection, session, sequence)?
        }
        Some(LIFECYCLE_HOOK) => {
            notices.lifecycle_hook = lifecycle_hook::at(connection, session, Some(sequence))?
        }
        Some(SANDBOX_WORKTREE_CONFIG) => {
            notices.sandbox_worktree_config =
                sandbox_worktree_config::at(connection, session, Some(sequence))?
        }
        Some(INTERRUPTED_TURN) => {
            notices.interrupted_turn =
                crate::interrupted_turn::interrupted_turns_at(connection, session, Some(sequence))?
        }
        Some(super::tool_excluded::TOOL_EXCLUDED) => {
            notices.tool_excluded = super::tool_excluded::at(connection, session, Some(sequence))?
        }
        _ => notices.mcp = read_at(connection, session, 1, Some(sequence))?,
    }
    Ok(notices)
}
