use super::*;
use crate::{
    backend::{Backend, Command},
    handle_key,
    ui::Ui,
};
use grapt_client::{wire::session_id, Slice, SliceKind};
use std::sync::mpsc;

fn setup() -> (Ui, Backend, crate::Commands) {
    let mut ui = Ui::new(false);
    ui.transcript.update(
        true,
        0,
        ["older alpha", "newer beta"]
            .into_iter()
            .enumerate()
            .map(|(ordinal, text)| Slice {
                compaction: None,
                kind: SliceKind::User,
                tool_id: None,
                tool_phase: None,
                ordinal,
                text: text.into(),
                read_detail_start: None,
                tool_summary: None,
                marks: Vec::new(),
                closed: true,
                footer: None,
                truncated_bytes: 0,
            })
            .collect(),
    );
    let (tx, commands) = crate::command_channel();
    let rx = crate::backend::Handoff::default();
    let (wake, _ready) = mpsc::sync_channel(1);
    (
        ui,
        Backend {
            commands: tx,
            updates: rx,
            wake,
        },
        commands,
    )
}

#[test]
fn search_accepts_for_editing_and_only_the_next_enter_submits(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut ui, backend, commands) = setup();
    ui.composer.insert_str("original draft");
    handle_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        &mut ui,
        &backend,
    )?;
    assert!(ui.recall.searching());
    assert!(!crate::keys::paste("alpha", &mut ui));
    assert_eq!(crate::draft(&ui), "older alpha");
    crate::keys::paste(&"q".repeat(257), &mut ui);
    assert_eq!(crate::draft(&ui), "older alpha");
    assert!(ui
        .recall
        .status()
        .is_some_and(|status| status.contains("256 bytes")));
    handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(!ui.recall.searching());
    assert!(commands.try_recv().is_err());
    handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(matches!(commands.try_recv()?, Command::Submit(text) if text == "older alpha"));
    Ok(())
}

#[test]
fn cancel_and_session_changes_restore_the_editor_and_cursor(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut ui, backend, commands) = setup();
    ui.composer.insert_str("draft 中👩‍💻");
    ui.composer.move_cursor(CursorMove::Head);
    let cursor = ui.composer.cursor();
    handle_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        &mut ui,
        &backend,
    )?;
    handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert_eq!(crate::draft(&ui), "draft 中👩‍💻");
    assert_eq!(ui.composer.cursor(), cursor);
    assert!(ui.picker.is_none());
    handle_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        &mut ui,
        &backend,
    )?;
    ui.recall
        .session(Some(session_id("ses_changed")?), &mut ui.composer);
    assert_eq!(crate::draft(&ui), "draft 中👩‍💻");
    assert_eq!(ui.composer.cursor(), cursor);
    assert!(!ui.recall.searching());
    assert!(commands.try_recv().is_err());
    Ok(())
}

#[test]
fn only_complete_loaded_prompts_are_recalled_and_up_down_keep_normal_editing(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut ui, backend, commands) = setup();
    let mut partial = Slice {
        compaction: None,
        kind: SliceKind::User,
        tool_id: None,
        tool_phase: None,
        ordinal: 2,
        text: "partial secret".into(),
        read_detail_start: None,
        tool_summary: None,
        marks: Vec::new(),
        closed: true,
        footer: None,
        truncated_bytes: 4,
    };
    ui.transcript.update(false, 2, vec![partial.clone()]);
    handle_key(
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert_eq!(crate::draft(&ui), "newer beta");
    handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert_eq!(crate::draft(&ui), "");
    partial.closed = false;
    partial.truncated_bytes = 0;
    ui.transcript.update(false, 2, vec![partial]);
    ui.composer.insert_str("first\nsecond");
    ui.recall.edited();
    handle_key(
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert_eq!(crate::draft(&ui), "first\nsecond");
    assert!(commands.try_recv().is_err());
    Ok(())
}

#[test]
fn a_submission_acknowledged_during_search_cannot_restore_a_sent_draft(
) -> Result<(), Box<dyn std::error::Error>> {
    for query in ["beta", "no match for this"] {
        let (mut ui, backend, commands) = setup();
        ui.composer.insert_str("already submitted");
        handle_key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            &mut ui,
            &backend,
        )?;
        crate::keys::paste(query, &mut ui);
        ui.recall.submitted("already submitted", &mut ui.composer);
        if query == "beta" {
            assert_eq!(crate::draft(&ui), "newer beta");
        } else {
            assert_eq!(crate::draft(&ui), "");
        }
        handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert_eq!(crate::draft(&ui), "");
        assert!(commands.try_recv().is_err());
    }
    Ok(())
}

#[test]
fn permission_focus_prevents_hidden_query_and_message_edits(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut ui, backend, commands) = setup();
    ui.composer.insert_str("original draft");
    handle_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        &mut ui,
        &backend,
    )?;
    let query = ui.recall.status();
    let preview = crate::draft(&ui);
    let mut double = grapt_client::ContractDouble::new();
    let session = double.create_session("permission").ok_or("session bound")?;
    double
        .ask_permission(&session, "pending")
        .ok_or("permission bound")?;
    let mut client = grapt_client::Client::new(double, grapt_client::Bounds::PRODUCTION);
    client.refresh_permissions()?;
    ui.permission = Some(
        client
            .permissions()
            .first()
            .cloned()
            .ok_or("permission absent")?,
    );
    crate::keys::paste("must not enter a hidden editor", &mut ui);
    assert_eq!(query, ui.recall.status());
    assert_eq!(preview, crate::draft(&ui));
    assert!(commands.try_recv().is_err());
    Ok(())
}

#[test]
fn long_search_queries_keep_the_cancel_and_edit_hints_visible(
) -> Result<(), Box<dyn std::error::Error>> {
    use ratatui::{backend::TestBackend, Terminal};
    for (width, height) in [(80, 24), (120, 40)] {
        let (mut ui, backend, commands) = setup();
        ui.composer.insert_str("saved fallback draft");
        handle_key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            &mut ui,
            &backend,
        )?;
        crate::keys::paste(&"q".repeat(200), &mut ui);
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let shown: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for text in [
            "Loaded history no match",
            "Enter edit",
            "Esc cancel",
            "saved fallback draft",
        ] {
            assert!(shown.contains(text), "{width}: {text}");
        }
        assert!(commands.try_recv().is_err());
    }
    Ok(())
}

#[test]
fn modified_arrows_keep_family_switching_with_loaded_prompt_history(
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::backend::Agent;
    use grapt_client::SessionSummary;
    let (mut ui, backend, commands) = setup();
    let parent = session_id("ses_recall_parent")?;
    let child = session_id("ses_recall_child")?;
    ui.agents = vec![
        Agent {
            display_name: None,
            context: Default::default(),
            row: SessionSummary {
                id: parent.clone(),
                title: "Parent".into(),
                parent: None,
                detail: Default::default(),
            },
            status: "idle".into(),
            current: true,
        },
        Agent {
            display_name: None,
            context: Default::default(),
            row: SessionSummary {
                id: child.clone(),
                title: "Child".into(),
                parent: Some(parent),
                detail: Default::default(),
            },
            status: "idle".into(),
            current: false,
        },
    ];
    for (code, modifiers) in [
        (KeyCode::Down, KeyModifiers::ALT),
        (KeyCode::Down, KeyModifiers::NONE),
        (KeyCode::Enter, KeyModifiers::NONE),
    ] {
        handle_key(KeyEvent::new(code, modifiers), &mut ui, &backend)?;
    }
    assert!(matches!(commands.try_recv()?, Command::Select(request) if request.id == child));
    assert_eq!(crate::draft(&ui), "");
    for modifiers in [
        KeyModifiers::CONTROL,
        KeyModifiers::SHIFT,
        KeyModifiers::ALT,
    ] {
        assert!(!ui.recall.key(
            KeyEvent::new(KeyCode::Up, modifiers),
            &ui.transcript,
            &mut ui.composer
        ));
        assert_eq!(crate::draft(&ui), "");
    }
    Ok(())
}

#[test]
fn session_changes_cancel_arrow_previews_but_keep_deliberately_edited_prompts(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut ui, backend, commands) = setup();
    ui.recall
        .session(Some(session_id("ses_recall_before")?), &mut ui.composer);
    handle_key(
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert_eq!(crate::draft(&ui), "newer beta");
    assert!(ui
        .recall
        .session(Some(session_id("ses_recall_after")?), &mut ui.composer));
    assert_eq!(crate::draft(&ui), "");
    assert_eq!(ui.composer.cursor(), ratatui_textarea::DataCursor(0, 0));
    handle_key(
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert_eq!(crate::draft(&ui), "newer beta");
    crate::keys::paste(" edited", &mut ui);
    ui.recall
        .session(Some(session_id("ses_recall_final")?), &mut ui.composer);
    assert_eq!(crate::draft(&ui), "newer beta edited");
    assert!(commands.try_recv().is_err());
    Ok(())
}

#[test]
fn refused_init_expands_the_original_draft_while_search_preview_stays_visible(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut ui, backend, commands) = setup();
    ui.composer.insert_str("/init");
    handle_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        &mut ui,
        &backend,
    )?;
    crate::keys::paste("alpha", &mut ui);
    assert_eq!(crate::draft(&ui), "older alpha");
    ui.command_update(crate::backend::commands::Completion {
        prepared_prompt: None,
        prepared_files: Vec::new(),
        text: "/init".into(),
        session: None,
        consume: false,
        replacement: Some("Expanded guidance task".into()),
        view: None,
        notice: "refused".into(),
    });
    assert_eq!(crate::draft(&ui), "older alpha");
    handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert_eq!(crate::draft(&ui), "Expanded guidance task");
    assert!(commands.try_recv().is_err());
    Ok(())
}
