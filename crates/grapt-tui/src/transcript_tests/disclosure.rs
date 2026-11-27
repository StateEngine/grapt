use crate::{backend::Backend, input_bytes::Parser, keys, mouse, ui::Ui};
use crossterm::event::{Event, KeyModifiers};
use crossterm::event::{KeyCode, KeyEvent};
use grapt_client::{projection::ToolPhase, Slice, SliceKind};
use grapt_tui_transcript::transcript::*;
use ratatui::{backend::TestBackend, Terminal};
use std::{sync::mpsc, time::Duration};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
fn a_single_call_has_one_disclosure_and_keeps_its_intent_when_the_group_grows() -> Result {
    let mut ui = Ui::new(false);
    ui.transcript.update(true, 0, vec![tool(0)?]);
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| ui.draw(frame))?;
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    assert!(ui.transcript.tool_key(key(KeyCode::BackTab)));
    assert_eq!(ui.transcript.tool_focus(), Some(Focus::Tool(0)));
    assert!(ui.transcript.tool_key(key(KeyCode::Enter)));
    assert!(ui.transcript.cell_expanded(0));
    ui.transcript.update(false, 1, vec![tool(1)?]);
    assert!(!ui.transcript.group_closed(0));
    assert!(!ui.transcript.tool_hidden(1));
    assert!(ui.transcript.cell_expanded(0));
    assert_eq!(ui.transcript.cell_count(), 2);
    Ok(())
}

fn tool(ordinal: usize) -> Result<Slice> {
    Ok(Slice {
        compaction: None,
        kind: SliceKind::Tool,
        ordinal,
        tool_id: Some(grapt_client::wire::tool_call_id(&format!(
            "call_hidden_{ordinal}"
        ))?),
        tool_phase: Some(ToolPhase::Completed),
        tool_summary: Some(grapt_client::projection::ToolSummary {
            terminal_session: None,
            stdin: None,
            title: "Run bounded command".into(),
            duration_ms: Some(8000),
            preview: vec![],
            omitted_lines: 0,
            error: None,
            name: format!("shell_{ordinal}"),
            argument: "command with a long target path to AGENTS.md and bounded arguments".into(),
            result: "80 output lines".into(),
        }),
        text: format!("DETAIL_{ordinal}\n{}", "retained body\n".repeat(80)),
        read_detail_start: None,
        marks: Vec::new(),
        closed: true,
        footer: None,
        truncated_bytes: 0,
    })
}
fn user(ordinal: usize) -> Result<Slice> {
    let mut row = tool(ordinal)?;
    row.kind = SliceKind::User;
    row.tool_id = None;
    row.tool_phase = None;
    row.tool_summary = None;
    row.text = format!("PROMPT_{ordinal}");
    Ok(row)
}
fn screen(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            let mut line: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            line.push('\n');
            line
        })
        .collect()
}
fn marker(terminal: &Terminal<TestBackend>, expanded: bool) -> Result<(u16, u16)> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .find_map(|y| {
            (0..buffer.area.width).find_map(|x| {
                (buffer[(x, y)].symbol() == if expanded { "▾" } else { "▸" }).then_some((x, y))
            })
        })
        .ok_or_else(|| "tool marker is not on screen".into())
}
fn click(ui: &mut Ui, x: u16, y: u16) -> Result {
    let mut parser = Parser::new();
    for suffix in ['M', 'm'] {
        let report = format!("\x1b[<0;{};{}{suffix}", x + 1, y + 1);
        for byte in report.bytes() {
            for event in parser.feed(&[byte], Duration::ZERO) {
                let Event::Mouse(event) = event? else {
                    return Err("mouse bytes became editor input".into());
                };
                mouse::handle(event, ui);
            }
        }
    }
    assert!(parser.idle());
    Ok(())
}
fn backend() -> Backend {
    let (commands, _) = mpsc::channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    Backend {
        commands,
        updates,
        wake,
    }
}

#[test]
fn fragmented_clicks_cover_folded_summaries_and_preserve_composer_ownership() -> Result {
    for width in [40, 80] {
        let mut ui = Ui::new(false);
        ui.composer.insert_str("UNSENT_DRAFT");
        ui.transcript.update(true, 0, vec![tool(0)?]);
        let mut terminal = Terminal::new(TestBackend::new(width, 24))?;
        terminal.draw(|f| ui.draw(f))?;
        assert!(!screen(&terminal).contains("F8") && !screen(&terminal).contains("DETAIL_0"));
        let (_, y) = marker(&terminal, false)?;
        // Trailing blank columns of the summary are clickable.
        let right = ui.transcript_area.right().saturating_sub(1);
        click(&mut ui, right, y)?;
        terminal.draw(|f| ui.draw(f))?;
        assert!(ui.transcript.cell_expanded(0));
        let expanded = marker(&terminal, true)?;
        assert_eq!(ui.composer.lines(), ["UNSENT_DRAFT"]);
        assert!(!ui.transcript.tools_focused());
        click(&mut ui, expanded.0, expanded.1)?;
        terminal.draw(|f| ui.draw(f))?;
        assert!(!screen(&terminal).contains("DETAIL_0"));
        assert_eq!(ui.composer.lines(), ["UNSENT_DRAFT"]);
        keys::handle_key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &mut ui,
            &backend(),
        )?;
        assert_eq!(ui.composer.lines(), ["UNSENT_DRAFT "]);
    }
    Ok(())
}

#[test]
fn click_geometry_rejects_stale_and_non_tool_rows() -> Result {
    let mut ui = Ui::new(false);
    ui.composer.insert_str("KEEP_DRAFT");
    let mut rows = (0..8).map(user).collect::<Result<Vec<_>>>()?;
    rows.push(tool(8)?);
    ui.transcript.update(true, 0, rows);
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|f| ui.draw(f))?;
    let old = marker(&terminal, false)?;
    ui.transcript.from_top = true;
    ui.transcript.scroll = 0;
    assert!(
        !ui.transcript.click_tool(old.0, old.1),
        "ignore pixels before the new scroll paints"
    );
    terminal.draw(|f| ui.draw(f))?;
    assert!(
        !ui.transcript.click_tool(old.0, old.1),
        "ordinary user rows cannot disclose tools"
    );
    ui.transcript.scroll = 24;
    terminal.draw(|f| ui.draw(f))?;
    assert!(
        !ui.transcript.from_top && ui.transcript.scroll == 0,
        "the collapsed page clamps to its full bottom viewport"
    );
    let point = marker(&terminal, false)?;
    click(&mut ui, point.0, point.1)?;
    terminal.draw(|f| ui.draw(f))?;
    let point = marker(&terminal, true)?;
    assert!(ui.transcript.cell_expanded(8));
    ui.transcript.invalidate_hits();
    assert!(!ui.transcript.click_tool(point.0, point.1));
    assert_eq!(ui.composer.lines(), ["KEEP_DRAFT"]);
    Ok(())
}

#[test]
fn keyboard_focus_is_explicit_and_f8_never_discloses_a_tool() -> Result {
    let mut ui = Ui::new(false);
    ui.composer.insert_str("DO_NOT_SUBMIT");
    ui.transcript.update(true, 0, vec![tool(0)?, tool(1)?]);
    let mut terminal = Terminal::new(TestBackend::new(100, 28))?;
    terminal.draw(|f| ui.draw(f))?;
    let backend = backend();
    keys::handle_key(
        KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(ui.transcript.expanded_cells().all(|expanded| !expanded));
    keys::handle_key(
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        &mut ui,
        &backend,
    )?;
    assert_eq!(ui.transcript.tool_focus(), Some(Focus::Group(0)));
    keys::handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(ui.transcript.expanded_cells().all(|expanded| expanded));
    terminal.draw(|f| ui.draw(f))?;
    keys::handle_key(
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        &mut ui,
        &backend,
    )?;
    let Some(Focus::Tool(index)) = ui.transcript.tool_focus() else {
        return Err("BackTab must focus a visible individual tool".into());
    };
    keys::handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(!ui.transcript.cell_expanded(index));
    terminal.draw(|f| ui.draw(f))?;
    keys::handle_key(
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(ui.transcript.cell_expanded(index));
    keys::handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(!ui.transcript.tools_focused() && ui.picker.is_none());
    assert_eq!(ui.composer.lines(), ["DO_NOT_SUBMIT"]);
    Ok(())
}

#[test]
fn help_advertised_tool_and_overlay_keys_keep_the_draft_and_submit_nothing() -> Result {
    use crate::backend::commands::{execute, View};
    use grapt_client::{Bounds, Client, ContractDouble};

    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    let help = execute(&mut client, "/help").ok_or("help command")?;
    let Some(View::Text(rows)) = help.view else {
        return Err("help reference absent".into());
    };
    let text = rows.join("\n");
    for hint in [
        "Shift+Tab focuses visible tools",
        "Tab cycles focused tools",
        "Enter/Space toggles details",
        "Esc clears tool focus or closes a picker/result",
        "terminal's native copy action (not Ctrl+C)",
        "Ctrl+C interrupts a response or clears the draft; press twice within 1.5s to exit (not copy)",
    ] {
        assert!(text.contains(hint), "missing help hint: {hint}");
    }
    let (tx, commands) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _ready) = mpsc::sync_channel(1);
    let backend = Backend {
        commands: tx,
        updates,
        wake,
    };
    let mut ui = Ui::new(false);
    ui.launch.update(crate::launch::Readiness::Manual);
    ui.composer.insert_str("KEEP_DRAFT");
    ui.composer.move_cursor(ratatui_textarea::CursorMove::Back);
    let cursor = ui.composer.cursor();
    ui.transcript.update(true, 0, vec![tool(0)?, tool(1)?]);
    ui.command_pane = Some(crate::command_pane::Pane::new(rows));
    let mut terminal = Terminal::new(TestBackend::new(100, 28))?;
    terminal.draw(|f| ui.draw(f))?;
    assert!(!keys::handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?);
    assert!(ui.command_pane.is_none());
    terminal.draw(|f| ui.draw(f))?;
    for (code, modifiers) in [
        (KeyCode::BackTab, KeyModifiers::SHIFT),
        (KeyCode::Enter, KeyModifiers::NONE),
        (KeyCode::Tab, KeyModifiers::NONE),
        (KeyCode::Char(' '), KeyModifiers::NONE),
        (KeyCode::Esc, KeyModifiers::NONE),
    ] {
        assert!(!keys::handle_key(
            KeyEvent::new(code, modifiers),
            &mut ui,
            &backend,
        )?);
        terminal.draw(|f| ui.draw(f))?;
        assert_eq!(keys::draft(&ui), "KEEP_DRAFT");
        assert_eq!(ui.composer.cursor(), cursor);
        assert!(ui.pending.is_none() && ui.picker.is_none());
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
    // With tool focus cleared, Esc opens the session picker; the next closes it.
    for open in [true, false] {
        assert!(!keys::handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?);
        assert_eq!(ui.picker.is_some(), open);
        terminal.draw(|f| ui.draw(f))?;
        assert_eq!(keys::draft(&ui), "KEEP_DRAFT");
        assert_eq!(ui.composer.cursor(), cursor);
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
    assert_eq!(client.peer_mut().session_count(), 0);
    Ok(())
}

#[test]
fn one_click_discloses_raw_tools_and_groups_while_drag_keeps_details_open() -> Result {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    for count in [1, 3] {
        let mut ui = Ui::new(false);
        ui.transcript
            .update(true, 0, (0..count).map(tool).collect::<Result<Vec<_>>>()?);
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let (x, y) = marker(&terminal, false)?;
        click(&mut ui, x, y)?;
        assert!(
            ui.transcript.expanded_cells().all(|expanded| expanded),
            "one click must disclose raw command and output, not another collapsed layer"
        );
        terminal.draw(|frame| ui.draw(frame))?;
        let (x, y) = marker(&terminal, true)?;
        let event = |kind, column| MouseEvent {
            kind,
            column,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        mouse::handle(event(MouseEventKind::Down(MouseButton::Left), x), &mut ui);
        ui.selection
            .capture(terminal.backend().buffer(), ui.transcript_area);
        mouse::handle(
            event(MouseEventKind::Drag(MouseButton::Left), x + 4),
            &mut ui,
        );
        mouse::handle(event(MouseEventKind::Up(MouseButton::Left), x + 4), &mut ui);
        assert!(
            ui.transcript.expanded_cells().all(|expanded| expanded),
            "dragging a tool heading must select without collapsing"
        );
        click(&mut ui, x, y)?;
        assert!(ui.transcript.expanded_cells().all(|expanded| !expanded));
        if count > 1 {
            assert!(ui.transcript.group_closed(0));
        }
    }
    Ok(())
}
