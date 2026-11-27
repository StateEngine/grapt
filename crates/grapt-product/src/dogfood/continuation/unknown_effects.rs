//! The harness-owned warning that replaces quarantine for unknown effects.
//!
//! An unknown outcome admits continuation, so its warning cannot depend on
//! the ordinary projection: a nested child never reaches model history, and
//! retained facts keep only the newest calls. Every request therefore carries
//! one notice for each unknown effect that no tool output already shows. It
//! names the call, never its payload, and is derived from durable history
//! alone, so restart and compaction reproduce it. An unreadable terminal row
//! is no evidence for any call: one that is not JSON gets a notice at its own
//! position, and the call it may have closed is projected without an outcome.
use super::*;
use rusqlite::OptionalExtension as _;

/// Newer unknown effects win; past this bound the oldest listed notice says
/// that earlier ones exist. Shown rows never count against it: an unknown
/// effect whose output a row already carries is warned by that row.
const MAX_NOTICES: usize = 16;

/// Identities are stored text of any length. A notice shows at most this many
/// bytes of each, then an ellipsis, so no notice can outgrow the request.
const MAX_SHOWN_IDENTITY_BYTES: usize = 128;

/// `(seq, notice)` for each unknown terminal before `ceiling` whose own seq
/// is not already a row in `shown`.
pub(super) fn notices(
    store: &EventStore,
    session: &SessionId,
    ceiling: u64,
    shown: &std::collections::BTreeSet<u64>,
) -> Result<Vec<(u64, CodexInputItem)>, String> {
    let ceiling = i64::try_from(ceiling).unwrap_or(i64::MAX);
    // Every terminal is classified by its decoder: an unreadable row gets an
    // unreadable notice, a readable one only when its effect is unknown.
    let data = grapt_store::stored_terminal::bounded_column("data");
    let mut statement = store
        .connection()
        .prepare(&format!(
            "SELECT seq, {data} FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type = 'session.tool.terminal.1' AND seq < ?2
             ORDER BY seq DESC"
        ))
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map((session.as_str(), ceiling), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                grapt_store::stored_terminal::classify(row, 1)?,
            ))
        })
        .map_err(|error| format!("unknown effect query failed: {error}"))?;
    // Streams newest first and stops at the first unshown effect past the
    // bound, so shown rows cannot crowd out an older, unshown one.
    let mut found = Vec::new();
    for row in rows {
        if found.len() > MAX_NOTICES {
            break;
        }
        let (seq, terminal) = row.map_err(|error| error.to_string())?;
        let seq = u64::try_from(seq).map_err(|_| "unknown effect has negative sequence")?;
        let fact = match terminal {
            StoredTerminal::Readable(fact) => fact,
            StoredTerminal::Unreadable { call } => {
                found.push((seq, call, None, true));
                continue;
            }
        };
        if !matches!(
            fact.outcome(),
            ToolTerminalOutcome::Failed {
                reason: grapt_types::ToolFailureReason::EffectUnknown,
                ..
            }
        ) || shown.contains(&seq)
        {
            continue;
        }
        let nested = nested_start(store, session, &fact)?;
        found.push((seq, Some(fact.call().clone()), nested, false));
    }
    let truncated = found.len() > MAX_NOTICES;
    found.truncate(MAX_NOTICES);
    let oldest = found.last().map(|(seq, ..)| *seq);
    Ok(found
        .into_iter()
        .map(|(seq, call, nested, unreadable)| {
            let subject = match (&call, &nested, unreadable) {
                (Some(call), _, true) => format!(
                    "Tool call {}, whose recorded outcome is unreadable,",
                    bounded(call.as_str())
                ),
                (None, _, _) => "A tool call whose recorded outcome is unreadable".to_owned(),
                (Some(call), Some(nested), false) => format!(
                    "Tool {} (call {}, run inside {})",
                    bounded(nested.tool().as_str()),
                    bounded(call.as_str()),
                    bounded(nested.parent().as_str())
                ),
                (Some(call), None, false) => format!("Tool call {}", bounded(call.as_str())),
            };
            let earlier = if truncated && Some(seq) == oldest {
                " Earlier unknown effects exist and are not listed."
            } else {
                ""
            };
            // A harness message, like presented mail: an assistant item outside
            // any response would read as part of the preceding one.
            let item = CodexInputItem::Message(InputMessage {
                role: MessageRole::User,
                content: format!(
                    "[Harness notice: {subject} ended with an unknown effect. It may have partly applied or may still be running. Verify the current state before relying on it; do not repeat it blindly.{earlier}]"
                ),
            });
            (seq, item)
        })
        .collect())
}

/// `identity`, cut at a character boundary past the shown bound.
fn bounded(identity: &str) -> std::borrow::Cow<'_, str> {
    if identity.len() <= MAX_SHOWN_IDENTITY_BYTES {
        return identity.into();
    }
    let mut end = MAX_SHOWN_IDENTITY_BYTES;
    while !identity.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &identity[..end]).into()
}

/// The nested start that admitted `fact`'s call, when it ran inside another.
fn nested_start(
    store: &EventStore,
    session: &SessionId,
    fact: &grapt_event::session_tool_terminal::ToolTerminalFact,
) -> Result<Option<grapt_event::session_tool_nested::NestedToolStartedFact>, String> {
    let data: Option<String> = store
        .connection()
        .query_row(
            "SELECT data FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type = 'session.tool.nested.started.1' AND json_valid(data)
             AND json_extract(data, '$.attempt') = ?2 AND json_extract(data, '$.call') = ?3
             LIMIT 1",
            (
                session.as_str(),
                fact.attempt().as_str(),
                fact.call().as_str(),
            ),
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("unknown effect nested lookup failed: {error}"))?;
    data.map(|data| {
        let value = serde_json::from_str(&data).map_err(|_| "nested start is not JSON")?;
        grapt_event::session_tool_nested::decode(&value)
            .map_err(|_| "nested start is corrupt".to_owned())
    })
    .transpose()
}
