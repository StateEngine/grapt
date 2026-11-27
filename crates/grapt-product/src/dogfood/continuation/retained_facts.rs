//! Deterministic facts retained beside a model-written compaction summary.
//!
//! A summary is lossy and model-written. These facts are derived only from
//! durable history before the retained cut, so a restart reproduces them byte
//! for byte and their accuracy does not depend on the summarizer: the user's
//! own earlier inputs, verbatim, and how each recent settled tool call ended.
//! Summary generation and this retention stay separate owners of their text.

use std::fmt::Write as _;

use super::*;

const MAX_USER_BYTES: usize = 16 * 1024;
/// The share of [`MAX_USER_BYTES`] the first earlier input always keeps: it
/// is the task the Session began with, and newer inputs as long as C04's
/// 24.7 KB archive prompts crowded it out entirely.
const FIRST_INPUT_BYTES: usize = MAX_USER_BYTES / 2;
const MAX_LEDGER_CALLS: usize = 32;
const MAX_ARGUMENT_PREVIEW: usize = 256;

pub(super) const FACTS_LABEL: &str = "[Harness-retained facts from durable history before the continuation summary. These are exact records, not model-written. Earlier user inputs are quoted verbatim, oldest first; the current user input remains authoritative.]\n";

/// The facts for history before `until`, or `None` when there are none.
pub(super) fn retained_facts(
    inbox: &Inbox,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    current: &MessageId,
    until: grapt_types::EventSeq,
) -> Result<Option<String>, String> {
    let mut facts = String::new();
    user_inputs(inbox, session, current, until, &mut facts)?;
    tool_ledger(inbox.store(), artifacts, session, until, &mut facts)?;
    Ok((!facts.is_empty()).then(|| format!("{FACTS_LABEL}{facts}")))
}

fn user_inputs(
    inbox: &Inbox,
    session: &SessionId,
    current: &MessageId,
    until: grapt_types::EventSeq,
    facts: &mut String,
) -> Result<(), String> {
    let users = match inbox.promoted_messages_window(session, until, MAX_CONTINUATION_ROWS) {
        Ok(users) => users,
        Err(grapt_inbox::InboxError::PromotedPageTooLong { .. }) => {
            facts.push_str(
                "Earlier user inputs exceed the retained bound; only the summary carries them.\n",
            );
            return Ok(());
        }
        Err(error) => return Err(format!("earlier user inputs are unreadable: {error}")),
    };
    let users = users
        .iter()
        .filter(|user| user.kind == grapt_inbox::PromotedInputKind::User && &user.input != current)
        .collect::<Vec<_>>();
    let Some((first, later)) = users.split_first() else {
        return Ok(());
    };
    // The first input within its share, then later ones newest first until
    // the bound, printed oldest first.
    let first = quote(&first.text, FIRST_INPUT_BYTES)?;
    let mut remaining = MAX_USER_BYTES - first.as_ref().map_or(0, String::len);
    let mut kept = Vec::new();
    for user in later.iter().rev() {
        let Some(quoted) = quote(&user.text, remaining)? else {
            break;
        };
        remaining -= quoted.len();
        kept.push(quoted);
    }
    let _ = writeln!(
        facts,
        "Earlier user inputs ({} of {}, oldest first):",
        kept.len() + usize::from(first.is_some()),
        users.len()
    );
    if let Some(first) = first {
        facts.push_str("- ");
        facts.push_str(&first);
        facts.push('\n');
    }
    let omitted = later.len() - kept.len();
    if omitted > 0 {
        let _ = writeln!(
            facts,
            "- ({omitted} later inputs omitted by the retained bound)"
        );
    }
    for quoted in kept.iter().rev() {
        facts.push_str("- ");
        facts.push_str(quoted);
        facts.push('\n');
    }
    Ok(())
}

/// A JSON string literal of `text`, cut at a character boundary when it does
/// not fit `limit` bytes, or `None` when not even a marker fits.
fn quote(text: &str, limit: usize) -> Result<Option<String>, String> {
    let encode = |text: &str| serde_json::to_string(text).map_err(|error| error.to_string());
    let whole = encode(text)?;
    if whole.len() <= limit {
        return Ok(Some(whole));
    }
    // The longest character-safe prefix whose ESCAPED form, its quotes and
    // the widest marker fit: one pass, measured as JSON writes it. Shrinking
    // by the escaped excess in raw bytes could overshoot to nothing when
    // escapes expand the text (review FB-H1).
    let widest = format!(" (first {0} of {0} bytes)", text.len()).len();
    let Some(room) = limit.checked_sub(widest + 2) else {
        return Ok(None);
    };
    let mut escaped = 0usize;
    let mut end = 0usize;
    for (index, character) in text.char_indices() {
        escaped += escaped_len(character);
        if escaped > room {
            break;
        }
        end = index + character.len_utf8();
    }
    if end == 0 {
        return Ok(None);
    }
    let marked = format!(
        "{} (first {end} of {} bytes)",
        encode(&text[..end])?,
        text.len()
    );
    Ok((marked.len() <= limit).then_some(marked))
}

/// The bytes `character` takes inside a JSON string as serde_json writes it.
pub(super) fn escaped_len(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        control if u32::from(control) < 0x20 => 6,
        other => other.len_utf8(),
    }
}

pub(super) fn tool_ledger(
    store: &EventStore,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    until: grapt_types::EventSeq,
    facts: &mut String,
) -> Result<(), String> {
    let terminals = model_terminals(store, session, None, until, MAX_LEDGER_CALLS + 1)?;
    if terminals.is_empty() {
        return Ok(());
    }
    let omitted = terminals.len() > MAX_LEDGER_CALLS;
    let _ = writeln!(
        facts,
        "Settled tool calls ({}newest {}, oldest first):",
        if omitted {
            "earlier calls omitted; "
        } else {
            ""
        },
        terminals.len().min(MAX_LEDGER_CALLS)
    );
    for (_, fact) in terminals.iter().take(MAX_LEDGER_CALLS).rev() {
        let (tool, arguments) = match called(store, artifacts, session, fact.call(), until)? {
            Some((tool, arguments)) => (tool, preview(&arguments)),
            None => ("unknown tool".to_owned(), String::new()),
        };
        let _ = writeln!(facts, "- {tool} {arguments} -> {}", outcome(fact.outcome()));
    }
    Ok(())
}

/// The recorded tool name and exact arguments of one call before `until`.
pub(super) fn called(
    store: &EventStore,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    call: &ToolCallId,
    until: grapt_types::EventSeq,
) -> Result<Option<(String, String)>, String> {
    let window = BoundedWindow {
        from: None,
        until: Some(until),
        max_rows: MAX_CONTINUATION_ROWS,
    };
    Ok(
        stored_calls_for(store, session, artifacts, window, Some(call), None)?
            .into_iter()
            .next()
            .map(|call| {
                (
                    call.tool
                        .as_ref()
                        .map_or("unknown tool", ToolName::as_str)
                        .to_owned(),
                    call.arguments.as_str().to_owned(),
                )
            }),
    )
}

pub(super) fn preview(arguments: &str) -> String {
    if arguments.len() <= MAX_ARGUMENT_PREVIEW {
        return arguments.to_owned();
    }
    let mut end = MAX_ARGUMENT_PREVIEW;
    while !arguments.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... ({} bytes)", &arguments[..end], arguments.len())
}

fn outcome(outcome: &ToolTerminalOutcome) -> String {
    match outcome {
        ToolTerminalOutcome::Completed {
            output: Some(output),
        } => {
            format!("completed, {} output bytes", output.reference().length())
        }
        ToolTerminalOutcome::Completed { output: None } => "completed, no output".to_owned(),
        ToolTerminalOutcome::Failed {
            reason: grapt_types::ToolFailureReason::EffectUnknown,
            ..
        } => "effect state unknown; verify before repeating it".to_owned(),
        ToolTerminalOutcome::Failed { reason, .. } => format!("failed ({reason:?})"),
        ToolTerminalOutcome::Cancelled { by } => format!("cancelled ({by:?})"),
        ToolTerminalOutcome::ResultMissing => "result missing".to_owned(),
    }
}

/// The newest model-visible tool terminals in `(after, until)`, newest first,
/// at most `limit`. Nested CodeMode effects are their parent call's business,
/// as elsewhere in continuation.
pub(super) fn model_terminals(
    store: &EventStore,
    session: &SessionId,
    after: Option<grapt_types::EventSeq>,
    until: grapt_types::EventSeq,
    limit: usize,
) -> Result<Vec<(u64, session_tool_terminal::ToolTerminalFact)>, String> {
    let after = after
        .map(|after| i64::try_from(after.value()))
        .transpose()
        .map_err(|_| "tool window exceeds SQLite range")?
        .unwrap_or(-1);
    let until = i64::try_from(until.value()).map_err(|_| "retained cut exceeds SQLite range")?;
    let limit = i64::try_from(limit).map_err(|_| "invalid ledger bound")?;
    let data = grapt_store::stored_terminal::bounded_column("e.data");
    let mut statement = store
        .connection()
        .prepare(&format!(
            "SELECT e.seq, {data} FROM event e
             WHERE e.aggregate_family = 'session' AND e.aggregate_id = ?1
               AND e.type = 'session.tool.terminal.1' AND e.seq > ?4 AND e.seq < ?2
               AND NOT EXISTS (SELECT 1 FROM event n WHERE n.aggregate_family = 'session'
                   AND n.aggregate_id = e.aggregate_id AND n.type = 'session.tool.nested.started.1'
                   AND json_valid(n.data) AND json_valid(e.data)
                   AND json_extract(n.data, '$.attempt') = json_extract(e.data, '$.attempt')
                   AND json_extract(n.data, '$.call') = json_extract(e.data, '$.call'))
             ORDER BY e.seq DESC LIMIT ?3"
        ))
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map((session.as_str(), until, limit, after), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                grapt_store::stored_terminal::classify(row, 1)?,
            ))
        })
        .map_err(|error| format!("tool ledger query failed: {error}"))?;
    let mut terminals = Vec::new();
    for row in rows {
        let (seq, data) = row.map_err(|error| error.to_string())?;
        let seq = u64::try_from(seq).map_err(|_| "tool terminal has negative sequence")?;
        let StoredTerminal::Readable(fact) = data else {
            continue;
        };
        terminals.push((seq, fact));
    }
    Ok(terminals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cq_quote_is_exact_when_it_fits_and_marked_when_cut() -> Result<(), String> {
        assert_eq!(
            quote("keep \"this\"", 64)?,
            Some(r#""keep \"this\"""#.to_owned())
        );
        let long = "é".repeat(100);
        let cut = quote(&long, 60)?.ok_or("marker fits")?;
        assert!(cut.len() <= 60, "{cut}");
        assert!(cut.ends_with("of 200 bytes)"), "{cut}");
        assert_eq!(quote(&long, 4)?, None);
        Ok(())
    }

    #[test]
    fn cq_preview_is_bounded_on_a_character_boundary() {
        assert_eq!(preview(r#"{"path":"a"}"#), r#"{"path":"a"}"#);
        let long = format!(r#"{{"text":"{}"}}"#, "ü".repeat(200));
        let shown = preview(&long);
        assert!(shown.ends_with(&format!("... ({} bytes)", long.len())));
        assert!(shown.len() <= MAX_ARGUMENT_PREVIEW + 32);
    }
}
