use super::*;

fn part(kind: SliceKind, text: &str, closed: bool, ordinal: usize) -> Slice {
    Slice {
        compaction: None,
        kind,
        ordinal,
        text: text.into(),
        closed,
        tool_id: None,
        tool_phase: None,
        tool_summary: None,
        read_detail_start: None,
        marks: Vec::new(),
        footer: None,
        truncated_bytes: 0,
    }
}

fn shown(transcript: &mut Transcript) -> String {
    transcript
        .visible(120, 100, Palette::new(false))
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn mixed_assistant_prose_survives_adjacent_compact_tools_and_final_flush() {
    let mut transcript = Transcript::new();
    let reasoning = part(SliceKind::Reasoning, "Summary body", true, 0);
    let before = part(SliceKind::Text, "Before tool without newline", false, 1);
    transcript.update(true, 0, vec![reasoning, before.clone()]);
    let mut before = before;
    before.closed = true;
    let mut tool = part(SliceKind::Tool, "Private tool output", true, 2);
    tool.tool_phase = Some(ToolPhase::Completed);
    transcript.update(
        false,
        1,
        vec![
            before,
            tool,
            part(SliceKind::Text, "After tool prose", true, 3),
            part(SliceKind::Text, "Final answer", true, 4),
        ],
    );
    for _ in 0..10 {
        transcript.drain();
    }
    let text = shown(&mut transcript);
    for required in [
        "Before tool without newline",
        "After tool prose",
        "Final answer",
    ] {
        assert!(text.contains(required), "{text}");
    }
    assert!(!text.contains("Private tool output"));
}

#[test]
fn reasoning_has_no_transcript_rows_or_disclosure() {
    let mut transcript = Transcript::new();
    transcript.update(
        true,
        0,
        vec![part(SliceKind::Reasoning, "Summary body 中", true, 0)],
    );
    let collapsed = shown(&mut transcript);
    assert!(collapsed.is_empty(), "{collapsed}");
    transcript.toggle_tool(0);
    let expanded = shown(&mut transcript);
    assert!(expanded.is_empty(), "{expanded}");
}
