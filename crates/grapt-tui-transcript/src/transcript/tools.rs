//! A compact tool row discloses retained detail only on explicit expansion.
use super::*;

/// Refresh the advisory header without replacing a running call's source.
pub(super) fn update_progress(old: &mut Cell, next: &Slice) -> bool {
    if old.slice.tool_phase != Some(ToolPhase::Running)
        || next.tool_phase != Some(ToolPhase::Running)
        || old.slice.tool_id.is_none()
        || old.slice.tool_id != next.tool_id
    {
        return false;
    }
    let (Some(previous), Some(current)) =
        (old.slice.tool_summary.as_mut(), next.tool_summary.as_ref())
    else {
        return false;
    };
    if previous.result == current.result {
        return false;
    }
    previous.result.clone_from(&current.result);
    old.cache = None;
    old.height = None;
    true
}

pub(super) fn summary(
    slice: &Slice,
    expanded: bool,
    width: u16,
    palette: Palette,
    focused: bool,
) -> Vec<Line<'static>> {
    let disclosure = if expanded { "▾" } else { "▸" };
    let failed = slice.tool_phase == Some(ToolPhase::Error);
    let tool = slice.tool_summary.as_ref();
    let title = tool.map(|tool| tool.title.as_str()).unwrap_or("Tool");
    let elapsed = tool
        .and_then(|tool| tool.duration_ms)
        .map(|ms| format!(" · {:.1}s", ms as f64 / 1000.0))
        .unwrap_or_default();
    let progress = tool
        .filter(|_| slice.tool_phase == Some(ToolPhase::Running))
        .map(|tool| tool.result.as_str())
        .filter(|message| !message.is_empty());
    let status = match slice.tool_phase {
        Some(ToolPhase::Streaming) => " · preparing",
        Some(ToolPhase::Running) => " · running",
        Some(ToolPhase::Error) => " · failed",
        Some(ToolPhase::Completed) => "",
        None => " · status unavailable",
    };
    let mut style = if failed {
        palette
            .normal()
            .fg(palette.muted)
            .add_modifier(Modifier::DIM)
            .remove_modifier(Modifier::BOLD)
    } else {
        palette
            .normal()
            .fg(palette.muted)
            .remove_modifier(Modifier::BOLD)
    };
    if focused {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    let status = progress
        .map(|message| format!(" · {message}"))
        .unwrap_or_else(|| status.into());
    let mut rows = vec![Line::styled(
        tool_groups::fit(&format!("● {title}{elapsed}{status} {disclosure}"), width),
        style,
    )];
    let dim = palette
        .normal()
        .fg(palette.muted)
        .add_modifier(Modifier::DIM)
        .remove_modifier(Modifier::BOLD);
    if let Some(tool) = tool {
        if expanded
            && matches!(tool.name.as_str(), "shell" | "exec_command" | "execute")
            && !tool.argument.is_empty()
            && slice.tool_phase != Some(ToolPhase::Streaming)
        {
            rows.push(Line::styled(
                tool_groups::fit(&format!("  {}", tool.argument), width),
                dim,
            ));
        }
        if failed {
            if let Some(error) = &tool.error {
                rows.push(Line::styled(
                    tool_groups::fit(&format!("  └ {error}"), width),
                    dim,
                ));
            }
        }
    }
    if failed && tool.is_none() {
        rows.push(Line::styled(
            tool_groups::fit("  └ Tool failed: expand for details", width),
            dim,
        ));
    }
    rows
}

/// The human heading replaces the projection's transport heading in expanded detail.
/// Keep the source and mark coordinates intact for retention and diff styling.
pub(super) fn detail<'a>(slice: &Slice, text: &'a str) -> (&'a str, usize) {
    if slice.kind == SliceKind::Tool && slice.tool_summary.is_some() && text.starts_with("tool ") {
        return (
            text.split_once('\n')
                .map(|(_, body)| body)
                .unwrap_or_default(),
            1,
        );
    }
    (text, 0)
}

impl Transcript {
    pub(super) fn collapsed_tool(&mut self, index: usize, width: u16, palette: Palette) -> bool {
        let cell = &self.cells[index];
        if cell.slice.kind != SliceKind::Tool
            || (cell.expanded
                && cell.slice.tool_phase != Some(ToolPhase::Streaming)
                && !self.closed_group(index))
        {
            return false;
        }
        self.paint_epoch = self.paint_epoch.saturating_add(1);
        let rendered = self.tool_heading(
            index,
            cell.expanded,
            width,
            palette,
            self.tool_focus == Some(disclosure::Focus::Tool(index)),
        );
        let height = rendered.len();
        let cell = &mut self.cells[index];
        cell.cache = Some(CachedOutput {
            width,
            theme: palette.light,
            rendered,
            row_epochs: vec![self.paint_epoch; height],
            source_end: cell.shown,
            markdown: None,
        });
        cell.height = Some((width, height));
        self.remember_cache(index);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tool(phase: ToolPhase) -> Result<Slice, Box<dyn std::error::Error>> {
        Ok(Slice {
            compaction: None,
            kind: SliceKind::Tool,
            tool_id: Some(grapt_client::wire::tool_call_id("call_do_not_show")?),
            tool_phase: Some(phase),
            tool_summary: Some(grapt_client::projection::ToolSummary {
                terminal_session: None,
                stdin: None,
                title: "Run bounded command".into(),
                duration_ms: Some(8000),
                preview: vec![],
                omitted_lines: 0,
                error: None,
                name: "shell".into(),
                argument: "se --help".into(),
                result: "113 output lines".into(),
            }),
            ordinal: 0,
            text: "tool shell completed call_do_not_show\n  output HIDDEN_HELP_DETAIL".into(),
            read_detail_start: None,
            marks: Vec::new(),
            closed: phase == ToolPhase::Completed || phase == ToolPhase::Error,
            footer: None,
            truncated_bytes: 0,
        })
    }
    #[test]
    fn live_progress_refreshes_the_same_running_call_without_replacing_its_source(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let original = tool(ToolPhase::Running)?;
        let mut transcript = Transcript::new();
        transcript.update(true, 0, vec![original.clone()]);
        for message in ["A 42%", "B 75%", ""] {
            let mut next = original.clone();
            let summary = next.tool_summary.as_mut().ok_or("metadata")?;
            summary.result = message.into();
            summary.title = "replacement title".into();
            summary.argument = "replacement argument".into();
            next.text = "replacement source".into();
            assert!(transcript.update(false, 0, vec![next]));
            let rows = transcript.recent_rows(1);
            let retained = rows.last().ok_or("retained tool")?;
            let summary = retained.tool_summary.as_ref().ok_or("retained metadata")?;
            assert_eq!(summary.result, message);
            assert_eq!(summary.title, "Run bounded command");
            assert_eq!(summary.argument, "se --help");
            assert_eq!(retained.text, original.text);
        }
        transcript.update(false, 0, vec![tool(ToolPhase::Completed)?]);
        assert_eq!(
            transcript.recent_rows(1)[0].tool_phase,
            Some(ToolPhase::Completed)
        );
        Ok(())
    }

    #[test]
    fn completed_and_failed_tools_start_compact_and_expand_only_explicitly(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for phase in [ToolPhase::Completed, ToolPhase::Error] {
            let mut transcript = Transcript::new();
            transcript.update(true, 0, vec![tool(phase)?]);
            let rows = transcript.visible(120, 40, Palette::new(false));
            let line = rows[0].to_string();
            assert!(
                line.contains("Run bounded command")
                    && !line.contains("se --help")
                    && !line.contains("F8")
            );
            assert!(!line.contains("call_do_not_show") && !line.contains("HIDDEN_HELP_DETAIL"));
            if phase == ToolPhase::Error {
                assert!(line.contains("failed"));
            }
            transcript.toggle_group(0);
            assert!(!transcript
                .visible(120, 40, Palette::new(false))
                .iter()
                .any(|row| row.to_string().contains("HIDDEN_HELP_DETAIL")));
            transcript.toggle_tool(0);
            let rows = transcript.visible(120, 40, Palette::new(false));
            assert!(rows
                .iter()
                .any(|row| row.to_string().contains("  │  output HIDDEN_HELP_DETAIL")));
            transcript.toggle_tool(0);
            assert!(!transcript
                .visible(120, 40, Palette::new(false))
                .iter()
                .any(|row| row.to_string().contains("HIDDEN_HELP_DETAIL")));
        }
        Ok(())
    }
    #[test]
    fn narrow_rows_keep_the_tool_name_before_optional_result_metadata(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rows = summary(
            &tool(ToolPhase::Completed)?,
            false,
            40,
            Palette::new(false),
            false,
        );
        let text = rows.iter().map(ToString::to_string).collect::<String>();
        assert!(
            text.contains("Run bounded command")
                && text.contains("8.0s")
                && !text.contains("completed")
        );
        assert!(rows.iter().all(|row| row.width() <= 40));
        Ok(())
    }
    #[test]
    fn terminal_update_preserves_disclosure_for_the_same_call_and_collapses_a_new_call(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut transcript = Transcript::new();
        transcript.update(true, 0, vec![tool(ToolPhase::Running)?]);
        transcript.visible(120, 40, Palette::new(false));
        transcript.toggle_group(0);
        transcript.toggle_tool(0);
        transcript.update(false, 0, vec![tool(ToolPhase::Completed)?]);
        assert!(transcript.visible(120, 40, Palette::new(false)).len() > 1);
        let mut next = tool(ToolPhase::Completed)?;
        next.tool_id = Some(grapt_client::wire::tool_call_id("call_next")?);
        transcript.update(false, 0, vec![next]);
        assert_eq!(transcript.visible(120, 40, Palette::new(false)).len(), 2);
        Ok(())
    }
    #[test]
    fn human_titles_hide_raw_commands_and_output_until_expanded(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut slice = tool(ToolPhase::Completed)?;
        let metadata = slice.tool_summary.as_mut().ok_or("missing metadata")?;
        metadata.title = "Inspect built-in shell help".into();
        metadata.preview = vec![
            "FIRST_OUTPUT".into(),
            "SECOND_OUTPUT".into(),
            "THIRD_OUTPUT".into(),
        ];
        metadata.omitted_lines = 110;
        let rows = summary(&slice, false, 80, Palette::new(false), false);
        assert!(rows[0]
            .to_string()
            .contains("Inspect built-in shell help · 8.0s"));
        assert_eq!(rows.len(), 1);
        assert!(!rows
            .iter()
            .any(|line| line.to_string().contains("completed")
                || line.to_string().contains("113 output lines")));
        let expanded = summary(&slice, true, 80, Palette::new(false), false);
        assert!(expanded[1].to_string().contains("se --help"));
        assert!(!expanded
            .iter()
            .any(|line| line.to_string().contains("FIRST_OUTPUT")));
        Ok(())
    }
    #[test]
    fn expanded_changes_replace_transport_ids_and_keep_diff_colors_and_height(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut slice = tool(ToolPhase::Completed)?;
        slice.text = "tool edit completed call_private\n-removed\n+added".into();
        slice.marks = vec![Mark::Plain, Mark::Delete, Mark::Insert];
        let mut transcript = Transcript::new();
        transcript.update(true, 0, vec![slice]);
        transcript.toggle_group(0);
        transcript.toggle_tool(0);
        let rows = transcript.visible(80, 40, Palette::new(false));
        assert!(!rows
            .iter()
            .any(|row| row.to_string().contains("call_private")));
        assert!(rows.iter().any(|row| row.to_string().contains("-removed")
            && row
                .spans
                .iter()
                .any(|span| span.style.fg == Some(Palette::new(false).bad))));
        assert!(rows.iter().any(|row| row.to_string().contains("+added")
            && row
                .spans
                .iter()
                .any(|span| span.style.fg == Some(Palette::new(false).good))));
        assert_eq!(transcript.height(0, 80), rows.len());
        Ok(())
    }

    #[test]
    fn audited_shell_log_expands_as_literal_lines_and_keeps_scroll_height(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../grapt-client/tests/fixtures/expanded-shell-audit.json"
        )))?;
        let captured = &fixture["tool"];
        let tool = grapt_client::wire::DurableTool {
            id: grapt_client::wire::tool_call_id(captured["id"].as_str().ok_or("id missing")?)?,
            name: captured["name"].as_str().ok_or("name missing")?.into(),
            duration_ms: None,
            progress: vec![],
            state: grapt_client::wire::DurableToolState::Completed {
                input: serde_json::to_string(&captured["state"]["input"])?,
                output: captured["state"]["content"][0]["text"]
                    .as_str()
                    .ok_or("output missing")?
                    .into(),
            },
        };
        let message = grapt_client::wire::DurableMessage::Assistant {
            items: vec![grapt_client::wire::MessageItem::Tool(tool)],
            finish: None,
            error: None,
            usage: None,
            cache: Box::default(),
            footer: None,
        };
        let mut view =
            grapt_client::SessionView::new(&grapt_client::wire::session_id("ses_000000000001")?);
        view.apply_durable(&message, grapt_client::Bounds::PRODUCTION)?;
        let slice = view
            .slices
            .into_iter()
            .find(|slice| slice.tool_id.is_some())
            .ok_or("tool missing")?;
        for width in [30, 120] {
            let palette = Palette::new(false);
            let mut transcript = Transcript::new();
            transcript.update(true, 0, vec![slice.clone()]);
            transcript.toggle_group(0);
            let preview = transcript.visible(width, 1000, palette);
            assert!(!preview
                .iter()
                .any(|row| row.to_string().contains("check_079")));
            transcript.toggle_tool(0);
            let rows = transcript.visible(width, 1000, palette);
            assert_eq!(transcript.height(0, width), rows.len());
            let text = rows
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("check_000") && text.contains("check_079"));
            assert!(text.contains("exit 0") && !text.contains("\\ncheck_000"));
            assert!(!text.contains("\"cleanup\""));
            transcript.from_top = true;
            let top = transcript.visible(width, 10, palette);
            assert!(top
                .iter()
                .any(|row| row.to_string().contains("cmd python3")));
            transcript.from_top = false;
            let bottom = transcript.visible(width, 10, palette);
            assert!(bottom
                .iter()
                .any(|row| row.to_string().contains("check_079")));
        }
        Ok(())
    }
}
