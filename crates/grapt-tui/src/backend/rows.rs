//! Retention shifts invalidate positional patches even when the length plateaus.
use grapt_client::{Slice, SliceKind};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct RowSync {
    sent: usize,
    front: Option<usize>,
    mutable: BTreeMap<usize, Slice>,
}

pub(super) struct Delta {
    pub reset: bool,
    pub start: usize,
    pub rows: Vec<Slice>,
    pub patches: Vec<(usize, Slice)>,
}

impl RowSync {
    pub fn diff(&mut self, rows: &[Slice], reset: bool) -> Delta {
        let front = rows.first().map(|row| row.ordinal);
        let reset = reset || rows.len() < self.sent || front != self.front;
        let start = if reset {
            0
        } else {
            self.sent.saturating_sub(1)
        };
        let mut patches = Vec::new();
        if reset {
            self.mutable.clear();
        }
        for (index, old) in &mut self.mutable {
            if let Some(row) = rows.get(*index) {
                if row != old {
                    patches.push((*index, row.clone()));
                    *old = row.clone();
                }
            }
        }
        // A coalesced batch may close an earlier text/reasoning part after a
        // newer part was appended. Keep it until that terminal patch is sent.
        self.mutable.retain(|_, row| {
            matches!(row.kind, SliceKind::Tool | SliceKind::AssistantFooter) || !row.closed
        });
        for (index, row) in rows.iter().enumerate().skip(start) {
            if matches!(row.kind, SliceKind::Tool | SliceKind::AssistantFooter) || !row.closed {
                self.mutable.entry(index).or_insert_with(|| row.clone());
            }
        }
        self.sent = rows.len();
        self.front = front;
        Delta {
            reset,
            start,
            rows: rows.get(start..).unwrap_or_default().to_vec(),
            patches,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_client::{
        wire::{DurableMessage, DurableTool, DurableToolState, MessageItem},
        Bounds, SessionView,
    };

    #[test]
    fn late_cost_updates_patch_a_completed_footer_before_newer_replies(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rows = grapt_client::wire::encode_messages(
            vec![grapt_client::wire::assistant_message(
                "msg_000000000001",
                &[],
                Some(grapt_client::wire::DurableFinish::Stop),
                Some((10, 2)),
                None,
            )?],
            None,
        )?;
        let page = grapt_client::wire::decode_messages(&rows)?;
        let mut view = SessionView::new(&grapt_client::wire::session_id("ses_000000000001")?);
        view.apply_durable(&page.messages[0], Bounds::PRODUCTION)?;
        let mut sync = RowSync::default();
        sync.diff(&view.slices, true);
        view.apply_durable(
            &DurableMessage::User {
                text: "Later input".into(),
            },
            Bounds::PRODUCTION,
        )?;
        sync.diff(&view.slices, false);
        let footer = view.slices[0].footer.as_mut().ok_or("footer missing")?;
        footer.estimated_usd = Some(0.0002);
        let delta = sync.diff(&view.slices, false);
        assert!(!delta.reset);
        assert_eq!(delta.patches.len(), 1);
        assert_eq!(delta.patches[0].0, 0);
        assert_eq!(
            delta.patches[0]
                .1
                .footer
                .as_deref()
                .and_then(|f| f.estimated_usd),
            Some(0.0002)
        );
        assert!(sync.diff(&view.slices, false).patches.is_empty());
        Ok(())
    }

    #[test]
    fn closing_text_before_another_part_is_delivered_as_a_patch(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let row = |kind, text: &str, closed, ordinal| Slice {
            compaction: None,
            kind,
            text: text.into(),
            closed,
            ordinal,
            tool_id: None,
            tool_phase: None,
            tool_summary: None,
            read_detail_start: None,
            marks: Vec::new(),
            footer: None,
            truncated_bytes: 0,
        };
        let mut rows = vec![
            row(SliceKind::Text, "", false, 0),
            row(SliceKind::Tool, "tool", false, 1),
        ];
        let mut sync = RowSync::default();
        sync.diff(&rows, true);
        rows[0].text = "Assistant prose before tool".into();
        rows[0].closed = true;
        rows[1].closed = true;
        rows.push(row(SliceKind::Text, "Final answer", true, 2));
        let delta = sync.diff(&rows, false);
        assert!(
            delta.patches.iter().any(|(index, row)| *index == 0
                && row.closed
                && row.text == "Assistant prose before tool"),
            "a coalesced completion must not strand the earlier empty/open prose row"
        );
        Ok(())
    }

    #[test]
    fn mixed_stream_batches_publish_all_prose_and_drop_reasoning_noise() {
        let row = |kind, text: &str, closed, ordinal| Slice {
            compaction: None,
            kind,
            text: text.into(),
            closed,
            ordinal,
            tool_id: None,
            tool_phase: None,
            tool_summary: None,
            read_detail_start: None,
            marks: Vec::new(),
            footer: None,
            truncated_bytes: 0,
        };
        let mut sync = RowSync::default();
        let mut transcript = grapt_tui_transcript::transcript::Transcript::new();
        let mut rows = vec![
            row(SliceKind::Reasoning, "", false, 0),
            row(SliceKind::Text, "", false, 1),
            row(SliceKind::Tool, "Tool output", false, 2),
        ];
        for phase in 0..3 {
            if phase == 1 {
                rows[0].text = "Synthetic summary must stay hidden".into();
                rows[0].closed = true;
                rows[1].text = "Prose before tool without newline".into();
                rows[1].closed = true;
                rows[2].closed = true;
                rows[2].tool_phase = Some(grapt_client::projection::ToolPhase::Completed);
                rows.push(row(SliceKind::Text, "After tool", false, 3));
            } else if phase == 2 {
                rows[3].text.push_str(" prose and final answer");
                rows[3].closed = true;
            }
            let delta = sync.diff(&rows, phase == 0);
            transcript.update(delta.reset, delta.start, delta.rows);
            for (index, row) in delta.patches {
                transcript.update(false, index, vec![row]);
            }
            for _ in 0..10 {
                transcript.drain();
            }
        }
        let text = transcript
            .visible(120, 100, grapt_tui_transcript::palette::Palette::new(false))
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Prose before tool without newline"), "{text}");
        assert!(text.contains("After tool prose and final answer"), "{text}");
        assert!(
            !text.contains("Reasoning") && !text.contains("Synthetic summary"),
            "{text}"
        );
        assert!(sync.mutable.values().all(|row| row.kind == SliceKind::Tool));
    }

    #[test]
    fn eviction_at_fifty_thousand_resets_rows_and_reindexes_tools() {
        let session = grapt_client::wire::session_id("ses_retention")
            .unwrap_or_else(|e| panic!("session: {e}"));
        let mut view = SessionView::new(&session);
        let bounds = Bounds {
            max_retained_slices: 50_000,
            ..Bounds::PRODUCTION
        };
        let user = DurableMessage::User {
            text: "message".into(),
        };
        view.apply_durable(
            &DurableMessage::User {
                text: "evicted sentinel".into(),
            },
            bounds,
        )
        .unwrap_or_else(|e| panic!("first row: {e}"));
        for _ in 0..49_998 {
            view.apply_durable(&user, bounds)
                .unwrap_or_else(|e| panic!("row: {e}"));
        }
        let mut tool = DurableTool {
            duration_ms: None,
            id: grapt_client::wire::tool_call_id("call_retention")
                .unwrap_or_else(|e| panic!("tool: {e}")),
            name: "shell".into(),
            state: DurableToolState::Running { input: "{}".into() },
            progress: vec![],
        };
        let message = |tool| DurableMessage::Assistant {
            items: vec![MessageItem::Tool(tool)],
            finish: None,
            error: None,
            usage: None,
            cache: Box::default(),
            footer: None,
        };
        view.apply_durable(&message(tool.clone()), bounds)
            .unwrap_or_else(|e| panic!("tool row: {e}"));
        let mut sync = RowSync::default();
        let mut transcript = grapt_tui_transcript::transcript::Transcript::new();
        let delta = sync.diff(&view.slices, true);
        transcript.update(delta.reset, delta.start, delta.rows);
        view.apply_durable(&user, bounds)
            .unwrap_or_else(|e| panic!("append: {e}"));
        assert_eq!(view.slices.len(), 50_000);
        assert_eq!(view.dropped_slices, 1);
        let delta = sync.diff(&view.slices, false);
        assert!(delta.reset);
        assert_eq!(delta.start, 0);
        assert_eq!(delta.rows.first().map(|r| r.ordinal), Some(1));
        assert!(delta.patches.is_empty());
        transcript.update(delta.reset, delta.start, delta.rows);
        transcript.from_top = true;
        let first = transcript.visible(80, 20, grapt_tui_transcript::palette::Palette::new(false));
        assert!(!first.iter().any(|line| line
            .spans
            .iter()
            .any(|s| s.content.contains("evicted sentinel"))));
        transcript.from_top = false;
        tool.state = DurableToolState::Completed {
            input: "{}".into(),
            output: "finished".into(),
        };
        view.apply_durable(&message(tool), bounds)
            .unwrap_or_else(|e| panic!("settle: {e}"));
        let delta = sync.diff(&view.slices, false);
        assert!(!delta.reset);
        assert_eq!(delta.patches.len(), 1);
        assert_eq!(delta.patches[0].0, 49_998);
        for (index, row) in delta.patches {
            transcript.update(false, index, vec![row]);
        }
        let lines = transcript.visible(80, 20, grapt_tui_transcript::palette::Palette::new(false));
        assert!(lines
            .iter()
            .any(|line| line.to_string().contains("Ran command")));
        assert!(!lines
            .iter()
            .any(|line| line.to_string().contains("finished")));
        transcript.toggle_group(49_998);
        transcript.toggle_tool(49_998);
        let lines = transcript.visible(80, 20, grapt_tui_transcript::palette::Palette::new(false));
        assert!(lines
            .iter()
            .any(|line| line.spans.iter().any(|s| s.content.contains("finished"))));
    }
}
