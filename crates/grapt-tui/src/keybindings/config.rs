// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Config normalization and explicit unbinding.
use super::*;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    keybindings: Overrides,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Overrides {
    leader: Option<String>,
    timeout_ms: Option<u64>,
    bindings: BTreeMap<String, Value>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Value {
    One(String),
    Many(Vec<String>),
    Disabled(bool),
}
impl Value {
    fn alternatives(&self) -> Result<Vec<&str>, String> {
        match self {
            Self::Disabled(false) => Ok(Vec::new()),
            Self::Disabled(true) => Err("Use false to disable an action".into()),
            Self::One(text) if text.eq_ignore_ascii_case("none") => Ok(Vec::new()),
            Self::One(text) => Ok(vec![text]),
            Self::Many(rows) if rows.len() <= 8 => Ok(rows.iter().map(String::as_str).collect()),
            Self::Many(_) => {
                Err("At most eight alternative bindings are allowed per action".into())
            }
        }
    }
}
pub(super) fn load(root: &Path) -> Result<Bindings, String> {
    let path = root.join("tui.json");
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Bindings::default())
        }
        Err(error) => {
            return Err(format!(
                "Cannot read native TUI settings {}: {error}",
                path.display()
            ))
        }
    };
    let mut text = String::new();
    file.take(65_537)
        .read_to_string(&mut text)
        .map_err(|e| format!("Cannot read native TUI settings: {e}"))?;
    if text.len() > 65_536 {
        return Err("Native TUI settings exceed 64 KiB".into());
    }
    parse(&text).map_err(|e| format!("Invalid native TUI settings {}: {e}", path.display()))
}
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn parse(text: &str) -> Result<Bindings, String> {
    let settings: Settings = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let raw = settings.keybindings;
    if raw.bindings.len() > crate::actions::SPECS.len() {
        return Err("Too many action overrides".into());
    }
    let mut bindings = Bindings::default();
    let leader = match raw.leader.as_deref() {
        Some(text) if text.eq_ignore_ascii_case("none") => None,
        Some(text) => Some(Stroke::parse(text)?),
        None => Some(Stroke {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::CONTROL,
        }),
    };
    if leader.is_some_and(|key| !key.prefix_allowed() || key.reserved()) {
        return Err(
            "Leader must use Ctrl, Alt or a function key; Ctrl+A and Ctrl+C are reserved".into(),
        );
    }
    bindings.rows.retain_mut(|row| {
        if matches!(row.action, Action::Sessions | Action::SessionSlot(_)) {
            if let Some(leader) = leader {
                row.first = leader;
            } else {
                return false;
            }
        }
        true
    });
    if let Some(timeout) = raw.timeout_ms {
        if !(100..=10_000).contains(&timeout) {
            return Err("Chord timeout_ms must be between 100 and 10000".into());
        }
        bindings.timeout = Duration::from_millis(timeout);
    }
    let explicit: BTreeSet<_> = raw.bindings.keys().cloned().collect();
    for (name, value) in raw.bindings {
        let action = crate::actions::SPECS
            .iter()
            .find(|spec| spec.action.id() == name)
            .map(|spec| spec.action)
            .ok_or_else(|| format!("Unknown native action {name}"))?;
        if action == Action::Connect {
            return Err("Ctrl+A is fixed as Connect; remove the connect override".into());
        }
        bindings.rows.retain(|row| row.action != action);
        for text in value.alternatives()? {
            let text = text.replace("<leader>", "leader ");
            if text.len() > 128 {
                return Err("A binding may contain at most 128 bytes".into());
            }
            let parts: Vec<_> = text.split_whitespace().take(3).collect();
            let (first, then) = match parts.as_slice() {
                ["leader", then] => (
                    leader.ok_or("A leader binding requires an enabled leader")?,
                    Some(Stroke::parse(then)?),
                ),
                [first] => (Stroke::parse(first)?, None),
                [first, then] => (Stroke::parse(first)?, Some(Stroke::parse(then)?)),
                _ => {
                    return Err(
                        "Use one key or a two-key chord, such as ctrl-x p or leader p".into(),
                    )
                }
            };
            if !first.prefix_allowed() || first.reserved() || then.is_some_and(Stroke::reserved) {
                return Err("Action shortcuts start with Ctrl, Alt or a function key; Ctrl+A and Ctrl+C are reserved".into());
            }
            if then.is_some_and(Stroke::cancels_chord) {
                return Err("Esc cancels a chord; choose another completion key".into());
            }
            if !bindings
                .rows
                .iter()
                .any(|row| row.action == action && row.first == first && row.then == then)
            {
                bindings.rows.push(Binding {
                    action,
                    first,
                    then,
                });
            }
        }
    }
    // New slot defaults must not invalidate an existing custom shortcut.
    let shadowed: Vec<_> = bindings
        .rows
        .iter()
        .filter(|row| {
            matches!(row.action, Action::SessionSlot(_)) && !explicit.contains(row.action.id())
        })
        .filter(|row| {
            bindings
                .rows
                .iter()
                .any(|other| explicit.contains(other.action.id()) && row.conflicts(other))
        })
        .map(|row| row.action)
        .collect();
    bindings.rows.retain(|row| !shadowed.contains(&row.action));
    for (index, row) in bindings.rows.iter().enumerate() {
        for other in &bindings.rows[index + 1..] {
            if row.conflicts(other) {
                return Err(format!(
                    "Conflicting bindings for {} and {}: {}",
                    row.action.id(),
                    other.action.id(),
                    row.label()
                ));
            }
        }
    }
    Ok(bindings)
}
