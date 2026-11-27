use super::*;
use crate::backend::{Backend, Command};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

struct Root(PathBuf);
impl Root {
    fn new() -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "grapt-keybindings-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).map_err(|e| e.to_string())?;
        Ok(Self(root))
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn event(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ctrl(ch: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
}
fn backend() -> (Backend, crate::Commands) {
    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    (
        Backend {
            commands,
            updates,
            wake,
        },
        received,
    )
}

#[test]
fn profile_overrides_rebind_disable_and_discover_real_actions_without_changing_the_draft(
) -> Result<(), String> {
    let root = Root::new()?;
    fs::write(root.0.join("tui.json"), r#"{"keybindings":{"leader":"ctrl-b","bindings":{"palette":"ctrl-y","models":"leader m","help":false}}}"#).map_err(|e| e.to_string())?;
    let mut ui = Ui::new(false);
    ui.bindings = Bindings::load(&root.0)?;
    ui.composer.insert_str("keep draft");
    let (backend, commands) = backend();
    crate::keys::handle_at(ctrl('y'), &mut ui, &backend, Duration::ZERO)?;
    assert!(ui.command_palette.is_some());
    assert!(matches!(commands.try_recv(), Ok(Command::Commands)));
    crate::keys::handle_at(ctrl('y'), &mut ui, &backend, Duration::ZERO)?;
    assert!(ui.command_palette.is_none());
    crate::keys::handle_at(event(KeyCode::F(1)), &mut ui, &backend, Duration::ZERO)?;
    crate::keys::handle_at(event(KeyCode::F(3)), &mut ui, &backend, Duration::ZERO)?;
    assert!(ui.command_pane.is_none() && ui.models.is_none());
    assert!(commands.try_recv().is_err());
    crate::keys::handle_at(ctrl('b'), &mut ui, &backend, Duration::ZERO)?;
    assert!(ui.chords.hint().is_some());
    crate::keys::handle_at(
        event(KeyCode::Char('m')),
        &mut ui,
        &backend,
        Duration::from_millis(10),
    )?;
    assert!(ui.models.is_some());
    assert!(matches!(commands.try_recv(), Ok(Command::Models)));
    assert_eq!(crate::keys::draft(&ui), "keep draft");
    let help = crate::backend::commands::help_lines_for(&ui.bindings).join("\n");
    assert!(help.contains("Ctrl+Y") && help.contains("Ctrl+B M") && help.contains("Unbound"));
    assert_eq!(ui.bindings.label(Action::Connect), "Ctrl+A");
    fs::write(
        root.0.join("tui.json"),
        r#"{"keybindings":{"bindings":{"palette":[]}}}"#,
    )
    .map_err(|e| e.to_string())?;
    let mut ui = Ui::new(false);
    ui.bindings = Bindings::load(&root.0)?;
    ui.composer.insert_str("/palette");
    crate::keys::handle_at(event(KeyCode::Enter), &mut ui, &backend, Duration::ZERO)?;
    assert!(
        ui.command_palette.is_some(),
        "a disabled shortcut retains slash access"
    );
    assert!(matches!(commands.try_recv(), Ok(Command::Commands)));
    Ok(())
}

#[test]
fn expired_cancelled_and_context_changed_chords_do_not_execute_or_swallow_following_text(
) -> Result<(), String> {
    let mut ui = Ui::new(false);
    let (backend, commands) = backend();
    ui.composer.insert_str("draft");
    crate::keys::handle_at(ctrl('x'), &mut ui, &backend, Duration::ZERO)?;
    crate::keys::handle_at(
        event(KeyCode::Char('l')),
        &mut ui,
        &backend,
        Duration::from_secs(2),
    )?;
    assert_eq!(crate::keys::draft(&ui), "draftl");
    assert!(ui.picker.is_none());
    crate::keys::handle_at(ctrl('x'), &mut ui, &backend, Duration::from_secs(3))?;
    ui.command_pane = Some(crate::command_pane::Pane::new(vec![
        "asynchronous result".into()
    ]));
    crate::keys::handle_at(
        event(KeyCode::Char('l')),
        &mut ui,
        &backend,
        Duration::from_millis(3010),
    )?;
    assert!(ui.chords.pending.is_none());
    assert!(ui.picker.is_none());
    assert_eq!(crate::keys::draft(&ui), "draftl");
    crate::keys::handle_at(
        event(KeyCode::Esc),
        &mut ui,
        &backend,
        Duration::from_millis(3020),
    )?;
    crate::keys::handle_at(ctrl('x'), &mut ui, &backend, Duration::from_secs(4))?;
    crate::keys::handle_at(
        event(KeyCode::Esc),
        &mut ui,
        &backend,
        Duration::from_millis(4010),
    )?;
    assert!(ui.picker.is_none());
    assert!(ui.chords.hint().is_none());
    assert!(commands.try_recv().is_err());
    ui.bindings = config::parse(r#"{"keybindings":{"bindings":{"questions":"leader q"}}}"#)?;
    let session = grapt_client::wire::session_id("ses_binding_modal").map_err(|e| e.to_string())?;
    ui.forms.update(
        Some(session.clone()),
        None,
        vec![grapt_client::wire::FormRow {
            metadata: Default::default(),
            id: grapt_client::wire::form_id("frm_binding_modal").map_err(|e| e.to_string())?,
            owner: grapt_client::wire::FormOwner::Session(session),
            title: "Modal editing".into(),
            fields: Vec::new(),
        }],
    );
    crate::keys::handle_at(ctrl('x'), &mut ui, &backend, Duration::from_secs(5))?;
    assert!(
        ui.chords.hint().is_none(),
        "form editing cannot start a global chord"
    );
    crate::keys::handle_at(
        event(KeyCode::F(6)),
        &mut ui,
        &backend,
        Duration::from_millis(5010),
    )?;
    assert!(
        !ui.forms.focused,
        "the local close key survives a rebound global Questions action"
    );
    ui.bindings =
        config::parse(r#"{"keybindings":{"bindings":{"palette":"ctrl-u","models":"f9"}}}"#)?;
    crate::keys::handle_at(
        event(KeyCode::F(9)),
        &mut ui,
        &backend,
        Duration::from_secs(6),
    )?;
    assert!(matches!(commands.try_recv(), Ok(Command::Models)));
    crate::keys::handle_at(ctrl('u'), &mut ui, &backend, Duration::from_millis(6010))?;
    assert!(ui.command_palette.is_none(), "the model filter owns Ctrl+U");
    crate::keys::handle_at(
        event(KeyCode::Esc),
        &mut ui,
        &backend,
        Duration::from_millis(6020),
    )?;
    ui.bindings = config::parse(r#"{"keybindings":{"bindings":{"palette":"ctrl-s"}}}"#)?;
    crate::keys::handle_at(ctrl('r'), &mut ui, &backend, Duration::from_secs(7))?;
    assert!(ui.recall.searching());
    crate::keys::handle_at(ctrl('s'), &mut ui, &backend, Duration::from_millis(7010))?;
    assert!(
        ui.command_palette.is_none() && ui.recall.searching(),
        "history search owns Ctrl+S"
    );
    Ok(())
}

#[test]
fn malformed_reserved_and_ambiguous_settings_fail_instead_of_falling_back() -> Result<(), String> {
    let root = Root::new()?;
    assert_eq!(Bindings::load(&root.0)?.label(Action::Palette), "Ctrl+P");
    for text in [
        r#"{"keybindings":{"bindings":{"unknown":"f9"}}}"#,
        r#"{"keybindings":{"bindings":{"connect":false}}}"#,
        r#"{"keybindings":{"bindings":{"palette":"ctrl-a"}}}"#,
        r#"{"keybindings":{"bindings":{"palette":"f3"}}}"#,
        r#"{"keybindings":{"bindings":{"models":"ctrl-p m"}}}"#,
        r#"{"keybindings":{"bindings":{"palette":"p"}}}"#,
        r#"{"keybindings":{"bindings":{"palette":"ctrl-m"}}}"#,
        r#"{"keybindings":{"bindings":{"evil\u001b[2J":"f9"}}}"#,
    ] {
        fs::write(root.0.join("tui.json"), text).map_err(|e| e.to_string())?;
        let error = Bindings::load(&root.0)
            .err()
            .ok_or("accepted invalid settings")?;
        assert!(
            !error.contains('\u{001b}'),
            "configuration errors must be literal terminal text"
        );
    }
    fs::remove_file(root.0.join("tui.json")).map_err(|e| e.to_string())?;
    fs::create_dir(root.0.join("tui.json")).map_err(|e| e.to_string())?;
    assert!(
        Bindings::load(&root.0).is_err(),
        "unreadable settings are not absent settings"
    );
    Ok(())
}

#[test]
fn terminal_function_keys_reach_configured_actions_without_touching_the_draft() -> Result<(), String>
{
    let root = Root::new()?;
    fs::write(
        root.0.join("tui.json"),
        r#"{"keybindings":{"bindings":{"models":["f9","ctrl-shift-f12","f24"]}}}"#,
    )
    .map_err(|e| e.to_string())?;
    let mut ui = Ui::new(false);
    ui.bindings = Bindings::load(&root.0)?;
    ui.composer.insert_str("unchanged draft");
    let (backend, commands) = backend();
    let mut parser = crate::input_bytes::Parser::new();
    for bytes in [b"\x1b[20~".as_slice(), b"\x1b[24;6~", b"\x1b[57387u"] {
        let decoded = parser
            .feed(bytes, Duration::ZERO)
            .into_iter()
            .next()
            .ok_or("configured function key was dropped")??;
        let crossterm::event::Event::Key(key) = decoded else {
            return Err("function key decoded as another event".into());
        };
        crate::keys::handle_at(key, &mut ui, &backend, Duration::ZERO)?;
        assert!(ui.models.is_some());
        assert!(matches!(commands.try_recv(), Ok(Command::Models)));
        assert_eq!(crate::keys::draft(&ui), "unchanged draft");
        crate::keys::handle_at(event(KeyCode::Esc), &mut ui, &backend, Duration::ZERO)?;
    }
    let native = crate::record_input::function_key(120).ok_or("native F9 record was dropped")?;
    crate::keys::handle_at(event(native), &mut ui, &backend, Duration::ZERO)?;
    assert!(matches!(commands.try_recv(), Ok(Command::Models)));
    assert_eq!(crate::keys::draft(&ui), "unchanged draft");
    Ok(())
}

#[test]
fn fixed_interrupt_and_escape_cancellation_cannot_be_configured_as_actions() -> Result<(), String> {
    let (backend, commands) = backend();
    for modifiers in [
        KeyModifiers::CONTROL,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ] {
        let mut ui = Ui::new(false);
        ui.composer.insert_str("draft");
        let key = KeyEvent::new(KeyCode::Char('c'), modifiers);
        assert_eq!(
            crate::quit::key(key, &mut ui, &backend, Duration::ZERO),
            Some(false)
        );
        assert!(crate::keys::draft(&ui).is_empty());
        assert!(ui.models.is_none());
    }
    let mut ui = Ui::new(false);
    ui.composer.insert_str("preserved draft");
    crate::keys::handle_at(ctrl('x'), &mut ui, &backend, Duration::ZERO)?;
    crate::keys::handle_at(
        event(KeyCode::Esc),
        &mut ui,
        &backend,
        Duration::from_millis(10),
    )?;
    assert!(ui.chords.hint().is_none() && ui.picker.is_none());
    assert_eq!(crate::keys::draft(&ui), "preserved draft");
    assert!(commands.try_recv().is_err());
    for key in ["ctrl-c", "ctrl-shift-c"] {
        for settings in [
            serde_json::json!({"keybindings":{"leader":key}}),
            serde_json::json!({"keybindings":{"bindings":{"models":key}}}),
            serde_json::json!({"keybindings":{"bindings":{"models":format!("leader {key}")}}}),
        ] {
            assert!(
                config::parse(&settings.to_string()).is_err(),
                "accepted fixed interrupt: {settings}"
            );
        }
    }
    assert!(config::parse(r#"{"keybindings":{"bindings":{"models":"leader esc"}}}"#).is_err());
    // Alt keeps Ctrl+C out of the fixed interrupt handler and remains bindable.
    ui.bindings = config::parse(r#"{"keybindings":{"bindings":{"models":"ctrl-alt-shift-c"}}}"#)?;
    let key = KeyEvent::new(
        KeyCode::Char('C'),
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
    );
    assert_eq!(
        crate::quit::key(key, &mut ui, &backend, Duration::from_millis(20)),
        None
    );
    crate::keys::handle_at(key, &mut ui, &backend, Duration::from_millis(20))?;
    assert!(ui.models.is_some());
    assert!(matches!(commands.try_recv(), Ok(Command::Models)));
    assert_eq!(crate::keys::draft(&ui), "preserved draft");
    Ok(())
}

#[test]
fn question_mark_opens_help_only_for_an_empty_ordinary_draft() -> Result<(), String> {
    let (backend, _) = backend();
    let mut ui = Ui::new(false);
    crate::keys::handle_at(event(KeyCode::Char('?')), &mut ui, &backend, Duration::ZERO)?;
    assert!(ui.command_pane.is_some());
    crate::keys::handle_at(event(KeyCode::Esc), &mut ui, &backend, Duration::ZERO)?;
    ui.composer.insert_str("why");
    crate::keys::handle_at(event(KeyCode::Char('?')), &mut ui, &backend, Duration::ZERO)?;
    assert!(ui.command_pane.is_none());
    assert_eq!(crate::keys::draft(&ui), "why?");
    ui.composer.edit(|area| {
        area.clear();
    });
    crate::keys::paste("?", &mut ui);
    assert!(ui.command_pane.is_none());
    assert_eq!(crate::keys::draft(&ui), "?");
    Ok(())
}
