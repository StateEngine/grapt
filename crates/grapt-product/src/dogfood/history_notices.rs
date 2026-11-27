//! Public advisory notice projection and bounded skipped-kind reporting.
use grapt_types::{ExecutionId, MessageId, SessionId};
use serde_json::json;

use super::history::IndexedRow;

mod interrupted_turn;
mod provider_error_detail;

/// A Session and the exact skipped kind names, as a structured key: no
/// delimiter can make two different kind sets look alike.
type SkippedKey = (String, Vec<String>);

/// The (Session, skipped kinds) pairs this process has already reported.
static SKIPPED_NOTICES_SEEN: std::sync::Mutex<std::collections::BTreeSet<SkippedKey>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());
/// Past this many reported pairs the set starts over: bounded memory, at
/// worst one repeated line per pair.
const MAX_SKIPPED_NOTICES_SEEN: usize = 1024;

/// A Session and one gap's exact durable coordinate, kind and class, as a
/// structured key: no delimiter can make two different gaps look alike.
type OmittedKey = (String, u64, String, &'static str);

/// The individual gaps this process has already logged.
static OMITTED_ROWS_SEEN: std::sync::Mutex<std::collections::BTreeSet<OmittedKey>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Log each unreadable owned item ONCE, at its own durable coordinate.
///
/// Per item, not per page: a page whose set changes because one new bad row
/// appeared must still announce that row, and a page that reports the same gap
/// again must not repeat it. The line carries the sequence, the kind and the
/// class, which is what a writer needs to find the producer; the decoder's own
/// text rides along as the detail and never reaches a user-facing surface.
pub(super) fn log_omissions(
    session: &SessionId,
    omissions: &[grapt_session::HistoryOmission],
) -> usize {
    if omissions.is_empty() {
        return 0;
    }
    let mut seen = OMITTED_ROWS_SEEN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut logged = 0usize;
    for omission in omissions {
        if seen.len() >= MAX_SKIPPED_NOTICES_SEEN {
            seen.clear();
        }
        let key = (
            session.as_str().to_owned(),
            omission.seq,
            omission.kind.clone(),
            omission.class.name(),
        );
        if !seen.insert(key) {
            continue;
        }
        logged = logged.saturating_add(1);
        crate::server_log::warn(format_args!(
            "session {session}: omitted unreadable history at seq {} kind {:?} class {}: {}",
            omission.seq,
            omission.kind,
            omission.class.name(),
            omission.detail
        ));
    }
    logged
}

/// The diagnostic line for notices a read skipped, the first time this
/// process sees that Session with those kinds; None otherwise. Counts are
/// left out of the key, so a growing count is not re-announced. Each name
/// is shown escaped and quoted, so the line is always one printable line.
fn skipped_notice_line(
    session: &SessionId,
    skipped: &[grapt_session::notice::SkippedKind],
    seen: &mut std::collections::BTreeSet<SkippedKey>,
) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let key = (
        session.as_str().to_owned(),
        skipped.iter().map(|skip| skip.kind.clone()).collect(),
    );
    if seen.len() >= MAX_SKIPPED_NOTICES_SEEN {
        seen.clear();
    }
    if !seen.insert(key) {
        return None;
    }
    let kinds = skipped
        .iter()
        .map(|skip| format!("{:?} x{}", skip.kind, skip.count))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "session {session}: skipped notices of unknown kinds: {kinds}"
    ))
}

/// A prompt that was admitted but not run yet, as a system row: the user's
/// to read, never the provider's.
fn not_run_yet_message(
    id: &str,
    created: i64,
    queued: &MessageId,
    reason: &str,
) -> Result<String, String> {
    // messageID names the queued prompt, which history shows only once it
    // runs: a client that admitted it finds this notice by it.
    let message: grapt_api::u3a::SessionMessageSystemDto = super::decode(json!({
        "type": "system",
        "id": id,
        "metadata": {"notRunYet": true, "messageID": queued.as_str()},
        "time": {"created": created},
        "text": not_run_yet_text(reason),
    }))?;
    serde_json::to_string(&message).map_err(|error| error.to_string())
}

/// What the user is told when an execution stopped because the model kept
/// repeating what its context already summarized.
pub(super) const CONTEXT_REPEATED_TEXT: &str = "The model kept re-reading content it had already summarized, and its context is now full, so the execution stopped. Its work so far is kept in the Session. Start a new session, or narrow the task so it needs less of what was read.";

/// The context-repeated notice as a system row beside its execution's
/// failure: the user's to read, never the provider's.
fn context_repeated_message(
    id: &str,
    created: i64,
    execution: &ExecutionId,
) -> Result<String, String> {
    let message: grapt_api::u3a::SessionMessageSystemDto = super::decode(json!({
        "type": "system",
        "id": id,
        "metadata": {"contextRepeated": true, "executionID": execution.as_str()},
        "time": {"created": created},
        "text": CONTEXT_REPEATED_TEXT,
    }))?;
    serde_json::to_string(&message).map_err(|error| error.to_string())
}

/// Why an execution stopped on an unusable summary, and what to do next.
/// True whether the retry also failed or could not be
/// admitted (review CF-H1).
pub(super) const COMPACTION_FAILED_TEXT: &str = "Couldn't summarize the conversation to free up context (the model's summary was unusable). Your history is intact. Try again, or start a new session from here.";

/// An execution that stopped on an unusable summary, as a system row.
fn compaction_failed_message(
    id: &str,
    created: i64,
    execution: &ExecutionId,
) -> Result<String, String> {
    let message: grapt_api::u3a::SessionMessageSystemDto = super::decode(json!({
        "type": "system",
        "id": id,
        "metadata": {"compactionFailed": true, "executionID": execution.as_str()},
        "time": {"created": created},
        "text": COMPACTION_FAILED_TEXT,
    }))?;
    serde_json::to_string(&message).map_err(|error| error.to_string())
}

/// A config key the Session's configuration sets that SE accepts but does
/// not apply, as a system row: named once per Session per key.
fn config_ignored_message(
    id: &str,
    created: i64,
    key: &str,
    reason: &str,
) -> Result<String, String> {
    let message: grapt_api::u3a::SessionMessageSystemDto = super::decode(json!({
        "type": "system",
        "id": id,
        "metadata": {"configIgnored": true, "key": key},
        "time": {"created": created},
        "text": config_ignored_text(key, reason),
    }))?;
    serde_json::to_string(&message).map_err(|error| error.to_string())
}

/// The config-ignored notice's words.
pub(super) fn config_ignored_text(key: &str, reason: &str) -> String {
    format!("Config key {key} is accepted but not applied; {reason}.")
}

/// The not-run-yet notice's words.
pub(super) fn not_run_yet_text(reason: &str) -> String {
    format!(
        "Not run yet: {reason}. Your message is queued and runs on the next turn once this is fixed."
    )
}

/// A durable MCP notice as a system row. It is the user's to read only: the
/// provider's history is folded from the event log, not from these rows.
fn notice_message(
    id: &str,
    created: i64,
    unavailable: &[grapt_session::notice::Unavailable],
) -> Result<String, String> {
    let text = if unavailable.is_empty() {
        "MCP servers are available again.".to_owned()
    } else {
        let sources = unavailable
            .iter()
            .map(|entry| format!("{} ({})", entry.source, entry.reason))
            .collect::<Vec<_>>()
            .join("; ");
        format!("MCP unavailable this turn: {sources}")
    };
    let message: grapt_api::u3a::SessionMessageSystemDto = super::decode(json!({
        "type": "system",
        "id": id,
        "metadata": {"mcpUnavailable": true},
        "time": {"created": created},
        "text": text,
    }))?;
    serde_json::to_string(&message).map_err(|error| error.to_string())
}

fn append_execution_notices(
    trimmed: Vec<grapt_session::notice::ContextTrimNotice>,
    failed: Vec<grapt_session::notice::StepFailureNotice>,
    rows: &mut Vec<IndexedRow>,
) -> Result<(), String> {
    for row in trimmed
        .into_iter()
        .map(super::execution::context_trim::row)
        .chain(
            failed
                .into_iter()
                .map(super::execution::failure_notice::row),
        )
    {
        let (seq, id, raw) = row?;
        rows.push(IndexedRow::rendered(seq, id, raw));
    }
    Ok(())
}

/// Append the existing notice rows while the caller retains its Inbox guard.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn append(
    session: &SessionId,
    notices: grapt_session::notice::NoticeRead,
    rows: &mut Vec<IndexedRow>,
) -> Result<(), String> {
    let grapt_session::notice::NoticeRead {
        mcp: notices,
        not_run_yet,
        context_repeated,
        config_ignored,
        compaction_failed,
        context_trimmed,
        provider_error_detail,
        step_failure,
        lifecycle_hook,
        sandbox_worktree_config,
        interrupted_turn,
        tool_excluded,
        skipped,
    } = notices;
    append_execution_notices(context_trimmed, step_failure, rows)?;
    for notice in sandbox_worktree_config {
        let suffix = notice
            .position
            .event
            .as_str()
            .strip_prefix("evt_")
            .ok_or("sandbox notice event identity has no prefix")?;
        let id = format!("msg_{suffix}");
        let message: grapt_api::u3a::SessionMessageSystemDto = super::decode(json!({
            "type": "system", "id": id,
            "metadata": {"sandboxWorktreeConfig": true},
            "time": {"created": notice.position.created_ms},
            "text": notice.text,
        }))?;
        let raw = serde_json::to_string(&message).map_err(|e| e.to_string())?;
        rows.push(IndexedRow::rendered(notice.position.seq, id, raw));
    }
    for notice in lifecycle_hook {
        let (seq, id, raw) = super::hooks::notice_row(notice)?;
        rows.push(IndexedRow::rendered(seq, id, raw));
    }
    provider_error_detail::append(rows, provider_error_detail)?;
    // A later build's notice kind is skipped, never silently: one
    // bounded diagnostic line, once per process, Session and kinds.
    let line = {
        let mut seen = match SKIPPED_NOTICES_SEEN.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        skipped_notice_line(session, &skipped, &mut seen)
    };
    if let Some(line) = line {
        crate::diagnostics::report(line);
    }
    interrupted_turn::append(rows, interrupted_turn)?;
    for notice in tool_excluded {
        let suffix = notice
            .position
            .event
            .as_str()
            .strip_prefix("evt_")
            .ok_or("tool exclusion event identity has no prefix")?;
        let id = format!("msg_{suffix}");
        // One line per excluded definition, then a bounded summary: a reader
        // must be able to see which tools are missing and why.
        // An emptied set is the repair the reader was promised when the
        // exclusion was first announced, so it renders as a real row rather
        // than as blank text.
        let mut text = if notice.excluded.is_empty() && notice.omitted == 0 {
            "Every tool definition is offered again.".to_owned()
        } else {
            String::new()
        };
        for excluded in &notice.excluded {
            text.push_str(&format!(
                "Tool {} is not offered this Session: {}.\n",
                excluded.name, excluded.reason
            ));
        }
        if notice.omitted > 0 {
            text.push_str(&format!(
                "and {} more tool definitions are not offered this Session.\n",
                notice.omitted
            ));
        }
        let message: grapt_api::u3a::SessionMessageSystemDto = super::decode(json!({
            "type": "system", "id": id,
            "metadata": {"toolExcluded": true},
            "time": {"created": notice.position.created_ms},
            "text": text.trim_end(),
        }))?;
        let raw = serde_json::to_string(&message).map_err(|e| e.to_string())?;
        rows.push(IndexedRow::rendered(notice.position.seq, id, raw));
    }
    for notice in not_run_yet {
        let suffix = notice
            .position
            .event
            .as_str()
            .strip_prefix("evt_")
            .ok_or("notice event identity has no prefix")?;
        let id = format!("msg_{suffix}");
        let raw = not_run_yet_message(
            &id,
            notice.position.created_ms,
            &notice.message,
            &notice.reason,
        )?;
        rows.push(IndexedRow::rendered(notice.position.seq, id, raw));
    }
    for notice in context_repeated {
        let suffix = notice
            .position
            .event
            .as_str()
            .strip_prefix("evt_")
            .ok_or("notice event identity has no prefix")?;
        let id = format!("msg_{suffix}");
        let raw = context_repeated_message(&id, notice.position.created_ms, &notice.execution)?;
        rows.push(IndexedRow::rendered(notice.position.seq, id, raw));
    }
    for notice in compaction_failed {
        let suffix = notice
            .position
            .event
            .as_str()
            .strip_prefix("evt_")
            .ok_or("notice event identity has no prefix")?;
        let id = format!("msg_{suffix}");
        let raw = compaction_failed_message(&id, notice.position.created_ms, &notice.execution)?;
        rows.push(IndexedRow::rendered(notice.position.seq, id, raw));
    }
    for notice in config_ignored {
        let suffix = notice
            .position
            .event
            .as_str()
            .strip_prefix("evt_")
            .ok_or("notice event identity has no prefix")?;
        let id = format!("msg_{suffix}");
        let raw =
            config_ignored_message(&id, notice.position.created_ms, &notice.key, &notice.reason)?;
        rows.push(IndexedRow::rendered(notice.position.seq, id, raw));
    }
    for notice in notices {
        let suffix = notice
            .position
            .event
            .as_str()
            .strip_prefix("evt_")
            .ok_or("notice event identity has no prefix")?;
        let id = format!("msg_{suffix}");
        let raw = notice_message(&id, notice.position.created_ms, &notice.unavailable)?;
        rows.push(IndexedRow::rendered(notice.position.seq, id, raw));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two polls of one Session with the same skipped
    /// kinds report once; a new kind set, or another Session, reports again.
    #[test]
    fn skipped_notice_kinds_are_reported_once_per_session_and_kinds() -> Result<(), String> {
        let skipped = |kind: &str, count| grapt_session::notice::SkippedKind {
            kind: kind.to_owned(),
            count,
        };
        let one: SessionId = "ses_skipped_one".parse().map_err(|_| "session id")?;
        let two: SessionId = "ses_skipped_two".parse().map_err(|_| "session id")?;
        let mut seen = std::collections::BTreeSet::new();
        let first = skipped_notice_line(&one, &[skipped("later", 1)], &mut seen);
        assert_eq!(
            first.as_deref(),
            Some("session ses_skipped_one: skipped notices of unknown kinds: \"later\" x1")
        );
        assert_eq!(
            skipped_notice_line(&one, &[skipped("later", 2)], &mut seen),
            None,
            "a second poll repeated the line"
        );
        assert!(
            skipped_notice_line(&one, &[skipped("later", 2), skipped("newer", 1)], &mut seen)
                .is_some()
        );
        assert!(skipped_notice_line(&two, &[skipped("later", 1)], &mut seen).is_some());
        assert_eq!(skipped_notice_line(&two, &[], &mut seen), None);
        Ok(())
    }

    /// Review T-H2/T-H3: kind sets are keyed exactly, so a
    /// delimiter in a name cannot make two sets alike, and a name with a
    /// newline or control character still yields one printable line.
    #[test]
    fn skipped_kind_names_are_keyed_exactly_and_shown_on_one_line() -> Result<(), String> {
        let kind = |name: &str| grapt_session::notice::SkippedKind {
            kind: name.to_owned(),
            count: 1,
        };
        let session: SessionId = "ses_skipped_delimiters".parse().map_err(|_| "session id")?;
        let mut seen = std::collections::BTreeSet::new();
        assert!(skipped_notice_line(&session, &[kind("a"), kind("b,c")], &mut seen).is_some());
        assert!(
            skipped_notice_line(&session, &[kind("a,b"), kind("c")], &mut seen).is_some(),
            "a delimiter made two kind sets alike"
        );
        let line = skipped_notice_line(&session, &[kind("x\ny\u{7}")], &mut seen)
            .ok_or("a new kind set was not reported")?;
        assert!(
            !line.chars().any(char::is_control),
            "the line is not one printable line: {line:?}"
        );
        Ok(())
    }

    /// A not-run-yet row names its queued prompt, so a client that never
    /// sees the prompt in history finds the notice by its id.
    #[test]
    fn a_not_run_yet_row_names_its_queued_prompt() -> Result<(), String> {
        let queued: MessageId = "msg_queued_row".parse().map_err(|_| "message id")?;
        let row = not_run_yet_message("msg_notice_row", 7, &queued, "the reason")?;
        let value: serde_json::Value = serde_json::from_str(&row).map_err(|e| e.to_string())?;
        assert_eq!(value["metadata"]["notRunYet"], true);
        assert_eq!(value["metadata"]["messageID"], "msg_queued_row");
        assert_eq!(value["text"], not_run_yet_text("the reason"));
        Ok(())
    }
}
