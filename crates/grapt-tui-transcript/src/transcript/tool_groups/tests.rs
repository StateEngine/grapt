use super::*;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
fn consecutive_completed_waits_fold_only_for_the_same_known_terminal() -> Result {
    let wait = |index, session: Option<&str>, phase| -> Result<Slice> {
        let mut view = grapt_client::SessionView::new(&grapt_client::wire::session_id("ses_wait")?);
        let message = grapt_client::wire::assistant_message(
            "msg_wait",
            &[grapt_client::wire::MessageItem::Tool(
                grapt_client::wire::DurableTool {
                    id: grapt_client::wire::tool_call_id(&format!("call_{index}"))?,
                    name: "write_stdin".into(),
                    duration_ms: Some(8000),
                    progress: vec![],
                    state: grapt_client::wire::DurableToolState::Completed {
                        input: session
                            .map_or_else(
                                || serde_json::json!({}),
                                |session| serde_json::json!({"session_id": session}),
                            )
                            .to_string(),
                        output: "retained output".into(),
                    },
                },
            )],
            Some(grapt_client::wire::DurableFinish::Stop),
            None,
            None,
        )?;
        let page = grapt_client::wire::decode_messages(&grapt_client::wire::encode_messages(
            vec![message],
            None,
        )?)?;
        view.apply_durable(
            page.messages.first().ok_or("wait message")?,
            grapt_client::Bounds::PRODUCTION,
        )?;
        let mut row = view
            .slices
            .into_iter()
            .find(|row| row.kind == SliceKind::Tool)
            .ok_or("wait slice")?;
        row.ordinal = index;
        row.tool_phase = Some(phase);
        row.closed = matches!(phase, ToolPhase::Completed | ToolPhase::Error);
        Ok(row)
    };
    let mut transcript = Transcript::new();
    let mut rows = vec![
        wait(0, Some("operation_a"), ToolPhase::Completed)?,
        wait(1, Some("operation_a"), ToolPhase::Completed)?,
        wait(2, Some("operation_b"), ToolPhase::Completed)?,
        wait(3, None, ToolPhase::Completed)?,
        wait(4, None, ToolPhase::Completed)?,
        wait(5, Some("operation_b"), ToolPhase::Running)?,
        wait(6, Some("operation_b"), ToolPhase::Error)?,
        wait(7, Some("operation_b"), ToolPhase::Completed)?,
        wait(8, Some("operation_b"), ToolPhase::Completed)?,
        wait(9, Some("operation_b"), ToolPhase::Completed)?,
        wait(10, Some("operation_b"), ToolPhase::Completed)?,
    ];
    rows[8].tool_summary.as_mut().ok_or("summary")?.title = "Another command".into();
    rows[9].tool_summary.as_mut().ok_or("summary")?.stdin =
        Some(grapt_client::projection::StdinAction::Write);
    transcript.update(true, 0, rows.clone());
    let folded = super::live::grouped_calls(&transcript.cells);
    assert_eq!(folded.len(), rows.len() - 1);
    assert_eq!(folded[0].1, 2);
    assert!(folded[1..].iter().all(|(_, count)| *count == 1));
    assert!(transcript
        .cells
        .iter()
        .zip(rows)
        .all(|(cell, row)| cell.slice == row));
    Ok(())
}

fn tool(index: usize, name: &str, phase: ToolPhase) -> Result<Slice> {
    Ok(Slice {
        compaction: None,
        footer: None,
        kind: SliceKind::Tool,
        ordinal: index,
        tool_id: Some(grapt_client::wire::tool_call_id(&format!("call_{index}"))?),
        tool_phase: Some(phase),
        tool_summary: Some(grapt_client::projection::ToolSummary {
            terminal_session: None,
            stdin: None,
            title: format!("Called {name}"),
            duration_ms: Some(8000),
            preview: vec![],
            omitted_lines: 0,
            name: name.into(),
            argument: "PRIVATE_COMMAND".into(),
            result: "80 output lines".into(),
            error: (phase == ToolPhase::Error).then(|| "Permission denied\nPRIVATE_STACK".into()),
        }),
        text: format!("RETAINED_OUTPUT_{index}"),
        read_detail_start: None,
        marks: vec![],
        closed: matches!(phase, ToolPhase::Completed | ToolPhase::Error),
        truncated_bytes: 0,
    })
}
fn visible(transcript: &mut Transcript) -> String {
    transcript
        .visible(120, 100, Palette::new(false))
        .iter()
        .map(|line| format!("{line}\n"))
        .collect()
}

#[test]
fn single_running_call_refreshes_and_clears_its_group_progress() -> Result {
    let mut transcript = Transcript::new();
    let mut row = tool(0, "execute", ToolPhase::Running)?;
    let title = row.tool_summary.as_ref().ok_or("summary")?.title.clone();
    for message in ["C 42%", "C 67%", ""] {
        row.tool_summary.as_mut().ok_or("summary")?.result = message.into();
        transcript.update(false, 0, vec![row.clone()]);
        let group = transcript.tool_groups.runs.get(&0).ok_or("group")?;
        if message.is_empty() {
            assert_eq!(group.label, title);
        } else {
            assert!(
                group.label.contains(message),
                "single-call progress was hidden"
            );
        }
    }
    row.tool_phase = Some(ToolPhase::Completed);
    row.closed = true;
    row.tool_summary.as_mut().ok_or("summary")?.result = "C 100%".into();
    transcript.update(false, 0, vec![row]);
    assert_eq!(
        transcript.tool_groups.runs.get(&0).ok_or("group")?.label,
        title
    );
    Ok(())
}

#[test]
fn mixed_calls_fold_to_one_dim_human_summary_and_keep_every_original_cell() -> Result {
    let names = [
        "exec_command",
        "shell",
        "shell",
        "read_lines",
        "read",
        "grep",
        "stateengine_resume",
        "stateengine_msg_read",
    ];
    let rows = names
        .iter()
        .enumerate()
        .map(|(i, name)| tool(i, name, ToolPhase::Completed))
        .collect::<Result<Vec<_>>>()?;
    let mut transcript = Transcript::new();
    transcript.update(true, 0, rows.clone());
    let lines = transcript.visible(120, 100, Palette::new(false));
    assert_eq!(lines.len(), 2);
    assert!(lines[0].to_string().contains(
        "Ran 3 shell commands, read 2 files, searched 1 pattern, called stateengine 2 times"
    ));
    assert_eq!(lines[0].style.fg, Some(Palette::new(false).muted));
    assert_eq!(transcript.cells.len(), rows.len());
    assert!(transcript
        .cells
        .iter()
        .zip(rows)
        .all(|(cell, source)| cell.slice == source));
    transcript.toggle_group(0);
    let expanded = visible(&mut transcript);
    assert!(!expanded.contains("RETAINED_OUTPUT") && expanded.contains("read_lines"));
    assert!(transcript.cells.iter().all(|cell| !cell.expanded));
    transcript.toggle_tool(0);
    assert!(visible(&mut transcript).contains("RETAINED_OUTPUT_0"));
    transcript.toggle_tool(4);
    assert!(visible(&mut transcript).contains("RETAINED_OUTPUT_4"));
    transcript.toggle_group(0);
    assert_eq!(transcript.visible(40, 20, Palette::new(false)).len(), 2);
    assert!(!visible(&mut transcript).contains("RETAINED_OUTPUT"));
    Ok(())
}

#[test]
fn prose_and_prompts_end_groups_and_reconciliation_keeps_disclosure() -> Result {
    let mut prose = tool(2, "read", ToolPhase::Completed)?;
    prose.kind = SliceKind::Text;
    prose.tool_id = None;
    prose.tool_phase = None;
    prose.tool_summary = None;
    prose.text = "ASSISTANT_PROSE".into();
    let rows = vec![
        tool(0, "shell", ToolPhase::Completed)?,
        tool(1, "read", ToolPhase::Completed)?,
        prose,
        tool(3, "grep", ToolPhase::Completed)?,
    ];
    let mut transcript = Transcript::new();
    transcript.update(true, 0, rows.clone());
    let shown = visible(&mut transcript);
    assert!(
        shown.contains("Ran 1 shell command, read 1 file")
            && shown.contains("ASSISTANT_PROSE")
            && shown.contains("Called grep")
    );
    transcript.toggle_group(0);
    transcript.toggle_tool(0);
    transcript.reconcile(rows);
    assert!(transcript.cells[0].expanded);
    assert!(visible(&mut transcript).contains("RETAINED_OUTPUT_0"));
    Ok(())
}

#[test]
fn child_completion_refreshes_the_group_error_and_stops_the_spinner() -> Result {
    let mut transcript = Transcript::new();
    transcript.update(true, 0, vec![tool(0, "shell", ToolPhase::Completed)?]);
    // Let the finished shell age, then start the read, so the group's start and
    // the running call have different ages.
    transcript.tick_compactions(Duration::from_secs(9));
    transcript.update(false, 1, vec![tool(1, "read", ToolPhase::Running)?]);
    assert!(transcript.tools_running());
    // The group starts at the finished shell; the row must name the tool that
    // is actually running and time it from that call, not the group's start.
    let (title, elapsed) = transcript
        .running_tool_elapsed()
        .ok_or("a running call is present")?;
    assert_eq!(
        title, "Called read",
        "the running call is named, not the group's first call"
    );
    assert_eq!(
        elapsed,
        Duration::ZERO,
        "timed from the read's own start, not the shell's"
    );
    let first = visible(&mut transcript);
    assert!(transcript.animate_tools(Duration::from_millis(100)));
    assert_ne!(visible(&mut transcript), first);
    assert!(!transcript.animate_tools(Duration::from_millis(101)));
    transcript.update(false, 1, vec![tool(1, "read", ToolPhase::Error)?]);
    assert!(!transcript.tools_running());
    assert!(
        transcript.running_tool_elapsed().is_none(),
        "a finished call is not reported as running"
    );
    assert!(!transcript.animate_tools(Duration::from_millis(200)));
    let lines = transcript.visible(80, 20, Palette::new(false));
    assert_eq!(lines.len(), 3);
    assert!(lines[0]
        .to_string()
        .contains("Ran 1 shell command, read 1 file"));
    assert_eq!(
        lines[1].to_string(),
        "  └ Called read failed: Permission denied"
    );
    transcript.update(false, 2, vec![tool(2, "grep", ToolPhase::Completed)?]);
    assert!(visible(&mut transcript).contains("searched 1 pattern"));
    Ok(())
}

#[test]
fn folded_failure_names_the_action_from_the_coding_audit_frame() -> Result {
    // run-04/frame 01: the eight-tool group hid the failed read's identity.
    let names = [
        "read",
        "read",
        "glob",
        "grep",
        "read",
        "apply_patch",
        "exec_command",
        "exec_command",
    ];
    let mut rows = names
        .iter()
        .enumerate()
        .map(|(index, name)| tool(index, name, ToolPhase::Completed))
        .collect::<Result<Vec<_>>>()?;
    rows[4] = tool(4, "read", ToolPhase::Error)?;
    let summary = rows[4].tool_summary.as_mut().ok_or("missing failed read")?;
    summary.title = "Read missing_config.py".into();
    summary.error = Some("Tool failed: EffectFailed\nPRIVATE_STACK".into());
    let mut transcript = Transcript::new();
    transcript.update(true, 0, rows);
    let lines = transcript.visible(120, 100, Palette::new(false));
    assert_eq!(lines.len(), 3);
    assert!(lines[0].to_string().contains(
        "Ran 2 shell commands, read 3 files, searched 1 pattern, listed 1 path, called 1 tool"
    ));
    assert_eq!(
        lines[1].to_string(),
        "  └ Read missing_config.py failed: Tool failed: EffectFailed"
    );
    assert!(!visible(&mut transcript).contains("PRIVATE_STACK"));
    for width in [0, 1, 20, 60, 120] {
        let height = transcript.height(0, width);
        let painted = transcript.visible(width, 100, Palette::new(false));
        assert_eq!(height, painted.len());
        assert!(painted
            .iter()
            .all(|line| line.width() <= usize::from(width)));
    }
    transcript.toggle_group(0);
    let expanded = visible(&mut transcript);
    assert!(expanded.contains("Read missing_config.py"));
    assert!(!expanded.contains("RETAINED_OUTPUT"));
    transcript.toggle_tool(4);
    assert!(visible(&mut transcript).contains("RETAINED_OUTPUT_4"));
    Ok(())
}

#[test]
fn first_failed_action_stays_identified_when_later_tools_fail() -> Result {
    let mut first = tool(0, "read", ToolPhase::Error)?;
    let summary = first.tool_summary.as_mut().ok_or("missing first failure")?;
    summary.title = "Read 界界 👩‍💻.py".into();
    summary.error = Some("Permission denied\nPRIVATE_STACK".into());
    let mut transcript = Transcript::new();
    transcript.update(true, 0, vec![first, tool(1, "shell", ToolPhase::Error)?]);
    assert!(visible(&mut transcript).contains("Read 界界 👩‍💻.py failed: Permission denied"));
    for width in 0..50 {
        let lines = transcript.visible(width, 100, Palette::new(false));
        assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
    }
    let mut missing = tool(0, "read", ToolPhase::Error)?;
    missing.tool_summary = None;
    transcript.update(true, 0, vec![missing]);
    assert!(visible(&mut transcript).contains("Tool failed: expand for details"));
    Ok(())
}

#[test]
fn preparing_calls_keep_disclosure_and_clip_unicode_safely() -> Result {
    let mut transcript = Transcript::new();
    transcript.update(
        true,
        0,
        vec![tool(0, "exec_command", ToolPhase::Streaming)?],
    );
    for expanded in [false, true] {
        if expanded {
            transcript.toggle_tool(0);
        }
        assert_eq!(transcript.cells[0].expanded, expanded);
        let lines = transcript.visible(20, 20, Palette::new(false));
        assert!(!lines
            .iter()
            .any(|line| line.to_string().contains("RETAINED_OUTPUT")));
    }
    for width in 0..30 {
        assert!(fit("⠋ Read 界界界 files 👩‍💻", width).width() <= usize::from(width));
    }
    Ok(())
}

#[test]
fn hidden_reasoning_between_calls_does_not_split_the_group() -> Result {
    let mut reasoning = tool(1, "read", ToolPhase::Completed)?;
    reasoning.kind = SliceKind::Reasoning;
    reasoning.tool_id = None;
    reasoning.tool_phase = None;
    reasoning.text = "PRIVATE_REASONING".into();
    let mut transcript = Transcript::new();
    transcript.update(
        true,
        0,
        vec![
            tool(0, "shell", ToolPhase::Completed)?,
            reasoning,
            tool(2, "read", ToolPhase::Completed)?,
        ],
    );
    let text = visible(&mut transcript);
    assert!(text.contains("Ran 1 shell command, read 1 file"));
    assert!(!text.contains("PRIVATE_REASONING"));
    assert_eq!(text.lines().filter(|line| line.contains('▸')).count(), 1);
    Ok(())
}

#[test]
fn opening_a_group_with_a_long_first_output_shows_all_headings_without_bodies() -> Result {
    let mut first = tool(0, "shell", ToolPhase::Completed)?;
    first.text = "LONG_PRIVATE_OUTPUT\n".repeat(10_000);
    let mut transcript = Transcript::new();
    transcript.update(
        true,
        0,
        vec![
            first,
            tool(1, "read", ToolPhase::Completed)?,
            tool(2, "grep", ToolPhase::Completed)?,
        ],
    );
    transcript.toggle_group(0);
    let text = visible(&mut transcript);
    assert!(text.contains("shell") && text.contains("read") && text.contains("grep"));
    assert!(!text.contains("PRIVATE_OUTPUT") && !text.contains("RETAINED_OUTPUT"));
    assert!(transcript.cells.iter().all(|cell| !cell.expanded));
    transcript.toggle_tool(0);
    assert!(visible(&mut transcript).contains("LONG_PRIVATE_OUTPUT"));
    transcript.toggle_group(0);
    assert!(!visible(&mut transcript).contains("PRIVATE_OUTPUT"));
    Ok(())
}

#[test]
fn unrelated_suffix_updates_keep_expanded_history_layouts_and_heights() -> Result {
    let mut rows = Vec::new();
    for index in 0..5000 {
        rows.push(tool(index * 2, "read", ToolPhase::Completed)?);
        let mut prose = tool(index * 2 + 1, "read", ToolPhase::Completed)?;
        prose.kind = SliceKind::Text;
        prose.tool_id = None;
        prose.tool_phase = None;
        prose.tool_summary = None;
        prose.text = "Prose between calls".into();
        rows.push(prose);
    }
    let mut transcript = Transcript::new();
    transcript.update(true, 0, rows);
    transcript.toggle_group(0);
    transcript.toggle_tool(0);
    transcript.from_top = true;
    visible(&mut transcript);
    let cache = transcript.cells[0]
        .cache
        .as_ref()
        .ok_or("history was not rendered")?;
    let allocation = cache.rendered.as_ptr();
    let epochs = cache.row_epochs.clone();
    let height = transcript.cells[0].height;
    let mut prose = transcript.cells[9999].slice.clone();
    prose.ordinal = 10_000;
    transcript.update(false, 10_000, vec![prose]);
    transcript.update(false, 9998, vec![tool(9998, "read", ToolPhase::Error)?]);
    let cache = transcript.cells[0]
        .cache
        .as_ref()
        .ok_or("unrelated update evicted history")?;
    assert_eq!(cache.rendered.as_ptr(), allocation);
    assert_eq!(cache.row_epochs, epochs);
    assert_eq!(transcript.cells[0].height, height);
    Ok(())
}

#[test]
fn animation_updates_only_the_header_of_an_expanded_first_tool() -> Result {
    let mut first = tool(0, "shell", ToolPhase::Completed)?;
    first.text = "RETAINED_LONG_OUTPUT\n".repeat(1000);
    let mut transcript = Transcript::new();
    transcript.update(true, 0, vec![first, tool(1, "read", ToolPhase::Running)?]);
    transcript.toggle_group(0);
    transcript.toggle_tool(0);
    transcript.from_top = true;
    visible(&mut transcript);
    let cache = transcript.cells[0]
        .cache
        .as_ref()
        .ok_or("missing expanded layout")?;
    let allocation = cache.rendered.as_ptr();
    let body = cache.row_epochs[1..].to_vec();
    let before = cache.rendered[0].to_string();
    assert!(transcript.animate_tools(Duration::from_millis(100)));
    let cache = transcript.cells[0]
        .cache
        .as_ref()
        .ok_or("spinner discarded the tool body")?;
    assert_eq!(cache.rendered.as_ptr(), allocation);
    assert_eq!(cache.row_epochs[1..], body);
    assert_ne!(cache.rendered[0].to_string(), before);
    Ok(())
}

#[test]
fn audit_paragraphs_and_group_boundary_have_one_gap_in_every_disclosure() -> Result {
    use ratatui::{backend::TestBackend, widgets::Paragraph, Terminal};
    // Audit run-04, 01-first-turn-folded: the final three paragraphs ran
    // together and the first touched the group summary. Keep the audit prose.
    let mut reply = tool(3, "read", ToolPhase::Completed)?;
    reply.kind = SliceKind::Text;
    reply.tool_id = None;
    reply.tool_phase = None;
    reply.tool_summary = None;
    reply.text = "Signed addition is fixed. All four tests passed.\n\nThe optional config file was absent; that read failed without affecting the change. The long validation log completed successfully.\n\nThe change keeps negative operands signed instead of applying absolute values.".into();
    for light in [false, true] {
        for width in [40, 114] {
            for details in [0, 1, 2] {
                let p = Palette::new(light);
                let mut transcript = Transcript::new();
                transcript.update(
                    true,
                    0,
                    vec![
                        tool(0, "shell", ToolPhase::Completed)?,
                        tool(1, "read", ToolPhase::Error)?,
                        reply.clone(),
                    ],
                );
                if details > 0 {
                    transcript.toggle_group(0);
                }
                if details > 1 {
                    transcript.toggle_tool(0);
                    transcript.toggle_tool(1);
                }
                let heights: Vec<_> = (0..3).map(|i| transcript.height(i, width)).collect();
                let rows = transcript.visible(width, 100, p);
                assert_eq!(rows.len(), heights.iter().sum::<usize>());
                for (i, height) in heights.into_iter().enumerate() {
                    assert_eq!(transcript.layout(i, width, p).len(), height);
                }
                let text: Vec<_> = rows.iter().map(ToString::to_string).collect();
                for phrase in [
                    "● Signed addition",
                    "The optional config",
                    "The change keeps",
                ] {
                    let index = text
                        .iter()
                        .position(|line| line.contains(phrase))
                        .ok_or("audit paragraph missing")?;
                    assert!(index >= 2 && text[index - 1].is_empty());
                    assert!(
                        !text[index - 2].is_empty(),
                        "exactly one gap before {phrase}"
                    );
                }
                if details > 0 {
                    let first = transcript.layout(0, width, p);
                    assert!(
                        !first.last().is_some_and(|row| row.width() == 0),
                        "no gap after an intermediate tool preview/body"
                    );
                }
                for top in [false, true] {
                    for scroll in 0..rows.len() {
                        transcript.scroll = scroll;
                        transcript.from_top = top;
                        let expected = transcript.visible(width, 5, p);
                        let mut actual = Terminal::new(TestBackend::new(width, 5))?;
                        let mut reference = Terminal::new(TestBackend::new(width, 5))?;
                        actual.draw(|f| {
                            let area = f.area();
                            transcript.paint(f, area, p);
                        })?;
                        reference.draw(|f| {
                            f.render_widget(Paragraph::new(expected.clone()), f.area())
                        })?;
                        assert_eq!(
                            actual
                                .backend()
                                .buffer()
                                .content
                                .iter()
                                .map(|c| c.symbol())
                                .collect::<Vec<_>>(),
                            reference
                                .backend()
                                .buffer()
                                .content
                                .iter()
                                .map(|c| c.symbol())
                                .collect::<Vec<_>>(),
                            "{width} details={details} scroll={scroll} top={top}"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn growing_a_group_moves_its_separator_and_invalidates_the_old_last_tool() -> Result {
    let p = Palette::new(false);
    let mut transcript = Transcript::new();
    transcript.update(true, 0, vec![tool(0, "shell", ToolPhase::Completed)?]);
    transcript.toggle_group(0);
    transcript.toggle_tool(0);
    assert!(transcript
        .layout(0, 80, p)
        .last()
        .is_some_and(|row| row.width() == 0));
    transcript.update(false, 1, vec![tool(1, "read", ToolPhase::Completed)?]);
    assert!(!transcript
        .layout(0, 80, p)
        .last()
        .is_some_and(|row| row.width() == 0));
    assert!(transcript
        .layout(1, 80, p)
        .last()
        .is_some_and(|row| row.width() == 0));
    for i in 0..2 {
        let expected = transcript.height(i, 80);
        assert_eq!(transcript.layout(i, 80, p).len(), expected);
    }
    transcript.toggle_group(0);
    assert_eq!(transcript.visible(80, 20, p).len(), 2);
    Ok(())
}

#[test]
fn an_unchanged_group_end_keeps_its_expanded_body_cache() -> Result {
    let mut last = tool(1, "read", ToolPhase::Completed)?;
    last.text = "LONG_RETAINED_OUTPUT\n".repeat(1000);
    let mut transcript = Transcript::new();
    transcript.update(true, 0, vec![tool(0, "shell", ToolPhase::Running)?, last]);
    transcript.toggle_group(0);
    transcript.toggle_tool(1);
    transcript.from_top = true;
    visible(&mut transcript);
    let cache = transcript.cells[1]
        .cache
        .as_ref()
        .ok_or("last body not rendered")?;
    let allocation = cache.rendered.as_ptr();
    let epochs = cache.row_epochs.clone();
    transcript.update(false, 0, vec![tool(0, "shell", ToolPhase::Completed)?]);
    let cache = transcript.cells[1]
        .cache
        .as_ref()
        .ok_or("unchanged group end lost its body cache")?;
    assert_eq!(cache.rendered.as_ptr(), allocation);
    assert_eq!(cache.row_epochs, epochs);
    Ok(())
}
mod live;
