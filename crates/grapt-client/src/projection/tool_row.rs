//! One durable tool call as a transcript row, updated in place by identity.
use super::*;

impl SessionView {
    /// Present one tool call, or update the one already on screen.
    ///
    /// DEDUPLICATION IS BY IDENTITY, not by position. Reconciliation re-lists
    /// pages this client may already hold, so the same invocation arrives more
    /// than once; appending would show one `read` twice and invent work that
    /// never happened. An existing row for the same id is REPLACED in place, so
    /// a call seen `running` and later `completed` becomes one row that
    /// progressed rather than two rows that disagree.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(crate) fn push_tool(&mut self, tool: &crate::wire::DurableTool, bounds: Bounds) {
        use crate::wire::DurableToolState as T;
        let phase = match &tool.state {
            T::Streaming { .. } => ToolPhase::Streaming,
            T::Running { .. } => ToolPhase::Running,
            T::Completed { .. } => ToolPhase::Completed,
            T::Error { .. } => ToolPhase::Error,
        };
        let closed = matches!(tool.state, T::Completed { .. } | T::Error { .. });
        if !closed
            && self
                .slices
                .iter()
                .any(|slice| slice.tool_id.as_ref() == Some(&tool.id) && slice.closed)
        {
            return;
        }

        // The status word is the contract's own, not a paraphrase.
        let (status, input, output, error) = match &tool.state {
            T::Streaming { input } => ("streaming", input.as_str(), None, None),
            T::Running { input } => ("running", input.as_str(), None, None),
            T::Completed { input, output } => {
                ("completed", input.as_str(), Some(output.as_str()), None)
            }
            T::Error {
                input,
                error,
                output,
            } => ("error", input.as_str(), output.as_deref(), Some(error)),
        };

        let mut text = format!("tool {} {}  {}", tool.name, status, tool.id.spelled());
        for row in &tool.progress {
            use grapt_api::u3a::ToolProgressStatus as P;
            let status = match row.status() {
                P::Running => "running",
                P::Completed => "completed",
                P::Error => "error",
            };
            if let Some(message) = row.message() {
                text.push_str(&format!("\n  {message}"));
            } else {
                text.push_str(&format!("\n  {} {status}", row.tool()));
            }
        }
        // A JSON object is presented as its FIELDS, one per row.
        //
        // An attended run captured a completed edit as a single raw JSON
        // row: the retained client showed what actually changed, and this one
        // showed a serialisation. The fields are what a reader wants -- for an
        // edit they ARE the before and the after -- and one per row makes them
        // legible and lets the renderer wrap them individually.
        //
        // Keyed on SHAPE, not on tool name. Hard-coding `edit`'s schema would
        // present that one tool nicely and leave every other tool, including
        // ones this client has never seen, as raw JSON.
        //
        // The wire types a tool input as `OpenObject`, so a non-object cannot
        // arrive; the `None` path is reached by an EMPTY object, which falls
        // back to showing `input {}` rather than a row that looks like a tool
        // that took no arguments because the client dropped them.
        fn fields(raw: &str) -> Option<Vec<(String, String)>> {
            let serde_json::Value::Object(map) = serde_json::from_str(raw).ok()? else {
                return None;
            };
            if map.is_empty() {
                return None;
            }
            Some(
                map.into_iter()
                    .map(|(key, value)| {
                        // A string renders as its text, not as a quoted and
                        // escaped literal: the escaping is the serialisation's,
                        // and showing it is what made this unreadable.
                        let shown = match value {
                            serde_json::Value::String(text) => text,
                            other => other.to_string(),
                        };
                        (key, shown)
                    })
                    .collect(),
            )
        }
        // The ARGUMENTS are shown because they are what the user approved. A
        // client that gates a tool behind a permission and then hides what it
        // ran is asking for authority over something invisible.
        match fields(input) {
            Some(pairs) => {
                for (key, value) in pairs {
                    text.push_str(
                        "
  ",
                    );
                    text.push_str(&key);
                    text.push(' ');
                    text.push_str(&value);
                }
            }
            None => {
                text.push_str(
                    "
  input ",
                );
                text.push_str(input);
            }
        }
        if let Some(output) = output {
            if !output.is_empty() {
                if let Some(shown) = exec::present(&tool.name, output) {
                    text.push_str(&shown);
                } else {
                    text.push_str(
                        "
  output ",
                    );
                    text.push_str(output);
                }
            }
        }
        if let Some(error) = error {
            // The product's own account of the failure, rendered rather than
            // counted, for the reason a `system` row is rendered.
            text.push_str(
                "
  error ",
            );
            text.push_str(&error.kind);
            text.push_str(": ");
            text.push_str(&error.message);
        }

        if closed {
            self.live_progress.remove(&tool.id);
        } else if let Some(message) = self.live_progress.get(&tool.id) {
            text.push_str(&format!("\n  progress {message}"));
        }
        let first = text.split('\n').next().unwrap_or_default().to_owned();
        let (text, detail_start, marks) = match plan::present(tool) {
            Some(text) => (text, None, Vec::new()),
            None => match read::present(tool) {
                Some((text, start)) => (text, Some(start), Vec::new()),
                None => match diff::present(tool, &first) {
                    Some((text, marks)) => (text, None, marks),
                    None => (text, None, Vec::new()),
                },
            },
        };
        let mut summary = tool_summary::for_view(tool, &self.slices);
        if let Some(message) = self.live_progress.get(&tool.id) {
            summary.result.clone_from(message);
        }
        if let Some(existing) = self
            .slices
            .iter_mut()
            .find(|slice| slice.tool_id.as_ref() == Some(&tool.id))
        {
            let mut retained = String::new();
            let mut truncated_bytes = 0usize;
            if text.len() <= bounds.max_slice_bytes {
                retained.push_str(&text);
            } else {
                let mut cut = bounds.max_slice_bytes;
                while cut > 0 && !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                retained.push_str(&text[..cut]);
                truncated_bytes = text.len() - cut;
            }
            existing.text = retained;
            existing.read_detail_start = detail_start.map(|start| start.min(existing.text.len()));
            existing.marks = marks;
            existing.closed = closed;
            existing.tool_phase = Some(phase);
            existing.tool_summary = Some(summary);
            existing.truncated_bytes = truncated_bytes;
            return;
        }

        self.push_slice(SliceKind::Tool, &text, bounds);
        if let Some(slice) = self.slices.last_mut() {
            slice.read_detail_start = detail_start.map(|start| start.min(slice.text.len()));
            slice.tool_id = Some(tool.id.clone());
            slice.tool_phase = Some(phase);
            slice.tool_summary = Some(summary);
            slice.marks = marks;
            slice.closed = closed;
        }
    }
}
