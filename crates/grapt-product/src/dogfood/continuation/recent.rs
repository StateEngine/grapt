//! The public `recent` value for committed session compactions.
//!
//! Transcription of the frozen oracle (the oracle's `compaction.ts`
//! and util/token.ts at the pinned freeze point; the corpus generator
//! re-verifies both blobs byte-for-byte before it writes expected values):
//! the conversation is the serialized promoted inputs and assistant
//! messages of the window the deterministic retained selection bounds,
//! step N reading [selection(N-1).from, selection(N).source_head]; the
//! estimate rounds UTF-16 units over four, floored at zero; the walk keeps
//! the final entries under the keep budget, always keeps the last entry,
//! and breaks on a strict over-bound; the split pulls back until the
//! recent half opens on a user entry; a walk to the front splits at the
//! latest user entry past the front; the halves join with blank lines;
//! and the published value is the recent half, empty only on the
//! summarize-recent branch (no previous recent and no head).
//!
//! Admitted non-parity against the oracle:
//! - User file-attachment lines and the system, skill and shell message
//!   types have no product representation and project to nothing.
//! - Opaque content items keep their fold position but serialize to no
//!   oracle line.
//! - Bound and corruption evidence is refused with typed product errors
//!   instead of the oracle undefined behavior: the 65_536-row compaction
//!   window, the 8 MiB materialization budget
//!   (labels and separators charged before the fetch), the 256-byte
//!   identity read bound on DB-controlled identity text, negative
//!   sequences, a duplicate or orphan terminal, a terminal that does not
//!   follow its call, and a descriptor that does not bind its committed
//!   lineage.
//! - A window past the 8 MiB budget is read from the earliest promoted user
//!   input between exchanges that fits it (SC-REL-10). The published value
//!   is the one the whole window would give, because the walk stops after
//!   that start; when the walk would reach it, the refusal stands.
//! - The published value must fit the 256 KiB DTO string bound. Past it,
//!   its oldest text is dropped behind a visible marker rather than failing
//!   the compaction; the conversation itself is internal text, bounded by
//!   the 8 MiB materialization budget only.

use std::collections::{HashMap, HashSet};

use super::retained::{Selection, MAX_SELECTION_BYTES};
use super::*;
use grapt_api::u3a::Utf16Text;
use grapt_types::{ContentKind, EventSeq, RetainedHistory};

/// The oracle DEFAULT_KEEP_TOKENS.
const KEEP_TOKENS: u64 = 8_000;
/// The oracle TOOL_OUTPUT_MAX_CHARS, counted in UTF-16 units.
pub(super) const TOOL_OUTPUT_MAX_UNITS: usize = 2_000;
/// Durable rows one compaction window may read before refusing.
///
/// Compaction must read past the 50_000-row page fold to recover from its
/// structural limit. The 65_536-row ceiling bounds fixed per-row overhead
/// to roughly 10 MiB; row data retains the separate projection byte budget.
pub(super) const MAX_RECENT_WINDOW_ROWS: usize = 65_536;
/// The projection's own read budget (SC-COMPACT-02), charged before any
/// fetch across every projected family and by the window's raw-history fold.
///
/// A structural compaction fires once the request past the retained cut
/// reaches [`MAX_CONTEXT_BYTES`], and the compactor projects that window
/// through the crossing row. Charged against the request's own bound, a
/// crossing carried by promoted inputs always refused, and automatic
/// compaction paused. This budget holds that crossing: a window whose
/// request fit the bound, one more input at the prompt cap, and the
/// projection's framing on every entry the fitting request could carry.
/// A larger window retries from a later complete exchange (SC-REL-10);
/// it still refuses when the public recent value would depend on unread rows.
pub(super) const MAX_RECENT_MATERIALIZATION_BYTES: usize = 2 * MAX_CONTEXT_BYTES;
const _: () = assert!(
    MAX_CONTEXT_BYTES
        + grapt_api::u3a::limits::MAX_PROMPT_TEXT_BYTES
        + (MAX_CONTEXT_ITEMS + 1) * (2 * MAX_RECENT_ID_BYTES + SYNTHETIC_LABEL.len() + 2)
        <= MAX_RECENT_MATERIALIZATION_BYTES
);
/// A descriptor larger than this is corruption, not a value.
const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;
/// The explicit read bound for DB-controlled identity text on this path:
/// a message or compaction event identity is unconstrained SQLite TEXT, so
/// the read must bound it itself. A resource bound, not an identity law -
/// an oversized value is refused, never truncated.
pub(in crate::dogfood) const MAX_RECENT_ID_BYTES: usize = 256;

/// The serialized line labels, verbatim from the oracle. Every label is
/// ASCII, so its byte length is its UTF-16 unit count. The tool labels
/// also price the bounded reads continuation.rs performs before a fetch.
pub(super) const USER_LABEL: &str = "[User]: ";
pub(super) const SYNTHETIC_LABEL: &str = "[Synthetic context]: ";
pub(super) const ASSISTANT_LABEL: &str = "[Assistant]: ";
pub(super) const REASONING_LABEL: &str = "[Assistant reasoning]: ";
pub(super) const TOOL_CALL_LABEL: &str = "[Assistant tool call]: ";
pub(super) const TOOL_RESULT_LABEL: &str = "[Tool result]: ";
pub(super) const TOOL_ERROR_LABEL: &str = "[Tool error]: ";

/// The worst UTF-8 length of a unit-truncated tool result: the 2_000 cut
/// units at at most three bytes apiece, the lone high surrogate the cut
/// may leave at four, and the 12-unit marker.
pub(super) const MAX_TRUNCATED_RESULT_BYTES: usize = 1_999 * 3 + 4 + 12;

/// Token.estimate: Math.max(0, Math.round(utf16 units / 4)).
pub(super) fn estimate_tokens(units: &[u16]) -> u64 {
    let units = u64::try_from(units.len()).unwrap_or(u64::MAX);
    (units.saturating_add(2)) / 4
}

/// The oracle tool-result truncation: verbatim at or under the bound,
/// otherwise the first 2_000 UTF-16 units plus the newline marker. The
/// cut is unit-exact like the oracle's slice: a surrogate pair may be
/// split, leaving a lone high surrogate at the cut.
pub(super) fn truncate_tool_output_units(units: &[u16]) -> Vec<u16> {
    if units.len() <= TOOL_OUTPUT_MAX_UNITS {
        return units.to_vec();
    }
    let mut kept = units[..TOOL_OUTPUT_MAX_UNITS].to_vec();
    kept.extend("\n[truncated]".encode_utf16());
    kept
}

/// One assistant part, in the message ordinal order.
#[derive(Debug)]
pub(super) enum Part {
    Text(Vec<u16>),
    Reasoning(Vec<u16>),
    Tool {
        name: String,
        input: String,
        result: Option<Vec<u16>>,
        error: Option<String>,
    },
}

/// The oracle assistant-message serialize: the part lines joined with
/// single newlines. None is the oracle falsy serialize, which the
/// conversation drops.
pub(super) fn serialize_assistant(parts: &[Part]) -> Option<Vec<u16>> {
    let labeled = |label: &str, text: &[u16]| -> Vec<u16> {
        let mut line = label.encode_utf16().collect::<Vec<u16>>();
        line.extend_from_slice(text);
        line
    };
    let mut lines: Vec<Vec<u16>> = Vec::with_capacity(parts.len().saturating_mul(2));
    for part in parts {
        match part {
            Part::Text(text) => lines.push(labeled(ASSISTANT_LABEL, text)),
            Part::Reasoning(text) if text.is_empty() => {}
            Part::Reasoning(text) => lines.push(labeled(REASONING_LABEL, text)),
            Part::Tool {
                name,
                input,
                result,
                error,
            } => {
                let mut line = TOOL_CALL_LABEL.encode_utf16().collect::<Vec<u16>>();
                line.extend(name.encode_utf16());
                line.push(b'(' as u16);
                line.extend(input.encode_utf16());
                line.push(b')' as u16);
                lines.push(line);
                if let Some(error) = error {
                    let mut line = TOOL_ERROR_LABEL.encode_utf16().collect::<Vec<u16>>();
                    line.extend(error.encode_utf16());
                    lines.push(line);
                } else if let Some(result) = result {
                    let mut line = TOOL_RESULT_LABEL.encode_utf16().collect::<Vec<u16>>();
                    line.extend(truncate_tool_output_units(result));
                    lines.push(line);
                }
            }
        }
    }
    if lines.is_empty() {
        return None;
    }
    let mut entry: Vec<u16> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            entry.push(0x0A);
        }
        entry.extend_from_slice(line);
    }
    Some(entry)
}

/// One serialized conversation entry, in durable sequence order.
#[derive(Debug)]
pub(super) struct ConversationEntry {
    pub(super) seq: u64,
    /// True only for a user promoted input: a synthetic context entry is
    /// not a pullback anchor, matching the oracle type check.
    pub(super) user: bool,
    /// UTF-16 units, bounded by the materialization budget, not by the DTO
    /// string bound: only the published recent value is a public string.
    pub(super) text: Vec<u16>,
}

/// select plus planContent.recent: the walk, the pullback, the
/// latest-user rule and the summarize-recent branch. Returns the head,
/// the recent half, and the public value the compaction publishes. The
/// head is discarded material and the halves are internal: only the
/// public value must fit the DTO string bound.
pub(super) fn select_recent(
    entries: &[ConversationEntry],
    keep_tokens: u64,
    previous_recent: &Utf16Text,
) -> Result<(Vec<u16>, Vec<u16>, Utf16Text), String> {
    if entries.is_empty() {
        return Err("compaction conversation is empty".to_owned());
    }
    let mut split = walk_split(entries, keep_tokens);
    if split == 0 {
        if let Some(latest) = entries.iter().rposition(|entry| entry.user) {
            if latest > 0 {
                split = latest;
            }
        }
    }
    let head = join_entry_units(&entries[..split]);
    let recent = join_entry_units(&entries[split..]);
    let public = if previous_recent.units().is_empty() && head.is_empty() {
        Utf16Text::empty()
    } else {
        publish(&recent)?
    };
    Ok((head, recent, public))
}

/// The walk and the pullback, before the latest-user rule.
fn walk_split(entries: &[ConversationEntry], keep_tokens: u64) -> usize {
    let mut total = 0u64;
    let mut split = entries.len();
    for (index, entry) in entries.iter().enumerate().rev() {
        let next = total.saturating_add(estimate_tokens(&entry.text));
        if split < entries.len() && next > keep_tokens {
            break;
        }
        total = next;
        split = index;
    }
    while split > 0 {
        match entries.get(split) {
            Some(entry) if entry.user => break,
            Some(_) => split -= 1,
            None => break,
        }
    }
    split
}

/// The marker that opens a published recent value whose oldest text was
/// dropped to fit the DTO string bound.
pub(super) const RECENT_CLIPPED_MARKER: &str = "[Earlier recent context truncated]\n";

/// The published recent value. The keep budget is soft (the last entry is
/// always kept, and the split pulls back to a user entry), so the recent
/// half can outgrow the DTO string bound; compaction never fails on it.
/// Past the bound, the OLDEST text is dropped behind
/// the marker so the latest turns survive. A surrogate pair is never split.
fn publish(recent: &[u16]) -> Result<Utf16Text, String> {
    if let Ok(public) = Utf16Text::try_new(recent.to_vec()) {
        return Ok(public);
    }
    let marker: Vec<u16> = RECENT_CLIPPED_MARKER.encode_utf16().collect();
    // Utf16Text's own measure: a pair or a lone surrogate is four bytes.
    let budget =
        grapt_api::u3a::limits::MAX_DTO_STRING_BYTES.saturating_sub(RECENT_CLIPPED_MARKER.len());
    let mut used = 0usize;
    let mut start = recent.len();
    while start > 0 {
        let unit = recent[start - 1];
        let pair = (0xDC00..=0xDFFF).contains(&unit)
            && start >= 2
            && (0xD800..=0xDBFF).contains(&recent[start - 2]);
        let (cost, width) = if pair {
            (4, 2)
        } else {
            let cost = char::from_u32(u32::from(unit)).map_or(4, char::len_utf8);
            (cost, 1)
        };
        if used + cost > budget {
            break;
        }
        used += cost;
        start -= width;
    }
    let mut clipped = marker;
    clipped.extend_from_slice(&recent[start..]);
    Utf16Text::try_new(clipped).map_err(|error| error.to_owned())
}

/// The entries joined with the oracle blank-line separator.
fn join_entry_units(entries: &[ConversationEntry]) -> Vec<u16> {
    let mut units: Vec<u16> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            units.extend_from_slice(&[0x0A, 0x0A]);
        }
        units.extend_from_slice(&entry.text);
    }
    units
}

/// Why a recent window could not be read. Only a capacity refusal, built
/// where a bound is hit, lets a later start be tried: every other refusal
/// is evidence. Either way the text is the producer's own message.
#[derive(Debug)]
pub(super) enum WindowError {
    Capacity(String),
    Failed(String),
}

impl From<String> for WindowError {
    fn from(detail: String) -> Self {
        Self::Failed(detail)
    }
}

impl From<&str> for WindowError {
    fn from(detail: &str) -> Self {
        Self::Failed(detail.to_owned())
    }
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capacity(detail) | Self::Failed(detail) => formatter.write_str(detail),
        }
    }
}

impl From<WindowError> for String {
    fn from(error: WindowError) -> Self {
        match error {
            WindowError::Capacity(detail) | WindowError::Failed(detail) => detail,
        }
    }
}

/// Charge one serialized unit of the conversation against the budget,
/// refusing before the fetch when the row cannot fit.
pub(super) fn charge_recent(
    budget: &mut usize,
    charge: usize,
    refusal: &str,
) -> Result<(), WindowError> {
    if *budget < charge {
        return Err(WindowError::Capacity(refusal.to_owned()));
    }
    *budget -= charge;
    Ok(())
}

/// Charge the serialized promoted inputs before Inbox materializes their text.
fn promoted_rows(
    store: &EventStore,
    session: &SessionId,
    from: u64,
    until: u64,
    budget: &mut usize,
) -> Result<Vec<grapt_inbox::promoted_read::PromotedRow>, WindowError> {
    grapt_inbox::promoted_read::window_rows(
        store,
        session,
        EventSeq::new(from),
        EventSeq::new(until),
        grapt_inbox::promoted_read::PromotedReadLimits {
            max_rows: MAX_RECENT_WINDOW_ROWS,
            max_identity_bytes: MAX_RECENT_ID_BYTES,
        },
        budget,
        |budget, lengths| {
            let label = match lengths.kind {
                grapt_inbox::PromotedInputKind::User => USER_LABEL,
                grapt_inbox::PromotedInputKind::Synthetic => SYNTHETIC_LABEL,
            };
            let charge = lengths
                .identity_bytes
                .checked_add(lengths.kind_bytes)
                .and_then(|sum| sum.checked_add(label.len()))
                .and_then(|sum| sum.checked_add(lengths.text_bytes))
                .and_then(|sum| sum.checked_add(2))
                .ok_or("recent promoted inputs overflow the request context bound")?;
            charge_recent(
                budget,
                charge,
                "recent promoted inputs exceed the request context bound",
            )
        },
    )
}

/// The window conversation: promoted inputs plus the folded assistant
/// messages, each serialized per the oracle, in durable sequence order.
#[expect(clippy::too_many_lines, reason = "legacy")]
fn conversation_entries(
    inbox: &Inbox,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    from: u64,
    source_head: u64,
) -> Result<Vec<ConversationEntry>, WindowError> {
    let until = source_head
        .checked_add(1)
        .ok_or("recent window head overflows the sequence space")?;
    let mut budget: usize = MAX_RECENT_MATERIALIZATION_BYTES;
    let mut entries: Vec<ConversationEntry> = Vec::new();
    for row in promoted_rows(inbox.store(), session, from, until, &mut budget)? {
        let user = row.kind == grapt_inbox::PromotedInputKind::User;
        let label = if user { USER_LABEL } else { SYNTHETIC_LABEL };
        entries.push(ConversationEntry {
            seq: row.seq.value(),
            user,
            text: format!("{label}{}", row.text).encode_utf16().collect(),
        });
    }
    let from_seq = EventSeq::new(from);
    let until_seq = EventSeq::new(until);
    for (seq, text) in grapt_inbox::operation_mail::presented_rows_classified(
        inbox.store(),
        session,
        Some(from_seq),
        Some(until_seq),
        until,
        Some(&mut budget),
    )
    .map_err(|error| {
        use grapt_inbox::operation_mail::PresentedRowsError;
        match error {
            PresentedRowsError::Failed(detail) => WindowError::Failed(detail),
            capacity => WindowError::Capacity(capacity.to_string()),
        }
    })? {
        entries.push(ConversationEntry {
            seq,
            user: false,
            text: text.encode_utf16().collect(),
        });
    }
    let window = BoundedWindow {
        from: Some(from_seq),
        until: Some(until_seq),
        max_rows: MAX_RECENT_WINDOW_ROWS,
    };
    let calls = stored_calls_for(
        inbox.store(),
        session,
        artifacts,
        window,
        None,
        Some(&mut budget),
    )?;
    let outputs = stored_outputs_for(
        inbox.store(),
        session,
        artifacts,
        window,
        None,
        Some((&mut budget, &calls)),
    )?;
    let mut terminals: HashMap<ToolCallId, &StoredOutput> = HashMap::with_capacity(outputs.len());
    for output in &outputs {
        if terminals.insert(output.call.clone(), output).is_some() {
            return Err(format!(
                "tool {} has more than one durable terminal in the recent window",
                output.call
            )
            .into());
        }
    }
    for call in &calls {
        if let Some(output) = terminals.get(&call.call) {
            if output.seq <= call.seq {
                return Err(format!("tool {} terminal does not follow its call", call.call).into());
            }
        }
    }
    let call_ids: HashSet<&ToolCallId> = calls.iter().map(|call| &call.call).collect();
    for output in &outputs {
        if !call_ids.contains(&output.call) {
            return Err(format!(
                "tool {} terminal has no recorded call in the recent window",
                output.call
            )
            .into());
        }
    }
    let history = SessionHistoryStore::over(inbox.store());
    let folded = history
        .fold_window_from_bounded(
            session,
            from_seq,
            until_seq,
            MAX_RECENT_WINDOW_ROWS,
            MAX_RECENT_MATERIALIZATION_BYTES,
        )
        .map_err(|error| {
            let text = format!("recent assistant history is unreadable: {error}");
            match error {
                grapt_session::HistoryReadError::WindowDataTooLong { .. } => {
                    WindowError::Capacity(text)
                }
                _ => WindowError::Failed(text),
            }
        })?;
    let messages = folded
        .messages()
        .map_err(|error| format!("recent assistant history is malformed: {error}"))?;
    let calls_by_call: HashMap<&ToolCallId, &StoredCall> =
        calls.iter().map(|call| (&call.call, call)).collect();
    for message in messages {
        // The entry separator: two units, charged even for a message that
        // serializes to nothing, so the bound stays conservative.
        charge_recent(
            &mut budget,
            2,
            "recent conversation exceeds the request context bound",
        )?;
        let Some(content) = &message.content else {
            continue;
        };
        let mut parts: Vec<Part> = Vec::with_capacity(content.items().len());
        for held in content.items() {
            held.committed
                .as_ref()
                .ok_or("recent assistant content has no durable position")?;
            match held.item.kind() {
                ContentKind::Text => {
                    charge_recent(
                        &mut budget,
                        ASSISTANT_LABEL
                            .len()
                            .checked_add(held.item.payload().length() as usize)
                            .and_then(|sum| sum.checked_add(1))
                            .ok_or("recent conversation overflows the request context bound")?,
                        "recent conversation exceeds the request context bound",
                    )?;
                    let text = read_text(
                        artifacts,
                        held.item.payload(),
                        held.disposition,
                        "recent projection",
                    )?;
                    parts.push(Part::Text(text.as_str().encode_utf16().collect()));
                }
                ContentKind::Reasoning => {
                    charge_recent(
                        &mut budget,
                        REASONING_LABEL
                            .len()
                            .checked_add(held.item.payload().length() as usize)
                            .and_then(|sum| sum.checked_add(1))
                            .ok_or("recent conversation overflows the request context bound")?,
                        "recent conversation exceeds the request context bound",
                    )?;
                    let text = read_text(
                        artifacts,
                        held.item.payload(),
                        held.disposition,
                        "recent projection",
                    )?;
                    parts.push(Part::Reasoning(text.as_str().encode_utf16().collect()));
                }
                // No oracle representation; the position stays in the fold.
                ContentKind::Opaque => {}
                ContentKind::Tool { call } => {
                    let stored = calls_by_call
                        .get(call)
                        .filter(|stored| stored.message == message.message)
                        .ok_or_else(|| {
                            format!("tool {call} has no recorded call in the recent window")
                        })?;
                    let name = stored
                        .tool
                        .as_ref()
                        .map(|name| name.as_str().to_owned())
                        .ok_or_else(|| {
                            format!("tool {call} has no recorded name for the recent projection")
                        })?;
                    let input = stored.arguments.as_str().to_owned();
                    match terminals.get(call) {
                        Some(output) if output.error.is_some() => parts.push(Part::Tool {
                            name,
                            input,
                            result: None,
                            error: output.error.clone(),
                        }),
                        Some(output) => {
                            let units: Vec<u16> = output.output.as_str().encode_utf16().collect();
                            parts.push(Part::Tool {
                                name,
                                input,
                                result: Some(truncate_tool_output_units(&units)),
                                error: None,
                            });
                        }
                        None => parts.push(Part::Tool {
                            name,
                            input,
                            result: None,
                            error: None,
                        }),
                    }
                }
            }
        }
        if let Some(text) = serialize_assistant(&parts) {
            entries.push(ConversationEntry {
                seq: message.opened_at_seq,
                user: false,
                text,
            });
        }
    }
    entries.sort_by_key(|entry| entry.seq);
    for pair in entries.windows(2) {
        if pair[0].seq == pair[1].seq {
            return Err(
                format!("recent conversation entries share sequence {}", pair[0].seq).into(),
            );
        }
    }
    Ok(entries)
}

mod chain;
pub(in crate::dogfood) use chain::{recent_chain, ChainCursor, TokenEstimates};
mod window;
#[cfg(test)]
pub(super) use window::user_inputs_after;
use window::window_recent;

/// The live-path projection: the public recent value the in-progress
/// compaction will publish, from the committed chain's retained cut. The
/// caller holds the inbox guard; this function never re-locks.
pub(super) fn prepare_recent(
    inbox: &Inbox,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    head: u64,
) -> Result<Utf16Text, String> {
    let (window_from, previous_recent) = recent_chain(inbox, artifacts, session)?
        .last()
        .map_or((0u64, Utf16Text::empty()), |step| {
            (step.from, step.recent.clone())
        });
    window_recent(
        inbox,
        artifacts,
        session,
        window_from,
        head,
        &previous_recent,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One corpus recent value: the plain string form or the internal
    /// "secode.unit16" object form, decoded through the public DTO path.
    fn recent_text(value: &serde_json::Value) -> Result<Utf16Text, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    #[test]
    fn corpus_pins_the_oracle_selection() -> Result<(), String> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/session-recent-parity/cases.json"
        );
        let raw = std::fs::read_to_string(path).map_err(|error| {
            format!("the session-recent-parity corpus is absent or unreadable: {error}")
        })?;
        let doc: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|error| format!("the session-recent-parity corpus is malformed: {error}"))?;
        if doc.get("schema").and_then(serde_json::Value::as_str)
            != Some("session-recent-parity-corpus/v1")
        {
            return Err("the session-recent-parity corpus has an unexpected schema".to_owned());
        }
        let cases = doc
            .get("cases")
            .and_then(serde_json::Value::as_array)
            .ok_or("the session-recent-parity corpus has no cases")?;
        let mut empty_public = false;
        let mut nonempty_public = false;
        for case in cases {
            let id = case
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or("a corpus case has no id")?
                .to_owned();
            let keep_tokens = case
                .get("keep_tokens")
                .and_then(serde_json::Value::as_u64)
                .ok_or("a corpus case has no keep_tokens")?;
            let previous_recent = recent_text(
                case.get("previous_recent")
                    .ok_or("a corpus case has no previous_recent")?,
            )
            .map_err(|error| format!("case {id} previous_recent is not a recent value: {error}"))?;
            let rows = case
                .get("entries")
                .and_then(serde_json::Value::as_array)
                .ok_or("a corpus case has no entries")?;
            let entries: Vec<ConversationEntry> = rows
                .iter()
                .map(|row| {
                    let text = recent_text(row.get("text").ok_or("corpus entry has no text")?)
                        .map_err(|error| {
                            format!("corpus entry text is not a recent value: {error}")
                        })?;
                    Ok(ConversationEntry {
                        seq: row
                            .get("seq")
                            .and_then(serde_json::Value::as_u64)
                            .ok_or("corpus entry has no seq")?,
                        user: row
                            .get("user")
                            .and_then(serde_json::Value::as_bool)
                            .ok_or("corpus entry has no user")?,
                        text: text.units().to_vec(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let (head, recent, public) = select_recent(&entries, keep_tokens, &previous_recent)
                .map_err(|error| format!("case {id} refused: {error}"))?;
            let expected = |key: &str| -> Result<Utf16Text, String> {
                recent_text(
                    case.get(key)
                        .ok_or_else(|| format!("case {id} has no {key}"))?,
                )
                .map_err(|error| format!("case {id} {key} is not a recent value: {error}"))
            };
            assert_eq!(
                head,
                expected("expected_head")?.units(),
                "case {id}: head diverges from the frozen oracle"
            );
            assert_eq!(
                recent,
                expected("expected_recent")?.units(),
                "case {id}: recent diverges from the frozen oracle"
            );
            assert_eq!(
                public,
                expected("expected_public")?,
                "case {id}: public diverges from the frozen oracle"
            );
            if public.units().is_empty() {
                empty_public = true;
            } else {
                nonempty_public = true;
            }
        }
        if !empty_public {
            return Err("the corpus lost its legitimately empty publication".to_owned());
        }
        if !nonempty_public {
            return Err("the corpus lost its non-empty publication".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    pub(in crate::dogfood) static CHAIN_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
