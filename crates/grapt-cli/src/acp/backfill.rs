//! Ordered display recovery within one surviving ACP connection.
//!
//! The live stream is advisory: the product may end it mid-turn (a refused
//! oversize frame, a subscriber overflow, a reconcile), and the frames in the
//! gap are gone. ACP chunks only append, so after a gap a client's text can
//! no longer be patched in place. From the first gap on, live text is held
//! back, and when the turn ends the client is sent what durable history holds
//! beyond what it already received.

use serde_json::{json, Value};

use super::events::{remote, settle_page, SettlePage, MAX_SETTLE_PAGES};
use super::session::AttachedSession;
use super::{AcpService, RpcFault};

/// This prompt's history rows, or its not-run-yet notice when it is queued.
pub(super) enum PromptRows {
    Rows(Vec<Value>),
    Queued(String, String),
}

impl AcpService {
    /// After a gap in the live stream, send the client the assistant text of
    /// this prompt's turn that it has not received.
    pub(super) async fn backfill(
        &self,
        state: &AttachedSession,
        input: &str,
        delivery: &mut Delivery,
    ) -> Result<(), RpcFault> {
        if !delivery.had_gap() {
            return Ok(());
        }
        if let PromptRows::Rows(rows) = self.prompt_rows(state, input).await? {
            self.send_owed(state, &rows, delivery).await?;
        }
        Ok(())
    }

    /// Send what `delivery` still owes of `rows` (newest first).
    pub(super) async fn send_owed(
        &self,
        state: &AttachedSession,
        rows: &[Value],
        delivery: &mut Delivery,
    ) -> Result<(), RpcFault> {
        for row in rows.iter().rev() {
            if row.get("type").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let message = row
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcFault::internal("assistant history identity is malformed"))?;
            let parts = row
                .get("content")
                .and_then(Value::as_array)
                .ok_or_else(|| RpcFault::internal("assistant history content is malformed"))?;
            let mut text_ordinal = 0;
            let mut thought_ordinal = 0;
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text" | "reasoning") => {
                        let (kind, ordinal) = if part["type"] == "text" {
                            ("agent_message_chunk", &mut text_ordinal)
                        } else {
                            ("agent_thought_chunk", &mut thought_ordinal)
                        };
                        let text = part
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or_else(|| RpcFault::internal("history text is malformed"))?;
                        let key = (message.to_owned(), kind, *ordinal);
                        *ordinal += 1;
                        let sent = delivery.prefix(&key, text)?;
                        for text in chunks(&text[sent..]) {
                            delivery
                                .send_text(
                                    key.clone(),
                                    &text,
                                    self.update(
                                        state,
                                        json!({"sessionUpdate": kind, "messageId": message,
                                    "content": {"type": "text", "text": text}}),
                                    ),
                                )
                                .await?;
                        }
                    }
                    Some("tool") => self.recover_tool(state, part, delivery).await?,
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// This prompt's rows, newest-first, read back page by page (bounded,
    /// as `run` reads them). A queued prompt is not history yet, so a walk
    /// that meets this prompt's own not-run-yet notice first ends there;
    /// one that meets neither refuses rather than borrow an earlier answer.
    pub(super) async fn prompt_rows(
        &self,
        state: &AttachedSession,
        input: &str,
    ) -> Result<PromptRows, RpcFault> {
        let mut rows: Vec<Value> = Vec::new();
        let mut response = self
            .client()
            .get_value_query(
                &format!("api/session/{}/message", state.id),
                &[("order", "desc"), ("limit", "50")],
            )
            .await
            .map_err(remote)?;
        for _page in 0..MAX_SETTLE_PAGES {
            let page = response
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| RpcFault::internal("message history response is malformed"))?;
            match settle_page(page, input) {
                SettlePage::Prompt(position) => {
                    rows.extend(page[..position].iter().cloned());
                    return Ok(PromptRows::Rows(own_turn(rows)));
                }
                SettlePage::Queued(id, text) => {
                    return Ok(PromptRows::Queued(id.to_owned(), text.to_owned()))
                }
                SettlePage::Older => rows.extend(page.iter().cloned()),
            }
            let next = response
                .pointer("/cursor/next")
                .and_then(Value::as_str)
                .filter(|_| !page.is_empty())
                .map(str::to_owned);
            let Some(cursor) = next else {
                break;
            };
            response = self
                .client()
                .get_value(&format!("api/session/{}/message?cursor={cursor}", state.id))
                .await
                .map_err(remote)?;
        }
        Err(RpcFault::internal(
            "history never showed this prompt or its notice",
        ))
    }
}

/// The most ESCAPED bytes one backfilled chunk's text may take. ACP refuses
/// an outbound frame over MAX_STDIN_RECORD_BYTES (262,144); the notification
/// envelope (ids, kind, JSON framing) takes well under the rest. Budgeted on
/// the encoded text, not raw bytes: a control character escapes to six
/// (review AC-H1).
const OWED_CHUNK_BYTES: usize = 128 * 1_024;

/// The rows of this prompt's own turn, newest first: those older than any
/// later user input, whose turn a later prompt owns.
fn own_turn(mut rows: Vec<Value>) -> Vec<Value> {
    if let Some(later) = rows
        .iter()
        .rposition(|row| row.get("type").and_then(Value::as_str) == Some("user"))
    {
        rows.drain(..=later);
    }
    rows
}

/// `text` in char-boundary chunks whose JSON-escaped form each takes at
/// most [`OWED_CHUNK_BYTES`].
pub(super) fn chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut escaped = 0usize;
    for character in text.chars() {
        let width = escaped_len(character);
        if escaped + width > OWED_CHUNK_BYTES && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            escaped = 0;
        }
        current.push(character);
        escaped += width;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// The bytes `character` takes inside a JSON string as serde_json writes it.
fn escaped_len(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        control if u32::from(control) < 0x20 => 6,
        other => other.len_utf8(),
    }
}

mod delivery;
mod tools;
pub(crate) use delivery::Delivery;

#[cfg(test)]
mod tests;
