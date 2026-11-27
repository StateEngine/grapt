use super::*;

struct Profile(std::path::PathBuf);
impl Profile {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "grapt-session-pins-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        Ok(Self(root))
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn saved_pins_quick_slots_and_delayed_server_search_preserve_the_composer(
) -> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::new()?;
    let mut pins = Pins::load(&profile.0)?;
    let rows = agents()?;
    for row in rows.iter().take(10) {
        pins.toggle(row.row.id.clone())?;
    }
    let mut pins = Pins::load(&profile.0)?;
    assert_eq!(pins.slot(9), Some(&rows[8].row.id));
    assert!(pins.slot(10).is_none());
    pins.toggle(rows[0].row.id.clone())?;
    assert_eq!(Pins::load(&profile.0)?.slot(1), Some(&rows[1].row.id));
    let path = profile.0.join("session-pins.json");
    std::fs::rename(&path, profile.0.join("saved-pins.json"))?;
    std::fs::create_dir(&path)?;
    assert!(
        Pins::load(&profile.0).is_err(),
        "unreadable preferences cannot become absent preferences"
    );
    assert!(pins.toggle(rows[1].row.id.clone()).is_err());
    assert_eq!(
        pins.slot(1),
        Some(&rows[1].row.id),
        "failed saves retain the previous pins"
    );
    std::fs::remove_dir(&path)?;
    std::fs::rename(profile.0.join("saved-pins.json"), &path)?;
    let (backend, commands) = backend();
    let mut ui = Ui::new(false);
    ui.agents = rows.into_iter().take(3).collect();
    for (index, row) in ui.agents.iter_mut().enumerate() {
        row.row.detail.updated_ms = Some((index as u64 + 1) * 86_400_000);
        row.row.detail.directory = Some(if index == 1 { "/a" } else { "/z" }.into());
    }
    ui.composer.insert_str("unchanged draft");
    ui.picker = Some(Picker::new(&ui.agents));
    let ctrl = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
    handle_key(
        KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert_eq!(
        ui.picker
            .as_ref()
            .and_then(|p| p.selected(&ui.agents, &ui.session_pins)),
        Some(&ui.agents[2].row.id)
    );
    handle_key(ctrl('g'), &mut ui, &backend)?;
    handle_key(
        KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert_eq!(
        ui.picker
            .as_ref()
            .and_then(|p| p.selected(&ui.agents, &ui.session_pins)),
        Some(&ui.agents[1].row.id)
    );
    ui.session_pins = pins;
    assert_eq!(crate::draft(&ui), "unchanged draft");
    crate::keys::handle_at(ctrl('x'), &mut ui, &backend, std::time::Duration::ZERO)?;
    crate::keys::handle_at(
        KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
        &mut ui,
        &backend,
        std::time::Duration::from_millis(10),
    )?;
    assert!(
        matches!(commands.try_recv()?, Command::Select(request) if request.id == ui.agents[1].row.id)
    );
    ui.picker = Some(Picker::new(&ui.agents));
    crate::keys::paste("Remote", &mut ui);
    poll_search(&mut ui, &backend, std::time::Duration::from_millis(149));
    assert!(commands.try_recv().is_err());
    poll_search(&mut ui, &backend, std::time::Duration::from_millis(150));
    assert!(matches!(commands.try_recv()?, Command::SessionSearch(query) if query == "Remote"));
    let mut remote = ui.agents[0].clone();
    remote.row.id = session_id("ses_remote_search")?;
    remote.row.title = "Remote match".into();
    ui.session_message(Message::Search(Search {
        query: "Old".into(),
        result: Ok(vec![remote.clone()]),
    }));
    assert!(ui
        .picker
        .as_ref()
        .and_then(|p| p.selected(&ui.agents, &ui.session_pins))
        .is_none());
    ui.session_message(Message::Search(Search {
        query: "Remote".into(),
        result: Ok(vec![remote.clone()]),
    }));
    handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(
        matches!(commands.try_recv()?, Command::Select(request) if request.id == remote.row.id)
    );
    assert_eq!(crate::draft(&ui), "unchanged draft");
    std::fs::write(
        profile.0.join("tui.json"),
        r#"{"keybindings":{"bindings":{"models":"leader 1"}}}"#,
    )?;
    ui.bindings = crate::keybindings::Bindings::load(&profile.0)?;
    assert!(ui
        .bindings
        .shortcut(crate::actions::Action::SessionSlot(1))
        .is_none());
    crate::keys::handle_at(ctrl('x'), &mut ui, &backend, std::time::Duration::ZERO)?;
    crate::keys::handle_at(
        KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
        &mut ui,
        &backend,
        std::time::Duration::from_millis(10),
    )?;
    assert!(matches!(commands.try_recv()?, Command::Models));
    assert!(ui.models.is_some());
    assert_eq!(crate::draft(&ui), "unchanged draft");
    Ok(())
}

#[test]
fn kss_picker_management_requires_confirmation_and_keeps_the_draft_on_refusal(
) -> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::new()?;
    let (backend, commands) = backend();
    let mut ui = Ui::new(false);
    ui.session_pins = Pins::load(&profile.0)?;
    ui.agents = agents()?.into_iter().take(2).collect();
    let id = ui.agents[0].row.id.clone();
    ui.session_pins.toggle(id.clone())?;
    ui.picker = Some(Picker::new(&ui.agents));
    ui.composer.insert_str("human draft");
    let ctrl = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
    let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
    handle_key(ctrl('d'), &mut ui, &backend)?;
    assert!(commands.try_recv().is_err());
    handle_key(plain(KeyCode::Esc), &mut ui, &backend)?;
    handle_key(ctrl('r'), &mut ui, &backend)?;
    crate::keys::paste("Renamed", &mut ui);
    handle_key(plain(KeyCode::Enter), &mut ui, &backend)?;
    let Command::ManageSession(request) = commands.try_recv()? else {
        return Err("rename request missing".into());
    };
    assert!(
        matches!(&request, Request::Rename { id: target, title } if target == &id && title == "Renamed")
    );
    ui.session_message(Message::Managed {
        request,
        result: Err("refused".into()),
        refresh_error: None,
    });
    assert!(ui.session_pins.contains(&id) && ui.agents.iter().any(|row| row.row.id == id));
    handle_key(plain(KeyCode::Esc), &mut ui, &backend)?;
    handle_key(ctrl('d'), &mut ui, &backend)?;
    handle_key(plain(KeyCode::Enter), &mut ui, &backend)?;
    let Command::ManageSession(request) = commands.try_recv()? else {
        return Err("delete request missing".into());
    };
    assert!(matches!(&request, Request::Delete { id: target } if target == &id));
    handle_key(plain(KeyCode::Enter), &mut ui, &backend)?;
    assert!(
        commands.try_recv().is_err(),
        "pending confirmation cannot duplicate deletion"
    );
    ui.select_view(&backend, ui.agents[1].row.id.clone())?;
    let Command::Select(_) = commands.try_recv()? else {
        return Err("newer view missing".into());
    };
    assert!(!ui.accept_selection_update(Some(crate::backend::CommandId::TEST), &mut []));
    ui.session_message(Message::Managed {
        request,
        result: Ok(()),
        refresh_error: None,
    });
    assert!(!ui.session_pins.contains(&id));
    assert!(ui.agents.iter().all(|row| row.row.id != id));
    let missing = ui.agents[0].clone();
    ui.session_pins.toggle(missing.row.id.clone())?;
    ui.picker = Some(Picker::new(&ui.agents));
    crate::keys::paste(&missing.row.title, &mut ui);
    ui.session_message(Message::Search(Search {
        query: missing.row.title.clone(),
        result: Ok(vec![missing.clone()]),
    }));
    ui.session_message(Message::Unavailable {
        id: missing.row.id.clone(),
    });
    assert!(!ui.session_pins.contains(&missing.row.id));
    assert!(ui
        .picker
        .as_ref()
        .is_some_and(|picker| picker.selected(&ui.agents, &ui.session_pins).is_none()));
    assert_eq!(crate::draft(&ui), "human draft");
    Ok(())
}
