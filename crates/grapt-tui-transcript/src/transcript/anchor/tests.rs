use super::*;
use crate::palette::Palette;
use grapt_client::{projection::ToolPhase, SliceKind};

fn row(ordinal: usize, kind: SliceKind, text: String) -> Slice {
    Slice {
        compaction: None,
        kind,
        tool_id: None,
        tool_phase: None,
        tool_summary: None,
        ordinal,
        text,
        read_detail_start: None,
        marks: Vec::new(),
        closed: true,
        footer: None,
        truncated_bytes: 0,
    }
}

fn tool(index: usize) -> Slice {
    let mut slice = row(
        index,
        SliceKind::Tool,
        format!("result {index}\ncontinued\n"),
    );
    slice.tool_id = Some(
        grapt_client::wire::tool_call_id(&format!("call_anchor_{index}"))
            .unwrap_or_else(|error| panic!("tool identity: {error}")),
    );
    slice.tool_phase = Some(ToolPhase::Completed);
    slice
}

fn visible(transcript: &mut Transcript) -> Vec<String> {
    transcript
        .visible(70, 12, Palette::new(false))
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn disclose(transcript: &mut Transcript) {
    for group in transcript.tool_groups.runs.values_mut() {
        group.expanded = true;
    }
    for cell in &mut transcript.cells {
        cell.expanded = true;
    }
}

#[test]
fn durable_startup_insertion_keeps_the_top_tool_instead_of_following_new_output() {
    let mut transcript = Transcript::new();
    transcript.update(true, 0, (0..8).map(tool).collect());
    disclose(&mut transcript);
    transcript.from_top = true;
    let before = visible(&mut transcript);
    let mut rows = vec![row(0, SliceKind::SeatStartup, "fixture startup".into())];
    rows.extend((0..17).map(tool));
    transcript.reconcile(rows);
    let after = visible(&mut transcript);
    assert!(after[0].contains("Called 12 tools"));
    assert_eq!(transcript.tool_groups.runs.len(), 2);
    assert_eq!(after[1..], before[1..]);
    assert!(transcript.from_top);
}

#[test]
fn detached_tail_keeps_its_visible_edge_across_rebuild_and_appends() {
    let mut transcript = Transcript::new();
    transcript.update(true, 0, (0..30).map(tool).collect());
    disclose(&mut transcript);
    transcript.scroll = 17;
    let mut expected = visible(&mut transcript);
    // The separator puts this growing group's heading in view. Its count
    // changes truthfully; every other visible row keeps its reading position.
    let heading = expected
        .iter()
        .position(|line| line.contains("Called 6 tools"))
        .unwrap_or_else(|| panic!("growing group heading missing"));
    expected[heading] = expected[heading].replace("Called 6 tools", "Called 12 tools");
    transcript.reconcile((0..40).map(tool).collect());
    assert_eq!(visible(&mut transcript), expected);
    assert!(!transcript.from_top);
    assert!(transcript.scroll > 17);
}

#[test]
fn terminal_tool_changes_resolve_by_identity_and_missing_entries_clamp_locally() {
    let mut transcript = Transcript::new();
    transcript.update(true, 0, (0..15).map(tool).collect());
    disclose(&mut transcript);
    transcript.from_top = true;
    // Keep the third tool's header at the reading edge independently of
    // the compact spacing between earlier tools.
    transcript.scroll = (0..2).map(|i| transcript.height(i, 70)).sum();
    visible(&mut transcript);
    let mut rows: Vec<_> = (0..15).map(tool).collect();
    rows[2].text = "changed terminal output\n".into();
    transcript.reconcile(rows);
    assert!(visible(&mut transcript)
        .iter()
        .any(|line| line.contains("changed terminal output")));
    transcript.reconcile((10..15).map(tool).collect());
    assert!(transcript.from_top);
    assert!(!visible(&mut transcript).is_empty());
}

#[test]
fn reconciliation_preserves_disclosed_calls_and_keeps_new_calls_collapsed() {
    let mut transcript = Transcript::new();
    transcript.update(true, 0, (0..8).map(tool).collect());
    transcript.cells[3].expanded = true;
    visible(&mut transcript);
    transcript.reconcile((0..12).map(tool).collect());
    let expanded: Vec<_> = transcript
        .cells
        .iter()
        .enumerate()
        .filter_map(|(index, cell)| cell.expanded.then_some(index))
        .collect();
    assert_eq!(expanded, [3]);
}

#[test]
fn expanded_automated_input_and_tail_following_keep_their_existing_modes() {
    let mut transcript = Transcript::new();
    let startup = row(0, SliceKind::SeatStartup, "startup\n".repeat(30));
    transcript.update(true, 0, vec![startup.clone()]);
    transcript.cells[0].expanded = true;
    transcript.from_top = true;
    transcript.scroll = 4;
    let before = visible(&mut transcript);
    transcript.reconcile(vec![startup.clone(), tool(1)]);
    assert_eq!(visible(&mut transcript), before);
    assert!(transcript.cells[0].expanded);
    transcript.from_top = false;
    transcript.scroll = 0;
    visible(&mut transcript);
    transcript.reconcile(vec![startup.clone(), tool(1), tool(2)]);
    assert!(!transcript.away_from_tail());
    transcript.from_top = true;
    transcript.scroll = 4;
    // Session/page replacement deliberately resets, unlike reconciliation.
    transcript.update(true, 0, vec![startup]);
    assert!(!transcript.away_from_tail());
}

#[test]
fn identical_footer_sources_keep_the_visible_assistant_identity_after_history_insertion(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_client::{wire, Bounds, SessionView};
    let mut view = SessionView::new(&wire::session_id("ses_000000000001")?);
    for index in 1..=12 {
        let message = wire::assistant_message(
            &format!("msg_{index:012}"),
            &[],
            Some(wire::DurableFinish::Stop),
            Some((index, 1)),
            None,
        )?;
        let page = wire::decode_messages(&wire::encode_messages(vec![message], None)?)?;
        view.apply_durable(&page.messages[0], Bounds::PRODUCTION)?;
    }
    let mut transcript = Transcript::new();
    transcript.update(true, 0, view.slices.clone());
    transcript.from_top = true;
    transcript.scroll = 8;
    visible(&mut transcript);
    let edge = transcript.window.first().ok_or("visible edge missing")?;
    let before = transcript.cells[edge.index]
        .slice
        .footer
        .as_ref()
        .ok_or("footer missing")?
        .message
        .clone();
    let mut inserted = view.slices[11].clone();
    inserted
        .footer
        .as_mut()
        .ok_or("older footer missing")?
        .message =
        wire::MessageWireId::parse("msg_000000000000".into()).ok_or("older message id")?;
    let mut rows = vec![inserted];
    rows.extend(view.slices);
    for (index, row) in rows.iter_mut().enumerate() {
        row.ordinal = index;
    }
    transcript.reconcile(rows);
    visible(&mut transcript);
    let edge = transcript.window.first().ok_or("rebuilt edge missing")?;
    let after = &transcript.cells[edge.index]
        .slice
        .footer
        .as_ref()
        .ok_or("rebuilt footer missing")?
        .message;
    assert_eq!(&before, after);
    Ok(())
}
