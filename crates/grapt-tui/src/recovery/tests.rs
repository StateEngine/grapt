use super::*;
use crate::{
    backend::{Backend, Command},
    keys,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use std::sync::mpsc;
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn local_capacity_public_status_projects_to_waiting_and_clears_when_work_resumes() -> Result {
    use grapt_client::{wire, Bounds, EventEnvelope, SessionView};
    let session = wire::session_id("ses_local_capacity")?;
    let status = |value| -> std::result::Result<EventEnvelope, Box<dyn std::error::Error>> {
        Ok(EventEnvelope::decode(
            &serde_json::json!({
                "id":"evt_local_capacity", "type":"session.status", "created":1,
                "data":{"sessionID":&session,"status":value}
            })
            .to_string(),
        )?)
    };
    let waiting = status(serde_json::json!({
        "type":"retry", "attempt":1, "next":1000,
        "message":"Waiting for capacity (waited 37s)",
        "action":{"reason":"local_capacity", "provider":"fixture", "title":"Waiting for capacity",
            "message":"Work resumes when capacity is free; Esc cancels.","label":"Wait"}
    }))?;
    for clearing in ["busy", "idle", "disconnect"] {
        let mut view = SessionView::new(&session);
        view.apply(&waiting, Bounds::PRODUCTION)?;
        assert!(view.feedback.is_some(), "capacity waiting was dropped");
        let mut ui = Ui::new(false);
        ui.status = "Connected".into();
        ui.composer.insert_str("unfinished draft");
        ui.feedback = view.feedback.clone();
        let visible = rows(&ui, 80).join(" ").to_lowercase();
        assert!(
            visible.contains("waiting") && visible.contains("capacity"),
            "{visible}"
        );
        assert!(visible.contains("37"), "elapsed wait missing: {visible}");
        assert!(
            !visible.contains("provider") && !visible.contains("failed"),
            "{visible}"
        );
        assert_eq!(keys::draft(&ui), "unfinished draft");
        if clearing == "disconnect" {
            view.invalidate_live_quiescence();
        } else {
            view.apply(
                &status(serde_json::json!({"type":clearing}))?,
                Bounds::PRODUCTION,
            )?;
        }
        assert_eq!(view.feedback, None, "capacity waiting survived {clearing}");
    }
    Ok(())
}

#[test]
fn refused_paste_remains_visible_during_execution_recovery() {
    let mut ui = Ui::new(false);
    ui.composer.insert_str("KEEP draft");
    for feedback in [
        Feedback::Interrupted,
        Feedback::Failed {
            message: "Provider refused the request".into(),
        },
    ] {
        ui.feedback = Some(feedback.clone());
        ui.status = "Connected".into();
        let recovery = rows(&ui, 80);
        assert!(!keys::paste(&"x".repeat(65_536), &mut ui));
        let updated = rows(&ui, 80);
        assert!(updated.iter().any(|row| row.contains("paste refused")));
        assert!(recovery.iter().all(|row| updated.contains(row)));
        assert_eq!(ui.feedback, Some(feedback));
        assert_eq!(keys::draft(&ui), "KEEP draft");
    }
}

#[test]
fn recovery_banner_is_visible_at_forty_columns_and_draft_stays_editable() -> Result {
    let (tx, commands) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands: tx,
        updates,
        wake,
    };
    let mut ui = Ui::new(false);
    ui.status = "Connected".into();
    ui.composer.insert_str("KEEP draft");
    for feedback in [
        Feedback::Retry {
            attempt: 1,
            message: "HTTP429 busy\u{1b}[2J\u{202e}".into(),
            action: None,
        },
        Feedback::Failed {
            message: "Inspect the Session before retrying.".into(),
        },
        Feedback::Interrupted,
    ] {
        ui.feedback = Some(feedback);
        for (width, height) in [(40, 16), (80, 24), (120, 40), (9, 7), (1, 1), (0, 0)] {
            let lines = rows(&ui, usize::from(width));
            assert!(lines.iter().all(|s| cells::cells(s) <= usize::from(width)));
            assert!(lines
                .iter()
                .all(|s| !s.contains('\u{1b}') && !s.contains('\u{202e}')));
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            terminal.draw(|frame| ui.draw(frame))?;
            if width == 40 {
                let screen = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(screen.contains("draft kept") || screen.contains("Provider retrying"));
                assert!(screen.contains("KEEP draft"));
            }
        }
        keys::handle_key(
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert!(commands.try_recv().is_err());
    }
    assert_eq!(keys::draft(&ui), "KEEP draft!!!");
    Ok(())
}

#[test]
fn original_submission_retry_is_explicit_and_does_not_replace_the_edit() -> Result {
    let (tx, commands) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands: tx,
        updates,
        wake,
    };
    let mut ui = Ui::new(false);
    ui.status = "Unconfirmed. Alt+R retries the original.".into();
    ui.composer.insert_str("EDITED follow-up");
    assert!(rows(&ui, 40).join(" ").contains("Alt+R"));
    keys::handle_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT),
        &mut ui,
        &backend,
    )?;
    assert!(matches!(commands.try_recv()?,Command::Retry(text) if text=="EDITED follow-up"));
    assert_eq!(keys::draft(&ui), "EDITED follow-up");
    keys::handle_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT),
        &mut ui,
        &backend,
    )?;
    assert!(commands.try_recv().is_err());
    Ok(())
}

fn failed_input(text: &str) -> std::result::Result<Ui, Box<dyn std::error::Error>> {
    let mut ui = Ui::new(false);
    ui.session_update(Some(grapt_client::wire::session_id("ses_failed")?), false);
    ui.feedback = Some(Feedback::Failed {
        message: "Provider refused the request".into(),
    });
    ui.failed_execution = Some(grapt_client::wire::execution_id("exe_failed")?);
    ui.transcript.update(
        true,
        0,
        vec![grapt_client::Slice {
            compaction: None,
            kind: grapt_client::SliceKind::User,
            tool_id: None,
            tool_phase: None,
            ordinal: 0,
            text: text.into(),
            read_detail_start: None,
            tool_summary: None,
            marks: Vec::new(),
            closed: true,
            footer: None,
            truncated_bytes: 0,
        }],
    );
    Ok(ui)
}

#[test]
fn first_failed_input_returns_exactly_and_enter_resends_once() -> Result {
    let text = "  preserve indent 中👩🏽‍💻\nsecond line\n";
    let mut ui = failed_input(text)?;
    let (tx, commands) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands: tx,
        updates,
        wake,
    };
    assert!(ui.restore_failed_input());
    assert_eq!(keys::draft(&ui), text);
    assert!(rows(&ui, 120).join(" ").contains("draft kept"));
    assert!(commands.try_recv().is_err());
    for _ in 0..2 {
        keys::handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
    }
    assert!(matches!(commands.try_recv()?, Command::Submit(sent) if sent == text));
    assert!(commands.try_recv().is_err());
    // Enter takes the prompt out of the composer at once; nothing is left to resend.
    assert!(keys::draft(&ui).is_empty());
    ui.pending = None;
    // The backend confirms submission and replaces the pending status together.
    ui.status = "Connected".into();
    assert!(!ui.restore_failed_input());
    assert!(keys::draft(&ui).is_empty());
    assert!(!rows(&ui, 120).join(" ").contains("draft kept"));
    ui.failed_execution = Some(grapt_client::wire::execution_id("exe_failed_again")?);
    assert!(ui.restore_failed_input());
    assert_eq!(keys::draft(&ui), text);
    Ok(())
}

#[test]
fn failed_input_preserves_concurrent_edits_and_pending_submission() -> Result {
    let mut ui = failed_input("original failed input")?;
    ui.pending = Some("new request".into());
    assert!(!ui.restore_failed_input());
    ui.pending = None;
    ui.composer.insert_str("edited follow-up");
    assert!(!ui.restore_failed_input());
    assert_eq!(keys::draft(&ui), "edited follow-up");
    ui.composer.edit(|area| {
        area.select_all();
        area.cut();
    });
    assert!(!ui.restore_failed_input());
    assert!(keys::draft(&ui).is_empty());
    Ok(())
}

#[test]
fn failed_input_never_restores_an_older_prompt_when_latest_is_partial() -> Result {
    let mut ui = failed_input("older complete input")?;
    ui.transcript.update(
        false,
        1,
        vec![grapt_client::Slice {
            compaction: None,
            kind: grapt_client::SliceKind::User,
            tool_id: None,
            tool_phase: None,
            ordinal: 1,
            text: "partial".into(),
            read_detail_start: None,
            tool_summary: None,
            marks: Vec::new(),
            closed: true,
            footer: None,
            truncated_bytes: 30,
        }],
    );
    assert!(!ui.restore_failed_input());
    assert!(keys::draft(&ui).is_empty());
    assert!(!rows(&ui, 120).join(" ").contains("draft kept"));
    Ok(())
}

#[test]
fn later_failure_waits_for_its_prompt_instead_of_restoring_prior_success() -> Result {
    let previous = "successful previous input";
    let failed = "  KEEP_FAILED_DRAFT\nSecond line Ω\n";
    let mut ui = failed_input(previous)?;
    ui.feedback = None;
    ui.failed_execution = None;
    ui.composer.insert_str(previous);
    assert!(ui.submission_accepted(previous));
    // The successful prompt is still the newest rendered user row when the
    // next submission is acknowledged and its terminal feedback arrives.
    ui.composer.insert_str(failed);
    assert!(ui.submission_accepted(failed));
    ui.feedback = Some(Feedback::Failed {
        message: "Missing credential".into(),
    });
    ui.failed_execution = Some(grapt_client::wire::execution_id("exe_second_failed")?);
    assert!(!ui.restore_failed_input());
    assert!(keys::draft(&ui).is_empty());
    ui.transcript.update(
        false,
        1,
        vec![grapt_client::Slice {
            compaction: None,
            kind: grapt_client::SliceKind::User,
            tool_id: None,
            tool_phase: None,
            ordinal: 1,
            text: failed.into(),
            read_detail_start: None,
            tool_summary: None,
            marks: Vec::new(),
            closed: true,
            footer: None,
            truncated_bytes: 0,
        }],
    );
    assert!(ui.restore_failed_input());
    assert_eq!(keys::draft(&ui), failed);
    assert!(!ui.restore_failed_input());
    Ok(())
}

#[test]
fn a_provider_slot_wait_never_claims_to_be_a_running_or_retrying_request() -> Result {
    let mut ui = Ui::new(false);
    ui.feedback = Some(Feedback::WaitingForProviderSlot {
        message: "Waiting for provider slot".into(),
    });
    ui.composer.insert_str("draft remains editable");
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| ui.draw(frame))?;
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("Waiting for provider slot"), "{text}");
    assert!(!text.contains("Provider retrying"), "{text}");
    assert!(text.contains("draft remains editable"), "{text}");
    Ok(())
}

#[test]
fn failed_wake_never_recovers_older_human_input_or_touches_the_composer() -> Result {
    for human_draft in ["", "unfinished human follow-up", " "] {
        let mut ui = failed_input("previous successful HUMAN input")?;
        let mut automated = grapt_client::Slice {
            compaction: None,
            kind: grapt_client::SliceKind::Automated,
            tool_id: None,
            tool_phase: None,
            ordinal: 1,
            text: "PRIVATE_WAKE_PACKET\nresume and mail instructions".into(),
            read_detail_start: None,
            tool_summary: None,
            marks: Vec::new(),
            closed: true,
            footer: None,
            truncated_bytes: 0,
        };
        for (truncated, kind) in [
            (0, grapt_client::SliceKind::Automated),
            (50, grapt_client::SliceKind::Automated),
            (0, grapt_client::SliceKind::SeatStartup),
        ] {
            automated.kind = kind;
            automated.truncated_bytes = truncated;
            ui.transcript.update(false, 1, vec![automated.clone()]);
            ui.feedback = Some(Feedback::Failed {
                message: "StateEngine unavailable".into(),
            });
            ui.composer = crate::editor::Editor::new(vec![human_draft.to_owned()]);
            assert!(!ui.restore_failed_input());
            assert_eq!(keys::draft(&ui), human_draft);
            let notice = rows(&ui, 120).join(" ");
            assert!(notice.contains("Wake failed") && notice.contains("StateEngine unavailable"));
            assert!(!notice.contains("draft kept") && !notice.contains("Enter sends"));
            assert_eq!(
                ui.transcript.prompts().collect::<Vec<_>>(),
                vec!["previous successful HUMAN input"]
            );
        }
    }
    Ok(())
}

#[test]
fn notices_ellipsize_at_word_boundaries_within_the_available_cells() {
    let mut ui = Ui::new(false);
    ui.status = "The connection failed because the peer did not respond in time 界界".into();
    for width in [0, 1, 7, 19, 40, 120] {
        let lines = rows(&ui, width);
        assert!(lines.iter().all(|line| cells::cells(line) <= width));
        for line in lines {
            if let Some(prefix) = line.strip_suffix('…') {
                assert!(
                    prefix.is_empty()
                        || [&ui.status, "Service notice", "Details above; keep editing."]
                            .iter()
                            .any(|source| source
                                .strip_prefix(prefix)
                                .is_some_and(|tail| tail.starts_with(char::is_whitespace)))
                );
            }
        }
    }
}
