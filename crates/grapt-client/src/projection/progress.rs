//! Advisory MCP progress is bounded to this live view and discarded on recovery.
use super::*;
use grapt_api::u3a::ToolProgressData;

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Messages(
    std::collections::BTreeMap<
        crate::wire::ToolCallWireId,
        Vec<(crate::wire::ToolCallWireId, String)>,
    >,
);

impl Messages {
    pub(super) fn get(&self, parent: &crate::wire::ToolCallWireId) -> Option<&String> {
        self.0
            .get(parent)
            .and_then(|sources| sources.last().map(|(_, message)| message))
    }
    pub(super) fn remove(&mut self, parent: &crate::wire::ToolCallWireId) {
        self.0.remove(parent);
    }
    fn update(
        &mut self,
        parent: &crate::wire::ToolCallWireId,
        source: crate::wire::ToolCallWireId,
        message: &str,
        bounds: Bounds,
    ) -> bool {
        let known = self
            .0
            .get(parent)
            .is_some_and(|sources| sources.iter().any(|(id, _)| id == &source));
        if !known
            && !message.is_empty()
            && self.0.values().map(Vec::len).sum::<usize>() >= bounds.max_pending_events
        {
            return false;
        }
        let sources = self.0.entry(parent.clone()).or_default();
        sources.retain(|(id, _)| id != &source);
        if !message.is_empty() {
            sources.push((source, message.into()));
        }
        if sources.is_empty() {
            self.0.remove(parent);
        }
        true
    }
}

impl SessionView {
    pub(crate) fn record_tool_progress(&mut self, data: &ToolProgressData, bounds: Bounds) -> bool {
        let Ok(session) = crate::wire::session_id(data.session().as_str()) else {
            return false;
        };
        if session != self.session_id {
            return false;
        }
        let Ok(call) = crate::wire::tool_call_id(data.call().as_str()) else {
            return false;
        };
        if self
            .slices
            .iter()
            .any(|slice| slice.tool_id.as_ref() == Some(&call) && slice.closed)
        {
            return false;
        }
        let Some(message) = data
            .metadata()
            .calls()
            .iter()
            .find_map(|entry| entry.message())
        else {
            return false;
        };
        let source = data.source_call().unwrap_or(data.call());
        let Ok(source) = crate::wire::tool_call_id(source.as_str()) else {
            return false;
        };
        let previous = self.live_progress.get(&call).cloned();
        if !self.live_progress.update(&call, source, message, bounds) {
            return false;
        }
        let message = self
            .live_progress
            .get(&call)
            .map(String::as_str)
            .unwrap_or_default();
        let Some(slice) = self
            .slices
            .iter_mut()
            .find(|slice| slice.tool_id.as_ref() == Some(&call))
        else {
            return false;
        };
        if let Some(previous) = previous {
            let suffix = format!("\n  progress {previous}");
            if slice.text.ends_with(&suffix) {
                slice.text.truncate(slice.text.len() - suffix.len());
            }
        }
        let suffix = format!("\n  progress {message}");
        if !message.is_empty() && slice.text.len() + suffix.len() <= bounds.max_slice_bytes {
            slice.text.push_str(&suffix);
        }
        if let Some(summary) = slice.tool_summary.as_mut() {
            summary.result = message.into();
        }
        true
    }

    /// Take the live progress and its display off this view's tool rows,
    /// recording exactly what was taken so a certified resume can put it back.
    pub(super) fn clear_tool_progress(&mut self) -> (Messages, Vec<Stripped>) {
        let messages = std::mem::take(&mut self.live_progress);
        let mut stripped = Vec::new();
        for (call, sources) in &messages.0 {
            let Some((_, message)) = sources.last() else {
                continue;
            };
            if let Some(slice) = self
                .slices
                .iter_mut()
                .find(|slice| slice.tool_id.as_ref() == Some(call))
            {
                let suffix = format!("\n  progress {message}");
                let shown = slice.text.ends_with(&suffix);
                if shown {
                    slice.text.truncate(slice.text.len() - suffix.len());
                }
                let summarized = slice
                    .tool_summary
                    .as_mut()
                    .is_some_and(|summary| &summary.result == message);
                if let Some(summary) = slice.tool_summary.as_mut().filter(|_| summarized) {
                    summary.result.clear();
                }
                stripped.push(Stripped {
                    call: call.clone(),
                    message: message.clone(),
                    shown,
                    summarized,
                });
            }
        }
        (messages, stripped)
    }

    /// Put back exactly what [`Self::clear_tool_progress`] took.
    pub(super) fn restore_tool_progress(&mut self, messages: Messages, stripped: Vec<Stripped>) {
        self.live_progress = messages;
        for row in stripped {
            let Some(slice) = self
                .slices
                .iter_mut()
                .find(|slice| slice.tool_id.as_ref() == Some(&row.call))
            else {
                continue;
            };
            if row.shown {
                slice
                    .text
                    .push_str(&format!("\n  progress {}", row.message));
            }
            if let Some(summary) = slice.tool_summary.as_mut().filter(|_| row.summarized) {
                summary.result = row.message;
            }
        }
    }
}

/// What a cut took from one tool row's display.
#[derive(Debug)]
pub(super) struct Stripped {
    call: crate::wire::ToolCallWireId,
    message: String,
    /// The row's text ended with the progress line.
    shown: bool,
    /// The row's summary result was the progress message.
    summarized: bool,
}
