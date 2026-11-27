//! Fixed native actions own their displayed bindings and key dispatch together.
use crate::{
    backend::{Backend, Command},
    keys, send,
    ui::Ui,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Palette,
    Connect,
    Sessions,
    Settings,
    SessionSlot(u8),
    Models,
    Agents,
    Usage,
    Theme,
    Questions,
    Help,
    Quit,
}
impl Action {
    pub fn id(self) -> &'static str {
        match self {
            Self::Palette => "palette",
            Self::Connect => "connect",
            Self::Sessions => "sessions",
            Self::Settings => "settings",
            Self::SessionSlot(slot) => [
                "session_slot",
                "session_slot_1",
                "session_slot_2",
                "session_slot_3",
                "session_slot_4",
                "session_slot_5",
                "session_slot_6",
                "session_slot_7",
                "session_slot_8",
                "session_slot_9",
            ]
            .get(usize::from(slot))
            .copied()
            .unwrap_or("session_slot"),
            Self::Models => "models",
            Self::Agents => "agents",
            Self::Usage => "usage",
            Self::Theme => "theme",
            Self::Questions => "questions",
            Self::Help => "help",
            Self::Quit => "quit",
        }
    }
}
pub struct Spec {
    pub action: Action,
    pub title: &'static str,
    pub slash: Option<&'static str>,
    pub key: Option<KeyCode>,
    pub modifiers: KeyModifiers,
}
macro_rules! spec {
    ($action:ident, $title:literal, $slash:expr, $key:expr, $mods:ident) => {
        Spec {
            action: Action::$action,
            title: $title,
            slash: $slash,
            key: $key,
            modifiers: KeyModifiers::$mods,
        }
    };
}
pub const SPECS: &[Spec] = &[
    spec!(
        Palette,
        "Command palette",
        Some("palette"),
        Some(KeyCode::Char('p')),
        CONTROL
    ),
    spec!(
        Connect,
        "Connect providers",
        Some("connect"),
        Some(KeyCode::Char('a')),
        CONTROL
    ),
    spec!(Sessions, "Switch session", Some("resume"), None, NONE),
    spec!(
        Settings,
        "Settings and preferences",
        Some("settings"),
        None,
        NONE
    ),
    spec!(
        Models,
        "Choose model and effort",
        Some("model"),
        Some(KeyCode::F(3)),
        NONE
    ),
    spec!(
        Agents,
        "Choose agent",
        Some("agents"),
        Some(KeyCode::F(7)),
        NONE
    ),
    spec!(
        Usage,
        "Session status and usage",
        Some("status"),
        Some(KeyCode::F(4)),
        NONE
    ),
    spec!(
        Theme,
        "Switch light/dark theme",
        None,
        Some(KeyCode::F(2)),
        NONE
    ),
    spec!(
        Questions,
        "Answer pending questions",
        None,
        Some(KeyCode::F(6)),
        NONE
    ),
    spec!(
        Help,
        "Help and shortcuts",
        Some("help"),
        Some(KeyCode::F(1)),
        NONE
    ),
    spec!(Quit, "Quit Grapt", Some("quit"), None, NONE),
    Spec {
        action: Action::SessionSlot(1),
        title: "Pinned session 1",
        slash: None,
        key: None,
        modifiers: KeyModifiers::NONE,
    },
    Spec {
        action: Action::SessionSlot(2),
        title: "Pinned session 2",
        slash: None,
        key: None,
        modifiers: KeyModifiers::NONE,
    },
    Spec {
        action: Action::SessionSlot(3),
        title: "Pinned session 3",
        slash: None,
        key: None,
        modifiers: KeyModifiers::NONE,
    },
    Spec {
        action: Action::SessionSlot(4),
        title: "Pinned session 4",
        slash: None,
        key: None,
        modifiers: KeyModifiers::NONE,
    },
    Spec {
        action: Action::SessionSlot(5),
        title: "Pinned session 5",
        slash: None,
        key: None,
        modifiers: KeyModifiers::NONE,
    },
    Spec {
        action: Action::SessionSlot(6),
        title: "Pinned session 6",
        slash: None,
        key: None,
        modifiers: KeyModifiers::NONE,
    },
    Spec {
        action: Action::SessionSlot(7),
        title: "Pinned session 7",
        slash: None,
        key: None,
        modifiers: KeyModifiers::NONE,
    },
    Spec {
        action: Action::SessionSlot(8),
        title: "Pinned session 8",
        slash: None,
        key: None,
        modifiers: KeyModifiers::NONE,
    },
    Spec {
        action: Action::SessionSlot(9),
        title: "Pinned session 9",
        slash: None,
        key: None,
        modifiers: KeyModifiers::NONE,
    },
];
pub const ALIASES: &[(&str, &str)] = &[
    ("sessions", "resume"),
    ("continue", "resume"),
    ("models", "model"),
    ("mo", "model"),
    ("variants", "effort"),
    ("preset", "permissions"),
    ("trust", "permissions"),
    ("summarize", "compact"),
    ("exit", "quit"),
    ("q", "quit"),
];
pub fn key(key: KeyEvent) -> Option<Action> {
    SPECS
        .iter()
        .find(|s| s.key == Some(key.code) && s.modifiers == key.modifiers)
        .map(|s| s.action)
}
pub fn canonical(text: &str) -> Option<String> {
    let command = text.trim().strip_prefix('/')?;
    let (name, suffix) = command
        .find(char::is_whitespace)
        .map_or((command, ""), |i| (&command[..i], &command[i..]));
    let (_, target) = ALIASES.iter().find(|(alias, _)| *alias == name)?;
    Some(format!("/{target}{suffix}"))
}
pub fn slash(text: &str) -> Option<Action> {
    let normalized = canonical(text);
    let name = normalized
        .as_deref()
        .unwrap_or(text)
        .trim()
        .strip_prefix('/')?;
    if name == "effort" {
        return Some(Action::Models);
    }
    SPECS
        .iter()
        .find(|s| s.slash == Some(name) && s.action != Action::Usage)
        .map(|s| s.action)
}
pub fn reason(action: Action, ui: &Ui) -> Option<&'static str> {
    match action {
        Action::SessionSlot(slot) if ui.session_pins.slot(slot).is_none() => {
            Some("Pin a session to fill this slot")
        }
        Action::Models | Action::Agents if ui.pending.is_some() => {
            Some("Wait for the submitted prompt")
        }
        Action::Models | Action::Agents if !ui.launch.readiness.attached() => {
            Some("Wait for session setup")
        }
        Action::Questions if ui.forms.count() == 0 => Some("No outstanding questions"),
        _ => None,
    }
}
pub fn names(spec: &Spec) -> String {
    let mut names = spec
        .slash
        .into_iter()
        .map(|s| format!("/{s}"))
        .collect::<Vec<_>>();
    if spec.action == Action::Models {
        names.push("/effort".into());
    }
    names.extend(
        ALIASES
            .iter()
            .filter(|(_, target)| {
                spec.slash == Some(*target)
                    || (spec.action == Action::Models && *target == "effort")
            })
            .map(|(alias, _)| format!("/{alias}")),
    );
    names.join(" ")
}
pub fn group(action: Action) -> &'static str {
    match action {
        Action::Sessions | Action::SessionSlot(_) | Action::Usage | Action::Questions => "Session",
        Action::Models | Action::Agents => "Agent",
        Action::Connect => "Integrations",
        _ => "System",
    }
}
pub fn help(bindings: &crate::keybindings::Bindings) -> Vec<String> {
    let mut rows = vec!["ACTIONS AND SHORTCUTS".into()];
    rows.extend(SPECS.iter().map(|s| {
        let command = s.slash.map(|name| format!("/{name}")).unwrap_or_default();
        format!(
            "{}{}{} — {}",
            bindings.label(s.action),
            if !command.is_empty() { " · " } else { "" },
            command,
            s.title
        )
    }));
    rows.push("Slash aliases:".into());
    rows.extend(
        ALIASES
            .iter()
            .map(|(alias, target)| format!("/{alias} → /{target}")),
    );
    rows.push(format!(
        "{} shows actions available in the current session; type to search.",
        bindings.label(Action::Palette)
    ));
    rows
}
/// A shortcut reports admission failures without handing terminal ownership away.
pub fn activate(action: Action, ui: &mut Ui, backend: &Backend) -> bool {
    match run(action, ui, backend) {
        Ok(exit) => exit,
        Err(error) => {
            ui.status = error;
            false
        }
    }
}

pub fn run(action: Action, ui: &mut Ui, backend: &Backend) -> Result<bool, String> {
    if let Some(reason) = reason(action, ui) {
        ui.status = reason.into();
        return Ok(false);
    }
    if !matches!(
        action,
        Action::Palette | Action::Questions | Action::Theme | Action::Connect
    ) {
        dismiss_views(ui);
    }
    match action {
        Action::Palette => {
            ui.clipboard.reset();
            ui.command_palette = Some(crate::command_palette::Picker::default());
            if let Err(error) = send(backend, Command::Commands) {
                ui.status = error;
            }
        }
        Action::Connect => {
            ui.chords.cancel();
            ui.command_palette = None;
            ui.clipboard.reset();
            ui.connect.open(ui.directory.as_deref());
        }
        Action::Sessions => ui.picker = Some(crate::sessions::Picker::new(&ui.agents)),
        Action::Settings => ui.settings = Some(crate::settings::Picker::default()),
        Action::SessionSlot(slot) => {
            if let Some(id) = ui.session_pins.slot(slot).cloned() {
                ui.select_view(backend, id)?;
            }
        }
        Action::Models => {
            send(backend, Command::Models)?;
            ui.models = Some(crate::models::Picker::default());
        }
        Action::Agents => send(backend, Command::Roster)?,
        Action::Usage => ui.usage_pane = Some(crate::usage::Pane::default()),
        Action::Theme => {
            ui.palette = grapt_tui_transcript::palette::Palette::new(!ui.palette.light)
        }
        Action::Questions => {
            let focused = !ui.forms.focused;
            dismiss_views(ui);
            ui.forms.focused = focused;
        }
        Action::Help => {
            ui.command_pane = Some(crate::command_pane::Pane::new(
                crate::backend::commands::help_lines_for(&ui.bindings),
            ))
        }
        Action::Quit => return Ok(true),
    }
    Ok(false)
}
pub fn dismiss_views(ui: &mut Ui) {
    ui.settings = None;
    ui.picker = None;
    ui.agent_picker = None;
    ui.models = None;
    ui.usage_pane = None;
    ui.command_pane = None;
    ui.grants_pane = None;
    ui.forms.focused = false;
}
pub fn command_name(row: &grapt_client::wire::CommandTemplate) -> String {
    if !ALIASES.iter().any(|(alias, _)| *alias == row.name)
        && !SPECS
            .iter()
            .any(|spec| spec.slash == Some(row.name.as_str()))
    {
        return row.name.clone();
    }
    let prefix = match row.origin {
        grapt_client::wire::CommandOrigin::User => "user",
        grapt_client::wire::CommandOrigin::Project => "project",
        _ => return row.name.clone(),
    };
    format!("{prefix}:{}", row.name)
}
pub fn stage(command: &str, ui: &mut Ui) {
    if !keys::draft(ui).is_empty() || ui.images.len() > 0 {
        ui.status = "Keep or clear the current draft before inserting a command".into();
        return;
    }
    ui.composer.insert_str(command);
    ui.recall.edited();
    ui.status = "Command inserted. Edit arguments, then Enter to run.".into();
}
