//! The double's message pages: the product's paging, cursor and order rules,
//! over recorded messages or a synthetic long history.
//!
//! Moved out of `double.rs` when paging became real. The product pages by a
//! row limit (default 50) and cuts a page short at its encoded-byte bound, and
//! a reopened long session is exactly where a client that only ever saw one
//! page holding everything could pass every witness and still misread the
//! product.

use super::*;

/// One row of a synthetic long history. Each row's id is fixed by its
/// position, so a history that grows keeps every earlier identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Synthetic {
    /// A prompt.
    User,
    /// An assistant step that called a tool: the turn continues.
    Step,
    /// An assistant step with no content at all, which folds to no slice.
    Empty,
    /// An assistant reply that ended the turn.
    Reply,
    /// An assistant record that ended in an error alone, with no finish.
    Errored,
    /// A product notice, which carries no lifecycle.
    Notice,
    /// An assistant step holding one tool call, `call_` and its row index;
    /// running until `finish_tools` completes it.
    Tool,
    /// An assistant reply with a large body unrelated to any tool.
    Large,
    /// One assistant record of text, then a tool call (`call_` and its row
    /// index), so retention can keep its call and evict its text.
    TextThenTool,
    /// One assistant record of a tool call, then text: the reverse cut.
    ToolThenText,
}

/// A continuation the double minted: the order it continues and the ROW it
/// continues from, as the product anchors its cursor on a returned row.
///
/// The position is the history index the continuation starts at — below it
/// for `desc`, from it for `asc` — fixed by the returned rows rather than by
/// a count served, so rows appended while a continuation is outstanding do
/// not shift what it returns.
fn token(order: &str, from: usize) -> String {
    format!("h-{order}-{from}")
}

fn parse(token: &str) -> Option<(bool, usize)> {
    let rest = token.strip_prefix("h-")?;
    let (order, from) = rest.split_once('-')?;
    let ascending = match order {
        "asc" => true,
        "desc" => false,
        _ => return None,
    };
    Some((ascending, from.parse().ok()?))
}

impl ContractDouble {
    /// Message pages read so far.
    pub fn message_list_calls(&self) -> usize {
        self.message_list_calls
    }

    /// Serve `rows` as `session`'s whole durable history, in place of what it
    /// recorded.
    #[cfg(test)]
    pub(crate) fn seed_history(&mut self, session: &SessionWireId, rows: Vec<Synthetic>) {
        self.long_histories.insert(session.clone(), rows);
    }

    /// Append to a seeded history: a later turn, as the product records it.
    #[cfg(test)]
    pub(crate) fn extend_history(&mut self, session: &SessionWireId, rows: &[Synthetic]) {
        self.long_histories
            .entry(session.clone())
            .or_default()
            .extend_from_slice(rows);
    }

    /// Complete every synthetic tool call, as a later durable update would.
    #[cfg(test)]
    pub(crate) fn finish_tools(&mut self) {
        self.tools_finished = true;
    }

    /// Cut every page after `rows` rows while its cursor continues.
    #[cfg(test)]
    pub(crate) fn cut_pages_at(&mut self, rows: usize) {
        self.short_pages = Some(rows);
    }

    fn synthetic_row(
        &self,
        index: usize,
        row: Synthetic,
    ) -> Result<grapt_api::u3a::SessionMessageInfoDto, crate::wire::WireError> {
        use crate::wire::{DurableFinish, MessageItem};
        let id = format!("msg_{:012x}", index + 1);
        let text = |what: &str| vec![MessageItem::Text(format!("{what} {index}"))];
        match row {
            Synthetic::User => crate::wire::user_message(&id, &format!("turn {index}")),
            Synthetic::Step => crate::wire::assistant_message(
                &id,
                &text("step"),
                Some(DurableFinish::ToolCalls),
                Some((12, 7)),
                None,
            ),
            Synthetic::Empty => {
                crate::wire::assistant_message(&id, &[], Some(DurableFinish::ToolCalls), None, None)
            }
            Synthetic::Reply => crate::wire::assistant_message(
                &id,
                &text("reply"),
                Some(DurableFinish::Stop),
                Some((12, 7)),
                None,
            ),
            Synthetic::Errored => crate::wire::assistant_message(
                &id,
                &text("errored"),
                None,
                None,
                Some(crate::wire::DurableError {
                    kind: "provider".to_owned(),
                    message: "the provider failed".to_owned(),
                }),
            ),
            Synthetic::Notice => crate::wire::model_switched_message(&id),
            Synthetic::Tool => crate::wire::assistant_tool_message_in(
                &id,
                "execute",
                &format!("call_{index:012}"),
                if self.tools_finished {
                    crate::wire::ToolStateDouble::Completed {
                        input: "{}".to_owned(),
                        output: format!("done {index}"),
                    }
                } else {
                    crate::wire::ToolStateDouble::Running
                },
            ),
            Synthetic::TextThenTool | Synthetic::ToolThenText => {
                let tool = MessageItem::Tool(self.synthetic_tool(index)?);
                let text = MessageItem::Text(format!("words {index}"));
                let items = if row == Synthetic::TextThenTool {
                    [text, tool]
                } else {
                    [tool, text]
                };
                crate::wire::assistant_message(
                    &id,
                    &items,
                    Some(DurableFinish::ToolCalls),
                    None,
                    None,
                )
            }
            Synthetic::Large => crate::wire::assistant_message(
                &id,
                &[MessageItem::Text("x".repeat(64 * 1024))],
                Some(DurableFinish::Stop),
                Some((12, 7)),
                None,
            ),
        }
    }

    fn synthetic_tool(
        &self,
        index: usize,
    ) -> Result<crate::wire::DurableTool, crate::wire::WireError> {
        let input = "{}".to_owned();
        Ok(crate::wire::DurableTool {
            duration_ms: None,
            id: crate::wire::tool_call_id(&format!("call_{index:012}"))?,
            name: "execute".to_owned(),
            state: if self.tools_finished {
                crate::wire::DurableToolState::Completed {
                    input,
                    output: format!("done {index}"),
                }
            } else {
                crate::wire::DurableToolState::Running { input }
            },
            progress: Vec::new(),
        })
    }

    /// Every recorded row of `session`, oldest first.
    fn recorded_rows(
        &self,
        session: &SessionWireId,
    ) -> Result<Vec<grapt_api::u3a::SessionMessageInfoDto>, crate::wire::WireError> {
        let entry = self.sessions.get(session);
        let messages = entry.map(|e| e.messages.as_slice()).unwrap_or_default();
        let mut rows = Vec::new();
        for (index, message) in messages.iter().enumerate() {
            let id = format!("msg_{:012x}", index + 1);
            rows.push(match message {
                DoubleMessage::User(text, input) => {
                    crate::wire::user_message(input.as_deref().unwrap_or(&id), text)?
                }
                DoubleMessage::Interrupted => crate::wire::assistant_message(
                    &id,
                    &[],
                    None,
                    None,
                    Some(crate::wire::DurableError {
                        kind: "aborted".to_owned(),
                        message: "Step interrupted".to_owned(),
                    }),
                )?,
                DoubleMessage::Assistant(items, finish) => {
                    // The SAME counts the live `session.usage.updated` frame
                    // carries. Durable and live truth agreeing is the whole
                    // convergence claim, and a durable record with no usage
                    // would make a reconciled transcript differ from the
                    // streamed one in a way no witness compared.
                    crate::wire::assistant_message(&id, items, *finish, Some((12, 7)), None)?
                }
            });
        }
        Ok(rows)
    }

    /// One page of a session's durable history, in the shape the REAL product
    /// serves it.
    ///
    /// Properties modelled deliberately, because their absence let a client
    /// pass every in-crate witness and then misread the product:
    ///
    /// * A page with rows ALWAYS offers a `next` cursor, even when it carried
    ///   everything, and following it returns an empty page with no cursor of
    ///   its own. A client that read a cursor's presence as truncation declared
    ///   every complete transcript incomplete.
    /// * The default order is DESCENDING, newest first, and `order=asc` is
    ///   honoured.
    /// * A page holds at most `limit` rows (default 50), and may be cut shorter
    ///   while its cursor continues, as the product's byte bound cuts it.
    pub(super) fn message_page(
        &self,
        session: &SessionWireId,
        request: &WireRequest,
    ) -> Result<Vec<u8>, crate::wire::WireError> {
        let query = |key: &str| {
            request
                .query()
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        // A cursor CARRIES its own order, so a request that names both is
        // conflicting and the real product answers it 400. Refusing it here too
        // is what stops a client sending the combination in a witness that
        // passes and against a product that refuses.
        if query("cursor").is_some() && query("order").is_some() {
            return Err(crate::wire::WireError::Invalid {
                what: "message page query",
                value: "a cursor already carries its order; naming both conflicts".to_owned(),
            });
        }
        let (ascending, from) = match query("cursor") {
            Some(cursor) => {
                if self.fault == Fault::EndlessPages {
                    // One row and a DIFFERENT cursor each time, forever. Both
                    // parts matter: an endless run of empty pages would stop
                    // the client at the first one, and a repeated cursor would
                    // stop it as no-progress — so neither would reach the page
                    // bound this fault exists to prove.
                    let row = crate::wire::user_message("msg_endless", "and another")?;
                    let advanced = format!("{cursor}-more");
                    return crate::wire::encode_messages(vec![row], Some(&advanced));
                }
                if self.fault == Fault::StuckCursor {
                    // The SAME rows and the SAME cursor, forever: a producer
                    // that answers a continuation by ignoring the cursor.
                    let row = crate::wire::user_message("msg_stuck", "the same row again")?;
                    return crate::wire::encode_messages(vec![row], Some(cursor));
                }
                match parse(cursor) {
                    Some((ascending, from)) => (ascending, Some(from)),
                    // A cursor this producer never minted names nothing after
                    // it.
                    None => return crate::wire::encode_messages(Vec::new(), None),
                }
            }
            None => (query("order") == Some("asc"), None),
        };
        let limit = query("limit")
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|limit| (1..=200).contains(limit))
            .unwrap_or(50);
        let limit = self.short_pages.map_or(limit, |cut| cut.min(limit));

        let synthetic = self.long_histories.get(session);
        let total = match synthetic {
            Some(rows) => rows.len(),
            None => self.sessions.get(session).map_or(0, |e| e.messages.len()),
        };
        // This page's history indices, in reading order.
        let indices: Vec<usize> = if ascending {
            let start = from.unwrap_or(0);
            (start..total.min(start.saturating_add(limit))).collect()
        } else {
            let end = from.unwrap_or(total).min(total);
            (end.saturating_sub(limit)..end).rev().collect()
        };
        let mut rows = Vec::new();
        match synthetic {
            Some(history) => {
                for index in &indices {
                    rows.push(self.synthetic_row(*index, history[*index])?);
                }
            }
            None => {
                let mut recorded = self.recorded_rows(session)?;
                let mut by_index: Vec<Option<_>> = recorded.drain(..).map(Some).collect();
                for index in &indices {
                    if let Some(row) = by_index.get_mut(*index).and_then(Option::take) {
                        rows.push(row);
                    }
                }
            }
        }
        let order = if ascending { "asc" } else { "desc" };
        let continues = match (ascending, indices.last()) {
            (true, Some(last)) => last.saturating_add(1),
            (false, Some(last)) => *last,
            (true, None) => from.unwrap_or(0),
            (false, None) => from.unwrap_or(total),
        };
        // The first page always offers a cursor; a continuation offers one
        // only while it still carried rows.
        let next = (from.is_none() || !rows.is_empty()).then(|| token(order, continues));
        if from.is_none() && self.fault == Fault::UnrenderableRow {
            return Self::with_unrenderable_row(rows);
        }
        crate::wire::encode_messages(rows, next.as_deref())
    }
}
