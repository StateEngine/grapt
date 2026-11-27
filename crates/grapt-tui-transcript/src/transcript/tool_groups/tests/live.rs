use super::*;

fn calls(count: usize) -> Result<Vec<Slice>> {
    (0..count)
        .map(|index| {
            let mut slice = tool(index, "exec_command", ToolPhase::Completed)?;
            let summary = slice.tool_summary.as_mut().ok_or("missing summary")?;
            summary.title = format!("Check step {index:02}");
            summary.preview = vec!["PRIVATE_PREVIEW".into()];
            Ok(slice)
        })
        .collect()
}

fn measured(transcript: &mut Transcript, width: u16) -> usize {
    (0..transcript.cells.len())
        .map(|index| transcript.height(index, width))
        .sum()
}

#[test]
fn completed_calls_remain_live_between_steps_then_fold_on_execution_end() -> Result {
    let mut transcript = Transcript::new();
    transcript.update(true, 0, calls(5)?);
    assert!(transcript.observe_running(true));
    for width in [1, 30, 120] {
        let height = measured(&mut transcript, width);
        let lines = transcript.visible(width, 100, Palette::new(false));
        assert_eq!(lines.len(), height);
        assert_eq!(height, 5);
        assert_eq!(lines.last().map(ToString::to_string).as_deref(), Some(""));
    }
    let text = visible(&mut transcript);
    for index in 2..5 {
        assert!(text.contains(&format!("Check step {index:02}")));
    }
    assert!(!text.contains("Check step 00") && !text.contains("Check step 01"));
    assert!(!text.contains("PRIVATE_") && !text.contains("RETAINED_OUTPUT"));
    assert!(transcript.observe_running(false));
    assert_eq!(measured(&mut transcript, 120), 2);
    assert!(!visible(&mut transcript).contains("Check step"));
    assert!(!transcript.observe_running(false));
    Ok(())
}

#[test]
fn long_silent_execution_splits_groups_and_keeps_only_the_new_tail_live() -> Result {
    let mut transcript = Transcript::new();
    transcript.observe_running(true);
    let rows = calls(27)?;
    for (index, slice) in rows.iter().cloned().enumerate() {
        transcript.update(false, index, vec![slice]);
        // Paint each intermediate state so a stale previous live heading is observable.
        visible(&mut transcript);
    }
    assert_eq!(
        transcript
            .tool_groups
            .runs
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![0, 12, 24]
    );
    let text = visible(&mut transcript);
    assert_eq!(text.lines().count(), 9);
    assert_eq!(text.lines().filter(|line| line.is_empty()).count(), 3);
    assert_eq!(text.matches("Ran 12 shell commands").count(), 2);
    assert!(text.contains("Ran 3 shell commands"));
    assert!(!text.contains("Check step 23") && text.contains("Check step 24"));
    assert_eq!(measured(&mut transcript, 120), 9);
    transcript.toggle_group(12);
    let text = visible(&mut transcript);
    assert!(text.contains("Check step 12") && text.contains("Check step 23"));
    assert!(!text.contains("RETAINED_OUTPUT"));
    transcript.toggle_tool(17);
    assert!(visible(&mut transcript).contains("RETAINED_OUTPUT_17"));
    transcript.reconcile(rows);
    assert!(transcript.cells[17].expanded);
    assert!(visible(&mut transcript).contains("Check step 26"));
    transcript.observe_running(false);
    assert!(!visible(&mut transcript).contains("Check step 26"));
    Ok(())
}

#[test]
fn prose_closes_live_titles_and_failure_cause_is_preserved() -> Result {
    let mut rows = calls(4)?;
    rows[2].tool_phase = Some(ToolPhase::Error);
    rows[2]
        .tool_summary
        .as_mut()
        .ok_or("missing summary")?
        .error = Some("Permission denied".into());
    let mut transcript = Transcript::new();
    transcript.update(true, 0, rows);
    transcript.observe_running(true);
    let lines = transcript.visible(120, 100, Palette::new(false));
    assert_eq!(
        lines[1].to_string(),
        "  └ Check step 02 failed: Permission denied"
    );
    assert!(lines[3].to_string().contains("Check step 02"));
    let mut prose = tool(4, "read", ToolPhase::Completed)?;
    prose.kind = SliceKind::Text;
    prose.tool_id = None;
    prose.tool_summary = None;
    prose.tool_phase = None;
    prose.text = "Progress report".into();
    transcript.update(false, 4, vec![prose]);
    let text = visible(&mut transcript);
    assert!(text.matches("Check step 02").count() == 1 && text.contains("Permission denied"));
    assert!(text.contains("Progress report"));
    transcript.update(false, 5, vec![tool(5, "read", ToolPhase::Running)?]);
    let text = visible(&mut transcript);
    assert!(text.matches("Called read").count() == 1 && text.matches("Check step 02").count() == 1);
    assert_eq!(measured(&mut transcript, 120), text.lines().count());
    Ok(())
}
