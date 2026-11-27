//! Form cases that drive the TUI: its draw, backend, keys and command pane.
//! The board's own cases live with `grapt-tui-forms`.
use crate::{
    backend::{Backend, Command},
    keys,
    ui::Ui,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::wire::{
    decode_session_forms, form_id, project_id, session_id, FormField, FormLocation, FormOwner,
    FormRow, FormValue,
};
use grapt_tui_forms::{Board, Decision};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::mpsc;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

// Fixture builders, duplicated from the board's unit tests.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn control(ch: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
}
fn row(id: &str) -> Result<FormRow> {
    Ok(FormRow {
        metadata: Default::default(),
        id: form_id(id)?,
        owner: FormOwner::Session(session_id("ses_forms")?),
        title: "Choose branch".into(),
        fields: vec![
            FormField {
                attributes: Default::default(),
                key: "branch".into(),
                kind: "string".into(),
                title: None,
                required: true,
            },
            FormField {
                attributes: Default::default(),
                key: "dry_run".into(),
                kind: "boolean".into(),
                title: None,
                required: true,
            },
        ],
    })
}
fn board() -> Result<Board> {
    let mut board = Board::default();
    board.update(Some(session_id("ses_forms")?), None, vec![row("frm_one")?]);
    Ok(board)
}

fn setup(fields: serde_json::Value, global: bool) -> Result<Board> {
    let rows = decode_session_forms(&serde_json::to_vec(&json!({"data":[{
        "id":"frm_constraints","sessionID":if global {"global"} else {"ses_constraints"},
        "title":"Constraints","fields":fields}]}))?)?;
    let mut board = Board::default();
    board.update(
        Some(session_id("ses_constraints")?),
        Some(FormLocation {
            project: project_id("prj_constraints")?,
            directory: "/work/a".into(),
        }),
        rows,
    );
    Ok(board)
}

#[test]
fn field_drafts_paste_booleans_and_explicit_submit_preserve_the_prompt() -> Result {
    let mut ui = Ui::new(false);
    ui.composer.insert_str("UNSENT PROMPT");
    ui.forms = board()?;
    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands,
        updates,
        wake,
    };
    keys::paste("main\nsecond", &mut ui);
    keys::handle_key(key(KeyCode::Tab), &mut ui, &backend)?;
    keys::handle_key(key(KeyCode::Char(' ')), &mut ui, &backend)?;
    keys::handle_key(control('s'), &mut ui, &backend)?;
    let Command::Form(request) = received.try_recv()? else {
        return Err("missing form response".into());
    };
    assert_eq!(
        request.decision,
        Some(Decision::Answer(BTreeMap::from([
            ("branch".into(), FormValue::Text("main\nsecond".into())),
            ("dry_run".into(), FormValue::Flag(true))
        ])))
    );
    assert_eq!(keys::draft(&ui), "UNSENT PROMPT");
    keys::handle_key(key(KeyCode::Enter), &mut ui, &backend)?;
    keys::paste("cannot leak", &mut ui);
    assert_eq!(keys::draft(&ui), "UNSENT PROMPT");
    assert!(received.try_recv().is_err());
    Ok(())
}

#[test]
fn focused_overlay_survives_stream_frames_resize_and_literal_controls() -> Result {
    let mut ui = Ui::new(false);
    // The hostile title enters through the public update, before any draw.
    let mut hostile = row("frm_one")?;
    hostile.title = "Question\x1b[2J".into();
    ui.forms
        .update(Some(session_id("ses_forms")?), None, vec![hostile]);
    for (width, height) in [(80, 24), (30, 12)] {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Question"));
        assert!(!text.contains('\x1b'));
        assert!(text.contains("Ctrl+S") || width < 40);
    }
    Ok(())
}

#[test]
fn a_live_approval_keeps_key_and_paste_priority_over_the_form() -> Result {
    let mut ui = Ui::new(false);
    ui.forms = board()?;
    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands,
        updates,
        wake,
    };
    ui.permission = Some(grapt_client::PermissionAsk {
        session_id: session_id("ses_forms")?,
        request_id: grapt_client::wire::permission_id("per_forms")?,
        title: "Approval".into(),
        subtitle: String::new(),
        live: true,
        allow_always: true,
        call: None,
        preview: None,
    });
    keys::paste("must not enter the form", &mut ui);
    ui.permission_input.presented(std::time::Duration::ZERO);
    keys::handle_at(
        key(KeyCode::Char('y')),
        &mut ui,
        &backend,
        std::time::Duration::from_millis(400),
    )?;
    assert!(matches!(received.try_recv()?, Command::Reply(_, _)));
    assert!(ui
        .forms
        .draft_values(0)
        .is_some_and(|values| values.is_empty()));
    assert_eq!(keys::draft(&ui), "");
    Ok(())
}

#[test]
fn a_command_snapshot_holds_focus_and_restores_the_pending_question_after_close() -> Result {
    let mut ui = Ui::new(false);
    ui.composer.insert_str("UNSENT prompt");
    ui.forms = board()?;
    ui.forms.paste("saved answer");
    ui.command_pane = Some(crate::command_pane::Pane::new(vec![
        "COMMAND SNAPSHOT".into()
    ]));
    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands,
        updates,
        wake,
    };
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))?;
    terminal.draw(|frame| ui.draw(frame))?;
    let shown: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(shown.contains("COMMAND SNAPSHOT") && !shown.contains("Choose branch"));
    assert!(!keys::paste("hidden edit", &mut ui));
    keys::handle_key(key(KeyCode::Char('x')), &mut ui, &backend)?;
    assert_eq!(
        ui.forms
            .draft_values(0)
            .and_then(|values| values.get("branch")),
        Some(&FormValue::Text("saved answer".into()))
    );
    keys::handle_key(key(KeyCode::Esc), &mut ui, &backend)?;
    assert!(ui.command_pane.is_none() && ui.forms.focused);
    terminal.draw(|frame| ui.draw(frame))?;
    let shown: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(shown.contains("Choose branch"));
    assert_eq!(keys::draft(&ui), "UNSENT prompt");
    assert!(received.try_recv().is_err());
    Ok(())
}

#[test]
fn hidden_branches_and_custom_entry_keep_composer_and_editor_visible_at_short_sizes() -> Result {
    for (width, height) in [(160, 45), (80, 16)] {
        let mut ui = crate::ui::Ui::new(false);
        ui.composer.insert_str("CONSTRAINT COMPOSER");
        ui.forms = setup(
            json!([
                {"key":"enabled","type":"boolean","default":false},
                {"key":"hidden","type":"string","default":"HIDDEN VALUE","when":[{"key":"enabled","op":"eq","value":true}]},
                {"key":"custom","type":"string","custom":true,"options":[]}
            ]),
            true,
        )?;
        ui.forms.key(key(KeyCode::Tab));
        ui.forms.key(control('e'));
        ui.forms.paste("CUSTOM EDITOR");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("CUSTOM EDITOR"));
        assert!(screen.contains("Enter chooses"));
        assert!(!screen.contains("HIDDEN VALUE"));
        assert_eq!(ui.composer.lines(), ["CONSTRAINT COMPOSER"]);
    }
    Ok(())
}

#[test]
fn short_location_questions_keep_the_answer_editor_visible() -> Result {
    for height in [15, 16, 24] {
        let mut row = row("frm_global")?;
        row.owner = FormOwner::Global;
        row.metadata
            .insert("message".into(), serde_json::json!("Pick a branch"));
        let location = FormLocation {
            project: project_id("prj_location")?,
            directory: "/work/a".into(),
        };
        let mut ui = crate::ui::Ui::new(false);
        ui.forms.update(None, Some(location), vec![row]);
        ui.forms.paste("ink7");
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            text.matches("ink7").count() >= 2,
            "height {height}: answer summary is present but the editable answer is hidden"
        );
    }
    Ok(())
}
