//! Input routing keeps overlays separate from draft edits and service commands.
use crate::{
    backend::{Backend, Command},
    send, subagents,
    ui::Ui,
    MAX_DRAFT,
};
use crossterm::event::{self, KeyCode, KeyModifiers};

pub(super) fn edits_composer(key: event::KeyEvent) -> bool {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return false;
    }
    match key.code {
        KeyCode::Char(_)
        | KeyCode::Backspace
        | KeyCode::Delete
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Home
        | KeyCode::End => true,
        KeyCode::Enter => key.modifiers.contains(KeyModifiers::SHIFT),
        _ => false,
    }
}

pub(super) fn draft(ui: &Ui) -> String {
    ui.composer.lines().join("\n")
}

#[cfg(test)]
pub(super) fn handle_key(
    key: event::KeyEvent,
    ui: &mut Ui,
    backend: &Backend,
) -> Result<bool, String> {
    handle_at(key, ui, backend, std::time::Duration::ZERO)
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn handle_at(
    key: event::KeyEvent,
    ui: &mut Ui,
    backend: &Backend,
    now: std::time::Duration,
) -> Result<bool, String> {
    // Any deliberate key disarms automatic launch; cursor edits and navigation
    // keep ownership even when their text happens to remain unchanged.
    if !ui.connect.visible()
        && key.code == KeyCode::Char('o')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(
            ui.launch.readiness,
            crate::launch::Readiness::Paused | crate::launch::Readiness::Unresolved
        )
    {
        match send(backend, Command::StartupRead) {
            Ok(()) => {
                ui.launch.readiness = crate::launch::Readiness::Connecting;
                ui.status = "Restoring saved Session choices…".into();
            }
            Err(error) => ui.status = error,
        }
        return Ok(false);
    }
    ui.launch.cancel();
    // Scrolling a result is not acknowledgment of its paired safety notice.
    if ui.command_pane.is_none() {
        ui.command_notice = None;
    }
    if ui.permission.is_none() && ui.connect.visible() {
        if key.code == KeyCode::Char('y') && key.modifiers == KeyModifiers::CONTROL {
            if let Some(url) = ui.connect.url() {
                ui.clipboard.request(crate::clipboard::Action::Copy(url));
            }
        } else if key.code == KeyCode::Char('v') && key.modifiers == KeyModifiers::CONTROL {
            ui.clipboard.request(crate::clipboard::Action::Paste);
        } else {
            ui.clipboard.reset();
            ui.connect.key(key);
        }
        return Ok(false);
    }
    if crate::actions::key(key) == Some(crate::actions::Action::Connect) {
        return crate::actions::run(crate::actions::Action::Connect, ui, backend);
    }
    if let Some(ask) = ui.permission.clone() {
        match key.code {
            KeyCode::F(4) => {
                ui.permission_details = !ui.permission_details;
                ui.permission_scroll = 0;
                return Ok(false);
            }
            KeyCode::Esc if ui.permission_details => {
                ui.permission_details = false;
                ui.permission_scroll = 0;
                return Ok(false);
            }
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => {
                ui.permission_details = true;
            }
            _ => {}
        }
        match key.code {
            KeyCode::PageUp => {
                ui.permission_scroll = ui.permission_scroll.saturating_sub(ui.permission_page)
            }
            KeyCode::PageDown => {
                ui.permission_scroll = ui.permission_scroll.saturating_add(ui.permission_page)
            }
            KeyCode::Home => ui.permission_scroll = 0,
            KeyCode::End => ui.permission_scroll = usize::MAX,
            _ => {}
        }
        if !ui.permission_input.ready(now)
            || key.kind != event::KeyEventKind::Press
            || !key.modifiers.is_empty()
        {
            return Ok(false);
        }
        let choice = match key.code {
            KeyCode::Char('y' | 'o') => Some(grapt_client::wire::PermissionReplyChoice::Once),
            KeyCode::Char('n' | 'r') => Some(grapt_client::wire::PermissionReplyChoice::Reject),
            KeyCode::Char('s') => Some(grapt_client::wire::PermissionReplyChoice::Session),
            KeyCode::Char('a') if ask.allow_always => {
                Some(grapt_client::wire::PermissionReplyChoice::Always)
            }
            _ => None,
        };
        if let Some(choice) = choice {
            ui.permission_scroll = 0;
            ui.status = match send(backend, Command::Reply(ask, choice)) {
                Ok(()) => "Sending permission reply…".into(),
                Err(error) => error,
            };
        }
        return Ok(false);
    }
    let context = crate::keybindings::context(ui);
    if context == crate::keybindings::Context::Questions
        && key.code == KeyCode::F(6)
        && key.modifiers.is_empty()
    {
        return crate::actions::run(crate::actions::Action::Questions, ui, backend);
    }
    let action = match ui.chords.advance(key, &ui.bindings, context, now) {
        crate::keybindings::Match::Pass => None,
        crate::keybindings::Match::Consumed => return Ok(false),
        crate::keybindings::Match::Action(action) => Some(action),
    };
    if ui.command_palette.is_some() {
        ui.clipboard.reset();
        if let Some(target) = crate::command_palette::key(key, action, ui) {
            match target {
                crate::command_palette::Target::Action(action) => {
                    return Ok(crate::actions::activate(action, ui, backend))
                }
                crate::command_palette::Target::Command(command) => {
                    crate::actions::dismiss_views(ui);
                    crate::actions::stage(&command, ui);
                }
            }
        }
        return Ok(false);
    }
    if ui.settings.is_some() {
        crate::settings::key(key, ui);
        return Ok(false);
    }
    if !ui.forms.focused {
        if let Some(
            action @ (crate::actions::Action::Palette
            | crate::actions::Action::Help
            | crate::actions::Action::SessionSlot(_)),
        ) = action
        {
            return Ok(crate::actions::activate(action, ui, backend));
        }
    }
    if let Some(pane) = &mut ui.grants_pane {
        if key.code == KeyCode::Esc {
            ui.grants_pane = None;
        } else if let Some(request) = pane.key(key.code) {
            if let Err(error) = send(backend, Command::RevokeGrant(request)) {
                pane.retry();
                ui.status = error;
            }
        }
        return Ok(false);
    }
    if let Some(pane) = &mut ui.usage_pane {
        if pane.key(key.code) {
            ui.usage_pane = None;
        }
        return Ok(false);
    }
    if let Some(pane) = &mut ui.command_pane {
        if pane.key(key.code) {
            ui.command_pane = None;
        }
        return Ok(false);
    }
    if let Some(picker) = &mut ui.agent_picker {
        if !picker.pending {
            match key.code {
                KeyCode::Esc => ui.agent_picker = None,
                KeyCode::Enter => {
                    if let Some(agent) = picker.selected().cloned() {
                        match send(backend, Command::ChooseAgent(agent)) {
                            Ok(()) => picker.pending = true,
                            Err(error) => ui.status = error,
                        }
                    }
                }
                _ => picker.key(key.code),
            }
        }
        return Ok(false);
    }
    if let Some(picker) = &mut ui.models {
        if picker.pending {
            return Ok(false);
        }
        match key.code {
            KeyCode::Esc => ui.models = None,
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => picker.clear_filter(),
            KeyCode::Enter => {
                if let Some(model) = picker.selected().cloned() {
                    if !model.enabled {
                        ui.status = "This model is disabled".into();
                    } else {
                        match send(backend, Command::Model(model)) {
                            Ok(()) => {
                                picker.pending = true;
                                ui.status = "Selecting model…".into();
                            }
                            Err(error) => ui.status = error,
                        }
                    }
                }
            }
            _ if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                picker.key(key.code)
            }
            _ => {}
        }
        return Ok(false);
    }
    if ui.picker.is_some() {
        return crate::sessions::handle(key, ui, backend, now);
    }
    if action == Some(crate::actions::Action::Questions) {
        return crate::actions::run(crate::actions::Action::Questions, ui, backend);
    }
    if ui.forms.focused {
        if let Some(request) = ui.forms.key(key) {
            if let Err(error) = send(backend, Command::Form(request)) {
                ui.forms.send_failed(error);
            }
        }
        return Ok(false);
    }
    if let Some(action) = action {
        return Ok(crate::actions::activate(action, ui, backend));
    }
    if key.code == KeyCode::Char('v') && key.modifiers == KeyModifiers::CONTROL {
        if ui.mentions_enabled() {
            ui.clipboard.request(crate::clipboard::Action::Paste);
        }
        return Ok(false);
    }
    if key.code == KeyCode::F(8) {
        ui.transcript.toggle_details();
        return Ok(false);
    }
    if ui.mentions_enabled() && ui.images.key(key, ui.composer.cursor() == (0, 0)) {
        return Ok(false);
    }
    if subagents::key(key, ui, backend) {
        return Ok(false);
    }
    if !ui.recall.searching()
        && !ui.mentions.visible()
        && !ui.slash.visible()
        && ui.transcript.tool_key(key)
    {
        return Ok(false);
    }
    if ui.mentions_enabled() {
        ui.mentions.sync(&ui.composer, true);
        match ui.mentions.key(key, &mut ui.composer) {
            Ok(true) => {
                ui.recall.edited();
                return Ok(false);
            }
            Err(error) => {
                ui.status = error;
                return Ok(false);
            }
            Ok(false) => {}
        }
    }
    let enabled = ui.pending.is_none() && ui.launch.readiness.attached() && !ui.recall.searching();
    ui.slash.sync(&draft(ui), ui.composer.cursor(), enabled);
    if let Some(suffix) = ui.slash.key(key) {
        if !suffix.is_empty() {
            ui.composer.insert_str(suffix);
            ui.recall.edited();
        }
        return Ok(false);
    }
    if ui.recall.key(key, &ui.transcript, &mut ui.composer) {
        return Ok(false);
    }
    if key.code == KeyCode::Char('?')
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && draft(ui).is_empty()
        && ui.images.len() == 0
    {
        return crate::actions::run(crate::actions::Action::Help, ui, backend);
    }
    if crate::sending::key(key, ui) || crate::followups::key(key, ui) {
        return Ok(false);
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('r')
            if key.modifiers == KeyModifiers::ALT && ui.launch.readiness.attached() =>
        {
            if ui.pending.is_none() {
                let text = draft(ui);
                match send(backend, Command::Retry(text.clone())) {
                    Ok(()) => {
                        ui.pending = Some(text);
                        ui.status = "Checking original submission; edit kept".into();
                    }
                    Err(error) => ui.status = error,
                }
            }
        }
        KeyCode::Home if control => {
            let movement = grapt_client::client::HistoryMove::Beginning;
            ui.navigation.jump(movement);
            history_request(ui, backend, movement);
        }
        KeyCode::End if control => {
            let movement = grapt_client::client::HistoryMove::Latest;
            ui.navigation.jump(movement);
            history_request(ui, backend, movement);
        }
        KeyCode::End
            if key.modifiers.is_empty() && (ui.browsing || ui.transcript.away_from_tail()) =>
        {
            let movement = grapt_client::client::HistoryMove::Latest;
            ui.navigation.jump(movement);
            history_request(ui, backend, movement);
        }
        KeyCode::PageUp => ui.navigation.scroll(20),
        KeyCode::PageDown => ui.navigation.scroll(-20),
        KeyCode::Esc if ui.views.loading() => {}
        KeyCode::Esc if ui.running() => {
            ui.followups.pause();
            if let Err(error) = send(
                backend,
                Command::InterruptAndResume(ui.live_execution.clone()),
            ) {
                ui.status = error;
            }
        }
        KeyCode::Esc => ui.picker = Some(crate::sessions::Picker::new(&ui.agents)),
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
            let text = draft(ui);
            if let Some(action) = crate::actions::slash(&text) {
                if let Some(reason) = crate::actions::reason(action, ui) {
                    ui.status = reason.into();
                    return Ok(false);
                }
                let exit = match crate::actions::run(action, ui, backend) {
                    Ok(exit) => exit,
                    Err(error) => {
                        ui.status = error;
                        return Ok(false);
                    }
                };
                ui.composer.edit(|area| {
                    area.clear();
                });
                ui.recall.edited();
                return Ok(exit);
            }
            if let Some(path) = text
                .trim()
                .strip_prefix("/attach ")
                .map(str::trim)
                .filter(|p| !p.is_empty())
            {
                let path = path
                    .strip_prefix('"')
                    .and_then(|p| p.strip_suffix('"'))
                    .unwrap_or(path);
                ui.clipboard.request(crate::clipboard::Action::Attach {
                    path: path.into(),
                    draft: Some(text),
                });
                ui.status = "Reading image…".into();
                return Ok(false);
            }
            if text.trim() == "/attach" {
                ui.status = "Usage: /attach <image path>".into();
                return Ok(false);
            }
            if !ui.launch.readiness.can_submit() {
                ui.status = if ui.launch.readiness == crate::launch::Readiness::Paused {
                    "Startup paused. Ctrl+O retry reads; draft kept".into()
                } else if ui.launch.readiness == crate::launch::Readiness::Unresolved {
                    "Repository unresolved. Ctrl+O retry reads; draft kept".into()
                } else {
                    "Restoring saved choices; draft kept".into()
                };
            } else if ui.images.len() == 0 {
                crate::sending::enter(ui, backend, text);
            } else if ui.pending.is_some() || ui.sending.busy() {
                // Images stay with their draft until earlier submissions settle.
                ui.status = "Wait for the current submission to send images; draft kept".into();
            } else {
                let command = match ui.images.submission(text.clone()) {
                    Ok(prompt) => Command::SubmitImages(prompt),
                    Err(error) => {
                        ui.status = error.into();
                        return Ok(false);
                    }
                };
                match crate::send_tracked(backend, command) {
                    Ok(id) => {
                        ui.images.sent(id);
                        if crate::backend::commands::submits_prompt(&text) {
                            ui.navigation
                                .jumped(grapt_client::client::HistoryMove::Latest, id);
                        }
                        ui.status = "Submitting; draft kept until confirmed".into();
                        ui.pending = Some(text);
                    }
                    Err(error) => ui.status = error,
                }
            }
        }
        _ => {
            let added = match key.code {
                KeyCode::Char(ch) => ch.len_utf8(),
                KeyCode::Enter => 1,
                _ => 0,
            };
            if draft(ui).len().saturating_add(added) <= MAX_DRAFT {
                ui.composer.input(key);
                ui.recall.edited();
            }
        }
    }
    Ok(false)
}

pub(super) fn history_request(
    ui: &mut Ui,
    backend: &Backend,
    movement: grapt_client::client::HistoryMove,
) {
    match crate::send_tracked(backend, Command::Browse(movement)) {
        Ok(id) => ui.navigation.sent(id),
        Err(error) => {
            ui.navigation.failed();
            ui.status = error;
        }
    }
}

pub(super) fn paste(text: &str, ui: &mut Ui) -> bool {
    if ui.permission.is_none() && !ui.connect.visible() {
        if let Some(picker) = &mut ui.settings {
            picker.paste(text);
            return false;
        }
    }
    if let Some(picker) = &mut ui.command_palette {
        picker.paste(text);
        return false;
    }
    if ui.connect.visible() && ui.permission.is_none() {
        ui.connect.paste(text);
        return false;
    }
    ui.transcript.clear_tool_focus();
    ui.launch.cancel();
    ui.command_notice = None;
    if ui.permission.is_some()
        || ui.usage_pane.is_some()
        || ui.command_pane.is_some()
        || ui.grants_pane.is_some()
        || ui.agent_picker.is_some()
    {
        return false;
    }
    if let Some(picker) = &mut ui.models {
        if !picker.pending && !picker.insert(text) {
            ui.status = "Model search is too long".into();
        }
        return false;
    }
    if let Some(picker) = &mut ui.picker {
        if !picker.insert(text, &ui.agents, &ui.session_pins) {
            ui.status = "Session search is too long".into();
        }
        return false;
    }
    if ui.forms.focused {
        ui.forms.paste(text);
        return false;
    }
    if ui.recall.searching() {
        ui.recall.paste(text, &mut ui.composer);
        return false;
    }
    if let Some(path) = crate::images::pasted_path(text) {
        ui.clipboard.request(crate::clipboard::Action::PastedPath {
            path,
            text: text.into(),
        });
        ui.status = "Reading image…".into();
        return true;
    }
    insert_paste(text, ui)
}

pub(super) fn insert_paste(text: &str, ui: &mut Ui) -> bool {
    if draft(ui).len().saturating_add(text.len()) <= MAX_DRAFT {
        ui.composer.insert_str(text);
        ui.recall.edited();
        true
    } else {
        ui.status = "Draft exceeds 64 KiB; paste refused".into();
        false
    }
}
