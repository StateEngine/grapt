//! Tool and promoted-input hints refresh matching rows, preserving live text.
//! The socket uses the existing bounded incremental HTTP codec and deadline.
use super::*;
use std::collections::BTreeSet;

#[derive(Debug)]
pub(super) struct Walk {
    session: SessionWireId,
    calls: BTreeSet<crate::wire::ToolCallWireId>,
    follow_up: BTreeSet<crate::wire::ToolCallWireId>,
    inputs: BTreeSet<crate::wire::MessageWireId>,
    follow_up_inputs: BTreeSet<crate::wire::MessageWireId>,
    found_inputs: Vec<(crate::wire::MessageWireId, crate::wire::DurableMessage)>,
    cursor: Option<String>,
    found: Vec<(crate::wire::DurableTool, bool)>,
    cache: crate::cache::CacheSummary,
    slices: usize,
    pages: usize,
    pending: bool,
}

impl<C: Connection> Client<C> {
    pub(super) fn cancel_tool_refresh(&mut self) {
        self.connection.cancel_tool_read();
        self.tool_refresh = None;
    }

    pub(super) fn request_promoted_input(&mut self, input: crate::wire::MessageWireId) {
        let Some(session) = self.selected.clone() else {
            return;
        };
        self.defer_queued_inputs();
        if let Some(walk) = self.tool_refresh.as_mut() {
            walk.follow_up_inputs.insert(input);
        } else {
            let mut walk = Walk::new(session, BTreeSet::new());
            walk.inputs.insert(input);
            self.tool_refresh = Some(walk);
        }
    }

    /// Whether the walk's page read is in flight.
    pub(super) fn tool_read_in_flight(&self) -> bool {
        self.tool_refresh.as_ref().is_some_and(|walk| walk.pending)
    }

    /// A cut hands the pump's tool hints to the walk without I/O, the one
    /// owner that survives it; the walk advances at the first live tail.
    pub(super) fn defer_tool_hints(&mut self, calls: BTreeSet<crate::wire::ToolCallWireId>) {
        let Some(session) = self.selected.clone().filter(|_| !calls.is_empty()) else {
            return;
        };
        match self.tool_refresh.as_mut() {
            Some(walk) if walk.session == session => walk.follow_up.extend(calls),
            _ => {
                self.cancel_tool_refresh();
                self.tool_refresh = Some(Walk::new(session, calls));
            }
        }
    }

    pub(super) fn promoted_input_pending(&self) -> bool {
        self.tool_refresh
            .as_ref()
            .is_some_and(|walk| !walk.inputs.is_empty() || !walk.follow_up_inputs.is_empty())
    }

    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn refresh_tool_rows(
        &mut self,
        calls: BTreeSet<crate::wire::ToolCallWireId>,
    ) -> Result<bool, ClientError> {
        let Some(session) = self.selected.clone() else {
            self.cancel_tool_refresh();
            return Ok(false);
        };
        if self
            .tool_refresh
            .as_ref()
            .is_some_and(|walk| walk.session != session)
        {
            self.cancel_tool_refresh();
        }
        if !calls.is_empty() {
            // Let the bounded current acquisition finish, even under a
            // continuous stream of progress hints. Re-read newer hints in
            // one coalesced follow-up rather than starving ready responses.
            if let Some(walk) = self.tool_refresh.as_mut() {
                walk.follow_up.extend(calls);
            } else {
                self.tool_refresh = Some(Walk::new(session.clone(), calls));
            }
        }
        if self.tool_refresh.as_ref().is_some_and(|walk| {
            walk.calls.len() + walk.inputs.len() > self.bounds.max_pending_events
                || walk.follow_up.len() + walk.follow_up_inputs.len()
                    > self.bounds.max_pending_events
        }) {
            self.cancel_tool_refresh();
            self.cursor.buffer_overflowed();
            // A refused refresh is a gap too: the transcript recovers through
            // the same re-read, so the notice must say so.
            self.live_data_refused();
            return Ok(true);
        }
        let Some(mut walk) = self.tool_refresh.take() else {
            return Ok(false);
        };
        // Embedded responses can complete immediately. A real socket yields
        // after starting or polling one bounded acquisition, including stalls.
        for _ in 0..self.bounds.max_message_pages {
            let response = if walk.pending {
                self.connection.poll_tool_read()?
            } else {
                let request = if self.history_window.is_some() {
                    paged_history::window_request(&session, walk.cursor.as_deref(), true)?
                } else {
                    let request = WireRequest::new(Operation::MessageList, Some(&session), None)?;
                    match walk.cursor.as_deref() {
                        Some(cursor) => request.with_query("cursor", cursor),
                        None => request.with_query("order", "desc"),
                    }
                };
                self.connection.begin_tool_read(&request)?
            };
            let Some(response) = response else {
                walk.pending = true;
                self.tool_refresh = Some(walk);
                return Ok(false);
            };
            walk.pending = false;
            walk.pages += 1;
            let bytes = self.accept_response(Operation::MessageList, response)?;
            let page = crate::wire::decode_messages(&bytes)?;
            if walk.cursor.is_some() && page.next == walk.cursor {
                return Err(crate::wire::WireError::Invalid {
                    what: "tool history",
                    value: "repeated cursor".to_owned(),
                }
                .into());
            }
            let mut window = crate::cache::CacheSummary::default();
            for message in page.messages.iter().rev() {
                if let Some(usage) = message.cache_usage() {
                    window.observe(usage);
                }
            }
            walk.cache.prepend(window);
            for (message, id) in page.messages.iter().zip(&page.ids) {
                if walk.inputs.contains(id) && !walk.found_inputs.iter().any(|(held, _)| held == id)
                {
                    walk.found_inputs.push((id.clone(), message.clone()));
                }
            }
            'rows: for message in &page.messages {
                let items = match message {
                    crate::wire::DurableMessage::Assistant { items, .. } => items.as_slice(),
                    _ => {
                        if walk.slices >= self.bounds.max_retained_slices {
                            break 'rows;
                        }
                        walk.slices += 1;
                        continue;
                    }
                };
                for item in items.iter().rev() {
                    if walk.slices >= self.bounds.max_retained_slices {
                        break 'rows;
                    }
                    walk.slices += 1;
                    if let crate::wire::MessageItem::Tool(tool) = item {
                        if walk.calls.contains(&tool.id)
                            && !walk.found.iter().any(|(held, _)| held.id == tool.id)
                        {
                            walk.found.push((tool.clone(), walk.cursor.is_none()));
                        }
                    }
                }
            }
            let complete = match page.next {
                Some(next)
                    if page.rows != 0
                        && walk.found.len() + walk.found_inputs.len()
                            < walk.calls.len() + walk.inputs.len()
                        && walk.slices < self.bounds.max_retained_slices
                        && walk.pages < self.bounds.max_message_pages =>
                {
                    walk.cursor = Some(next);
                    continue;
                }
                Some(_) if page.rows != 0 => false,
                _ => true,
            };
            if let Some(view) = self.views.get_mut(&session) {
                // Existing rows update in place; calls outside the retained
                // window stay with the ordinary older-history walk.
                for (tool, newest) in walk.found.iter().rev() {
                    if *newest
                        || view
                            .slices
                            .iter()
                            .any(|slice| slice.tool_id.as_ref() == Some(&tool.id))
                    {
                        view.push_tool(tool, self.bounds);
                    }
                }
                for (id, message) in walk.found_inputs.iter().rev() {
                    if matches!(message, crate::wire::DurableMessage::User { .. })
                        && !view.tail_rows.iter().any(|(held, _)| held == id)
                        && !self.promoted_inputs.contains(id)
                    {
                        let execution = view.execution.clone();
                        view.apply_promoted(message, self.bounds)?;
                        if matches!(execution, crate::projection::ExecutionState::Running { .. }) {
                            view.execution = execution;
                        }
                        self.promoted_inputs.push_back(id.clone());
                        if self.promoted_inputs.len() > self.bounds.max_retained_slices {
                            self.promoted_inputs.pop_front();
                        }
                    }
                }
                if complete {
                    walk.cache.history_complete(true);
                    view.cache.refresh(walk.cache);
                }
            }
            if !walk.inputs.is_empty() && walk.follow_up_inputs.is_empty() {
                if walk.found_inputs.len() == walk.inputs.len() {
                    self.request_queued_inputs(&session);
                } else {
                    self.queued_inputs_unavailable(&session, ClientError::NotOffered {
                        what: "promoted input is outside the bounded history read; reopen the session",
                    });
                }
            }
            if !walk.follow_up.is_empty() || !walk.follow_up_inputs.is_empty() {
                let mut next = Walk::new(session, walk.follow_up);
                next.inputs = walk.follow_up_inputs;
                self.tool_refresh = Some(next);
            }
            return Ok(true);
        }
        Ok(false)
    }
}

impl Walk {
    fn new(session: SessionWireId, calls: BTreeSet<crate::wire::ToolCallWireId>) -> Self {
        Self {
            session,
            calls,
            follow_up: BTreeSet::new(),
            inputs: BTreeSet::new(),
            follow_up_inputs: BTreeSet::new(),
            found_inputs: Vec::new(),
            cursor: None,
            found: Vec::new(),
            cache: crate::cache::CacheSummary::default(),
            slices: 0,
            pages: 0,
            pending: false,
        }
    }
}
