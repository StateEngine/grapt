use super::*;
use crate::{
    backend::{Backend, Command},
    keys,
    ui::Ui,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use std::{sync::mpsc, time::Duration};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn ask(
    preview: Option<serde_json::Value>,
) -> std::result::Result<PermissionAsk, Box<dyn std::error::Error>> {
    Ok(PermissionAsk {
        session_id: grapt_client::wire::session_id("ses_context")?,
        request_id: grapt_client::wire::permission_id("per_context")?,
        title: "shell".into(),
        subtitle: "review the prepared effect".into(),
        live: true,
        allow_always: true,
        call: None,
        preview: preview.map(serde_json::from_value).transpose()?,
    })
}

fn backend() -> (Backend, crate::Commands) {
    let (commands, receiver) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    (
        Backend {
            commands,
            updates,
            wake,
        },
        receiver,
    )
}

fn paint(
    terminal: &mut Terminal<TestBackend>,
    ui: &mut Ui,
) -> std::result::Result<String, Box<dyn std::error::Error>> {
    terminal.draw(|frame| ui.draw(frame))?;
    let buffer = terminal.backend().buffer();
    Ok(buffer
        .content
        .chunks(usize::from(buffer.area.width.max(1)))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n"))
}

#[test]
fn a_failed_reply_remains_visible_inside_the_pending_approval() -> Result {
    for (width, height) in [(40, 16), (80, 24), (160, 45)] {
        let mut ui = Ui::new(false);
        ui.status = "v2.session.permission.reply answer failed: HTTP 503: the service is unavailable; retrying can work".into();
        ui.composer.insert_str("unsent draft");
        ui.permission_update(Some(ask(None)?));
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        let text = paint(&mut terminal, &mut ui)?;
        assert!(
            text.contains("503"),
            "reply failure was hidden at {width}x{height}: {text}"
        );
        for choice in ["[y] Once", "[s] Session", "[n] Reject"] {
            assert!(text.contains(choice), "decision control lost: {text}");
        }
        assert!(
            ui.permission.is_some(),
            "a failed reply removed the pending request"
        );
        assert_eq!(keys::draft(&ui), "unsent draft");
    }
    Ok(())
}

#[test]
fn a_failed_reply_is_visible_after_reviewing_the_tail_of_the_same_request() -> Result {
    let preview = serde_json::json!({"type":"patch","format":"operations",
        "patch":format!("{}\nPATCH_CONTEXT_TAIL", "line\n".repeat(100)),"truncated":false});
    for (width, height) in [(40, 16), (80, 24), (160, 45)] {
        let mut ui = Ui::new(false);
        ui.status = "Connected".into();
        ui.composer.insert_str("unsent draft");
        let request = ask(Some(preview.clone()))?;
        ui.permission_update(Some(request.clone()));
        ui.permission_details = true;
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        paint(&mut terminal, &mut ui)?;
        let (backend, commands) = backend();
        for attempt in 1..=2 {
            ui.permission_scroll = usize::MAX;
            assert!(paint(&mut terminal, &mut ui)?.contains("PATCH_CONTEXT_TAIL"));
            ui.permission_input.presented(Duration::ZERO);
            keys::handle_at(
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
                &mut ui,
                &backend,
                Duration::from_millis(400),
            )?;
            assert!(matches!(
                commands.try_recv()?,
                Command::Reply(_, grapt_client::wire::PermissionReplyChoice::Once)
            ));
            paint(&mut terminal, &mut ui)?;
            ui.status = "v2.session.permission.reply answer failed: HTTP 503: the service is unavailable; retrying can work".into();
            ui.permission_update(Some(request.clone()));
            ui.permission_details = true;
            let text = paint(&mut terminal, &mut ui)?;
            assert!(
                text.contains("503"),
                "reply failure {attempt} was hidden at {width}x{height}: {text}"
            );
            for choice in ["[y] Once", "[s] Session", "[a] Always", "[n] Reject"] {
                assert!(text.contains(choice));
            }
        }
        assert_eq!(ui.permission, Some(request));
        assert_eq!(keys::draft(&ui), "unsent draft");
    }
    Ok(())
}

#[test]
fn every_command_row_and_its_tail_are_reachable_with_fixed_choices() -> Result {
    let command = format!(
        "  printf 'two  spaces'  \n{}\nSHELL_CONTEXT_TAIL",
        (0..80)
            .map(|i| format!("ROW_{i:03}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let request = ask(Some(serde_json::json!({"type":"shell","command":command,
        "cwd":"/work/context","shell":"/bin/sh","truncated":false,
        "classifierVerdict":"review required"})))?;
    for (width, height) in [(40, 16), (80, 24), (120, 40)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        let mut ui = Ui::new(false);
        ui.composer.insert_str("unsent 中 draft");
        ui.permission_update(Some(request.clone()));
        ui.permission_details = true;
        let (backend, commands) = backend();
        let mut text = paint(&mut terminal, &mut ui)?;
        assert!(!text.contains("SHELL_CONTEXT_TAIL"));
        let mut seen = String::new();
        for _ in 0..256 {
            for choice in ["[y] Once", "[s] Session", "[a] Always", "[n] Reject"] {
                assert!(text.contains(choice), "{width}x{height}: {text}");
            }
            seen.push_str(&text);
            let before = ui.permission_scroll;
            keys::handle_key(
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
                &mut ui,
                &backend,
            )?;
            text = paint(&mut terminal, &mut ui)?;
            if ui.permission_scroll == before {
                break;
            }
        }
        assert!(seen.contains("Classification: review required"));
        assert!(seen.contains("SHELL_CONTEXT_TAIL"));
        for i in 0..80 {
            assert!(
                seen.contains(&format!("ROW_{i:03}")),
                "skipped row {i} at {width}x{height}"
            );
        }
        assert!(commands.try_recv().is_err(), "paging must not reply");
        assert_eq!(keys::draft(&ui), "unsent 中 draft");
        keys::handle_key(
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert!(paint(&mut terminal, &mut ui)?.contains("Explanation:"));
        keys::handle_key(
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert!(paint(&mut terminal, &mut ui)?.contains("SHELL_CONTEXT_TAIL"));
        ui.permission_input.presented(Duration::ZERO);
        keys::handle_at(
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            &mut ui,
            &backend,
            Duration::from_millis(400),
        )?;
        let Command::Reply(reply, choice) = commands.try_recv()? else {
            panic!("approval reply absent")
        };
        assert_eq!(reply.request_id, request.request_id);
        assert_eq!(reply.session_id, request.session_id);
        assert_eq!(choice, grapt_client::wire::PermissionReplyChoice::Reject);
    }
    Ok(())
}

#[test]
fn typed_preview_updates_repaint_without_inventing_advice_and_new_requests_reset() -> Result {
    let mut request = ask(Some(
        serde_json::json!({"type":"shell","command":"echo safe",
        "cwd":"/work","shell":"sh","truncated":false}),
    ))?;
    let mut ui = Ui::new(false);
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    assert!(ui.permission_update(Some(request.clone())));
    ui.permission_details = true;
    assert!(!paint(&mut terminal, &mut ui)?.contains("Classification:"));
    assert!(!ui.permission_update(Some(request.clone())));
    request.preview = Some(serde_json::from_value(
        serde_json::json!({"type":"shell","command":"echo safe",
        "cwd":"/work","shell":"sh","truncated":false,"classifierVerdict":"needs review"}),
    )?);
    ui.permission_scroll = 2;
    assert!(ui.permission_update(Some(request.clone())));
    assert_eq!(ui.permission_scroll, 2);
    ui.permission_scroll = 0;
    assert!(paint(&mut terminal, &mut ui)?.contains("Classification: needs review"));
    request.request_id = grapt_client::wire::permission_id("per_next")?;
    ui.permission_scroll = 50;
    assert!(ui.permission_update(Some(request)));
    assert_eq!(ui.permission_scroll, 0);
    Ok(())
}

#[test]
fn patch_formats_truncation_absence_and_controls_remain_distinct() -> Result {
    for light in [false, true] {
        for format in ["apply-patch", "operations", "replacement", "write"] {
            let request = ask(Some(serde_json::json!({"type":"patch","format":format,
                "patch":"-old\n+new\n  two  spaces  \n\n\t\u{001b}[2J\u{202e}SAFE_TAIL","truncated":true})))?;
            let mut ui = Ui::new(light);
            ui.permission_update(Some(request));
            ui.permission_details = true;
            let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
            let text = paint(&mut terminal, &mut ui)?;
            for required in [
                format,
                "source preview truncated",
                "two  spaces",
                "\\t\\u{1b}[2J\\u{202e}SAFE_TAIL",
                "remaining patch unavailable",
            ] {
                assert!(text.contains(required), "missing {required}: {text}");
            }
            let color = if format == "operations" {
                ui.palette.text
            } else {
                ui.palette.good
            };
            let buffer = terminal.backend().buffer();
            let row = buffer.content.chunks(80).position(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .contains("+new")
            });
            let Some(row) = row else {
                panic!("patch addition absent")
            };
            let Some(column) = (0..80).find(|x| buffer[(*x, row as u16)].symbol() == "+") else {
                panic!("patch sign absent")
            };
            assert_eq!(buffer[(column, row as u16)].fg, color);
        }
    }
    for (preview, required) in [
        (None, "Preview unavailable for this request."),
        (
            Some(serde_json::json!({"type":"unavailable","reason":"legacy request"})),
            "Preview unavailable: legacy request",
        ),
    ] {
        let mut ui = Ui::new(false);
        ui.permission_update(Some(ask(preview)?));
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
        assert!(paint(&mut terminal, &mut ui)?.contains(required));
    }
    Ok(())
}

#[test]
fn resizing_and_tiny_viewports_do_not_lose_the_tail_or_escape_bounds() -> Result {
    let request = ask(Some(
        serde_json::json!({"type":"patch","format":"operations",
        "patch":format!("{}\nPATCH_CONTEXT_TAIL","line\n".repeat(100)),"truncated":false}),
    ))?;
    let mut ui = Ui::new(false);
    ui.permission_update(Some(request));
    ui.permission_details = true;
    for (width, height) in [(80, 24), (40, 16), (9, 7), (1, 1), (0, 0), (120, 40)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        ui.permission_scroll = usize::MAX;
        let text = paint(&mut terminal, &mut ui)?;
        if width >= 40 {
            assert!(text.contains("PATCH_CONTEXT_TAIL"));
        }
        assert!(ui.permission_page >= 1);
    }
    Ok(())
}

#[test]
fn expanding_permission_details_keeps_the_request_and_draft_until_an_explicit_reply() -> Result {
    let mut ui = Ui::new(false);
    let session = grapt_client::wire::session_id("ses_context")?;
    let form = grapt_client::wire::FormRow {
        metadata: [("kind".into(), serde_json::json!("request_user_input"))].into(),
        id: grapt_client::wire::form_id("frm_waiting_question")?,
        owner: grapt_client::wire::FormOwner::Session(session.clone()),
        title: "Pending question".into(),
        fields: vec![grapt_client::wire::FormField {
            attributes: Default::default(),
            key: "answer".into(),
            kind: "string".into(),
            title: None,
            required: true,
        }],
    };
    ui.forms.update(Some(session), None, vec![form.clone()]);
    ui.forms.paste("unfinished answer");
    assert!(ui.question_band_height(24).is_some());
    let request = ask(None)?;
    ui.composer.insert_str("unfinished draft");
    ui.permission_update(Some(request.clone()));
    let (backend, commands) = backend();
    assert!(!ui.permission_details);
    for expanded in [true, false, true] {
        keys::handle_key(
            KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert_eq!(ui.permission_details, expanded);
        assert!(ui.question_band_height(24).is_none());
        assert!(ui.forms.input_band());
        assert_eq!(ui.permission.as_ref(), Some(&request));
        assert_eq!(keys::draft(&ui), "unfinished draft");
        assert!(commands.try_recv().is_err());
    }
    let mut next = request;
    next.request_id = grapt_client::wire::permission_id("per_next")?;
    ui.permission_update(Some(next.clone()));
    assert!(!ui.permission_details);
    ui.permission_input.presented(Duration::ZERO);
    keys::handle_at(
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        &mut ui,
        &backend,
        Duration::from_millis(400),
    )?;
    let Command::Reply(reply, choice) = commands.try_recv()? else {
        panic!("reply absent")
    };
    assert_eq!(reply.request_id, next.request_id);
    assert_eq!(choice, grapt_client::wire::PermissionReplyChoice::Reject);
    ui.permission_update(None);
    assert!(ui.question_band_height(24).is_some());
    keys::handle_key(
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        &mut ui,
        &backend,
    )?;
    let Command::Form(answer) = commands.try_recv()? else {
        panic!("question answer absent")
    };
    assert_eq!(answer.form.id, form.id);
    assert_eq!(
        answer.decision,
        Some(grapt_tui_forms::Decision::Answer(
            [(
                "answer".into(),
                grapt_client::wire::FormValue::Text("unfinished answer".into()),
            )]
            .into()
        ))
    );
    Ok(())
}

#[test]
fn permission_footer_preserves_quit_confirmation_and_stop_feedback() -> Result {
    let mut ui = Ui::new(false);
    ui.permission_update(Some(ask(None)?));
    let (backend, commands) = backend();
    for (millis, confirmed) in [(0, false), (1, true)] {
        let now = std::time::Duration::from_millis(millis);
        assert_eq!(
            crate::quit::key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &mut ui,
                &backend,
                now,
            ),
            Some(confirmed)
        );
        let reply = if confirmed {
            ui.quit.request(&backend, now)?;
            let Command::Quit(reply) = commands.try_recv()? else {
                panic!("session stop request absent")
            };
            Some(reply)
        } else {
            None
        };
        let hint = ui.quit.hint().ok_or("quit feedback missing")?;
        assert!(crate::chrome::footer(&ui, 80)[0].starts_with(hint));
        assert!(ui.permission.is_some());
        if let Some(reply) = reply {
            assert!(ui.quit.poll(&backend, now).is_none());
            reply.send(Ok(()))?;
            assert!(matches!(ui.quit.poll(&backend, now), Some(Ok(()))));
        }
    }
    assert!(!ui.quit.waiting());
    Ok(())
}

#[test]
fn permission_keys_wait_for_the_visible_window_and_enter_never_approves() -> Result {
    use grapt_client::wire::PermissionReplyChoice::{Always, Once, Reject, Session};
    for (ch, choice) in [('y', Once), ('n', Reject), ('s', Session), ('a', Always)] {
        let (backend, commands) = backend();
        let mut ui = Ui::new(false);
        ui.permission_update(Some(ask(None)?));
        let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
        keys::handle_at(key, &mut ui, &backend, Duration::from_secs(1))?;
        ui.permission_input.presented(Duration::from_secs(1));
        keys::handle_at(key, &mut ui, &backend, Duration::from_millis(1399))?;
        assert!(commands.try_recv().is_err());
        ui.permission_input.presented(Duration::from_millis(1399));
        keys::handle_at(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut ui,
            &backend,
            Duration::from_millis(1400),
        )?;
        assert!(commands.try_recv().is_err());
        keys::handle_at(key, &mut ui, &backend, Duration::from_millis(1400))?;
        assert!(matches!(commands.try_recv()?, Command::Reply(_, actual) if actual == choice));
        let mut next = ask(None)?;
        next.request_id = grapt_client::wire::permission_id("per_next")?;
        ui.permission_update(Some(next));
        keys::handle_at(key, &mut ui, &backend, Duration::from_secs(2))?;
        assert!(commands.try_recv().is_err());
    }
    Ok(())
}
