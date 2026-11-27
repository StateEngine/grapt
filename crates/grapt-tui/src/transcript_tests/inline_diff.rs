use crate::{backend::Backend, keys, ui::Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::{wire::*, Bounds, SessionView};
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
use std::sync::mpsc;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn frame_rows(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        })
        .collect()
}

#[test]
fn crlf_file_lines_drop_the_break_not_a_literal_marker() -> Result {
    let tool = DurableTool {
        duration_ms: None,
        id: tool_call_id("call_inline_crlf")?,
        name: "patch".into(),
        state: DurableToolState::Completed {
            input: serde_json::json!({"operations":[
                {"op":"update","path":"crlf.rs",
                 "old_text":"one\r\nold\r\ntail\r","new_text":"one\r\nnew\r\ntail\r"}
            ]})
            .to_string(),
            output: serde_json::json!({"files":[
                {"relativePath":"crlf.rs","countsAvailable":true,"additions":1,"deletions":1,
                 "diff":{"firstLine":1,"oldText":"one\r\nold\r\ntail\r",
                         "newText":"one\r\nnew\r\ntail\r","omittedOldLines":0,"omittedNewLines":0}}
            ]})
            .to_string(),
        },
        progress: Vec::new(),
    };
    let mut view = SessionView::new(&session_id("ses_000000000002")?);
    view.apply_durable(
        &DurableMessage::Assistant {
            items: vec![MessageItem::Tool(tool)],
            finish: None,
            error: None,
            usage: None,
            cache: Box::default(),
            footer: None,
        },
        Bounds::default(),
    )?;
    let mut ui = Ui::new(false);
    ui.transcript.update(true, 0, view.slices.clone());
    let mut terminal = Terminal::new(TestBackend::new(120, 45))?;
    terminal.draw(|f| ui.draw(f))?;
    let rows = frame_rows(terminal.backend().buffer());
    let head = rows
        .iter()
        .position(|row| row.contains("crlf.rs"))
        .ok_or("diff block missing")?;
    let end = rows[head..]
        .iter()
        .position(|row| row.trim().is_empty())
        .ok_or("diff block unterminated")?;
    let block = &rows[head..head + end];
    assert!(
        block.iter().any(|row| row.contains("-old")),
        "deleted line missing"
    );
    assert!(
        block.iter().any(|row| row.contains("+new")),
        "inserted line missing"
    );
    let tail = block
        .iter()
        .find(|row| row.contains("tail"))
        .ok_or("last line missing")?;
    assert!(
        tail.contains("\\r"),
        "a lone final CR without a break is a content byte and must stay: {tail:?}"
    );
    assert!(
        block
            .iter()
            .filter(|row| !row.contains("tail"))
            .all(|row| !row.contains("\\r")),
        "a CRLF break leaked a literal CR marker: {block:?}"
    );
    Ok(())
}

#[test]
fn consecutive_diff_blocks_keep_one_blank_row_between_them() -> Result {
    let tool = DurableTool {
        duration_ms: None,
        id: tool_call_id("call_inline_pair")?,
        name: "patch".into(),
        state: DurableToolState::Completed {
            input: serde_json::json!({"operations":[
                {"op":"update","path":"one.rs","old_text":"old\n".repeat(12),"new_text":"new\n".repeat(12)},
                {"op":"create","path":"two.rs","content":"new\n".repeat(12)}
            ]}).to_string(),
            output: serde_json::json!({"files":[
                {"relativePath":"one.rs","countsAvailable":true,"additions":12,"deletions":12,
                 "diff":{"firstLine":1,"oldText":"old\n".repeat(12),"newText":"new\n".repeat(12),
                         "omittedOldLines":0,"omittedNewLines":0}},
                {"relativePath":"two.rs","countsAvailable":true,"additions":12,"deletions":0,
                 "diff":{"firstLine":1,"oldText":"","newText":"new\n".repeat(12),
                         "omittedOldLines":0,"omittedNewLines":0}}
            ]}).to_string(),
        },
        progress: Vec::new(),
    };
    let mut view = SessionView::new(&session_id("ses_000000000003")?);
    view.apply_durable(
        &DurableMessage::Assistant {
            items: vec![MessageItem::Tool(tool)],
            finish: None,
            error: None,
            usage: None,
            cache: Box::default(),
            footer: None,
        },
        Bounds::default(),
    )?;
    let mut ui = Ui::new(false);
    ui.transcript.update(true, 0, view.slices.clone());
    let mut terminal = Terminal::new(TestBackend::new(120, 45))?;
    terminal.draw(|f| ui.draw(f))?;
    let rows = frame_rows(terminal.backend().buffer());
    let first = rows
        .iter()
        .position(|row| row.contains("one.rs"))
        .ok_or("first block missing")?;
    let second = rows
        .iter()
        .position(|row| row.contains("two.rs"))
        .ok_or("second block missing")?;
    assert!(
        rows[second - 1].trim().is_empty(),
        "no blank row between the blocks: {:?}",
        &rows[first..=second]
    );
    assert!(
        rows[first..second - 1]
            .iter()
            .all(|row| !row.trim().is_empty()),
        "a blank row inside the first block: {:?}",
        &rows[first..=second]
    );
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn live_and_relisted_diff_folds_keep_file_state_and_the_unsent_draft() -> Result {
    for light in [false, true] {
        let tool = DurableTool {
            duration_ms: None, id: tool_call_id("call_inline_0001")?, name: "patch".into(),
            state: DurableToolState::Completed {
                input: serde_json::json!({"operations":[
                    {"op":"update","path":"one.rs","old_text":"old\n".repeat(18),"new_text":"new\n".repeat(18)},
                    {"op":"create","path":"two.rs","content":"new\n".repeat(24)}
                ]}).to_string(), output: serde_json::json!({"files":[
                    {"relativePath":"one.rs","countsAvailable":true,"additions":18,"deletions":18,
                     "diff":{"firstLine":1,"oldText":"old\n".repeat(18),"newText":"new\n".repeat(18),"omittedOldLines":0,"omittedNewLines":0}},
                    {"relativePath":"two.rs","countsAvailable":true,"additions":24,"deletions":0,
                     "diff":{"firstLine":1,"oldText":"","newText":"new\n".repeat(24),"omittedOldLines":0,"omittedNewLines":0}}
                ]}).to_string(),
            }, progress: Vec::new(),
        };
        let mut view = SessionView::new(&session_id("ses_000000000001")?);
        view.apply_durable(
            &DurableMessage::Assistant {
                items: vec![MessageItem::Tool(tool.clone())],
                finish: None,
                error: None,
                usage: None,
                cache: Box::default(),
                footer: None,
            },
            Bounds::default(),
        )?;
        let mut ui = Ui::new(light);
        ui.composer.insert_str("keep this draft");
        let rows = view.slices.clone();
        ui.transcript.update(true, 0, rows.clone());
        let mut terminal = Terminal::new(TestBackend::new(120, 45))?;
        terminal.draw(|f| ui.draw(f))?;
        let hit = ui
            .transcript
            .painted_rows()
            .enumerate()
            .find_map(|(y, row)| {
                (ui.transcript.diff_target(row.cell, row.row) == Some(0))
                    .then_some((ui.transcript_area.x, ui.transcript_area.y + y as u16))
            })
            .ok_or("diff click target missing")?;
        assert!(ui.transcript.click_tool(hit.0, hit.1));
        assert_eq!(
            ui.transcript.diff_expanded(0).ok_or("diff missing")?,
            ["patch one.rs"]
        );
        let mut tool = tool;
        if let DurableToolState::Completed { output, .. } = &mut tool.state {
            let mut value: serde_json::Value = serde_json::from_str(output)?;
            value["files"]
                .as_array_mut()
                .ok_or("file receipts absent")?
                .reverse();
            *output = value.to_string();
        }
        view.apply_durable(
            &DurableMessage::Assistant {
                items: vec![MessageItem::Tool(tool)],
                finish: None,
                error: None,
                usage: None,
                cache: Box::default(),
                footer: None,
            },
            Bounds::default(),
        )?;
        ui.transcript.update(false, 0, view.slices.clone());
        ui.transcript.reconcile(view.slices.clone());
        assert_eq!(
            ui.transcript
                .diff_expanded(0)
                .ok_or("relisted diff missing")?,
            ["patch one.rs"]
        );
        terminal.draw(|f| ui.draw(f))?;
        let (commands, _receiver) = crate::command_channel();
        let updates = crate::backend::Handoff::default();
        let (wake, _) = mpsc::sync_channel(1);
        let backend = Backend {
            commands,
            updates,
            wake,
        };
        // Ctrl+O toggles a visible file after receipt reordering.
        let before = ui.transcript.diff_expanded(0).ok_or("diff missing")?;
        keys::handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            &mut ui,
            &backend,
        )?;
        assert_ne!(
            ui.transcript.diff_expanded(0).ok_or("diff missing")?,
            before
        );
        assert_eq!(ui.composer.lines(), ["keep this draft"]);
        terminal.draw(|f| ui.draw(f))?;
        assert!(ui
            .transcript
            .cache_theme(0)
            .is_some_and(|theme| theme == light));
    }
    Ok(())
}
