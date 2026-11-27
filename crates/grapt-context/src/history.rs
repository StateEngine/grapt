//! Bounded history pages over a session-scoped durable reader.
use grapt_types::{EventSeq, MessageId, ToolCallId};

const LIST_ROWS: usize = 32;
const OUTPUT_PAGE_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub struct CallRow {
    pub seq: EventSeq,
    pub data: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Completed,
    Failed,
}

#[derive(Debug)]
pub struct ListMetadata {
    pub name: String,
    pub outcome: Outcome,
    pub output_bytes: u64,
}

/// The application binds this reader to one session and owns tool formatting.
/// Reads must retain their existing durable validation and artifact bounds.
pub trait Source {
    fn output(&self, call: &ToolCallId) -> Result<String, String>;
    /// Read at most `limit` candidate rows, ordered by sequence after `cursor`.
    /// Metadata is projected only for the selected page, never its lookahead row.
    fn calls_after(&self, cursor: EventSeq, limit: usize) -> Result<Vec<CallRow>, String>;
    fn metadata(
        &self,
        seq: EventSeq,
        message: &MessageId,
        call: &ToolCallId,
    ) -> Result<Option<ListMetadata>, String>;
}

/// Recover one output page, or list one bounded page of settled top-level calls.
/// A listing cursor is deliberately ignored when an exact call is requested.
pub fn page(
    source: &impl Source,
    call: Option<&ToolCallId>,
    cursor: Option<u64>,
    offset: Option<u64>,
) -> Result<Vec<u8>, String> {
    if let Some(call) = call {
        let output = source.output(call)?;
        let offset = usize::try_from(offset.unwrap_or(0))
            .map_err(|_| "history output offset exceeds this platform".to_owned())?;
        if offset > output.len() {
            return Err(format!(
                "history output offset {offset} exceeds total {} bytes",
                output.len()
            ));
        }
        if !output.is_char_boundary(offset) {
            return Err("history output offset is not a UTF-8 boundary".to_owned());
        }
        let mut end = offset.saturating_add(OUTPUT_PAGE_BYTES).min(output.len());
        while !output.is_char_boundary(end) {
            end = end
                .checked_sub(1)
                .ok_or("history output boundary underflow")?;
        }
        return serde_json::to_vec(&serde_json::json!({
            "call": call.as_str(),
            "content": &output[offset..end],
            "totalBytes": output.len(),
            "offset": offset,
            "nextOffset": (end < output.len()).then_some(end),
        }))
        .map_err(|error| error.to_string());
    }
    if offset.is_some() {
        return Err("history output offset requires a call identity".to_owned());
    }
    let rows = source.calls_after(EventSeq::new(cursor.unwrap_or(0)), LIST_ROWS + 1)?;
    let has_more = rows.len() > LIST_ROWS;
    let mut calls = Vec::with_capacity(rows.len().min(LIST_ROWS));
    let mut next_cursor = None;
    for row in rows.into_iter().take(LIST_ROWS) {
        let value = serde_json::from_str(&row.data)
            .map_err(|error| format!("history call is not JSON: {error}"))?;
        let fact = grapt_event::session_message::decode_appended(&value)
            .map_err(|error| format!("history call is corrupt: {error}"))?;
        let grapt_types::ContentKind::Tool { call } = fact.item().kind() else {
            return Err("history list selected non-tool content".to_owned());
        };
        if let Some(metadata) = source.metadata(row.seq, fact.message(), call)? {
            calls.push(serde_json::json!({
                "call": call.as_str(),
                "tool": metadata.name,
                "outcome": metadata.outcome,
                "outputBytes": metadata.output_bytes,
            }));
        }
        next_cursor = Some(row.seq.value());
    }
    serde_json::to_vec(&serde_json::json!({
        "calls": calls,
        "nextCursor": has_more.then_some(next_cursor).flatten(),
    }))
    .map_err(|error| error.to_string())
}
