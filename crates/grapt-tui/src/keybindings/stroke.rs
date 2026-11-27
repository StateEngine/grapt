// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Modified for bounded native action bindings and crossterm normalization.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stroke {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}
impl Stroke {
    pub fn event(event: KeyEvent) -> Self {
        let mut stroke = Self {
            code: event.code,
            modifiers: event.modifiers,
        };
        match stroke.code {
            KeyCode::BackTab => {
                stroke.code = KeyCode::Tab;
                stroke.modifiers.insert(KeyModifiers::SHIFT);
            }
            KeyCode::Char(ch) if ch.is_ascii_uppercase() => {
                stroke.code = KeyCode::Char(ch.to_ascii_lowercase());
                stroke.modifiers.insert(KeyModifiers::SHIFT);
            }
            _ => {}
        }
        stroke
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() || raw.len() > 64 || !raw.is_ascii() {
            return Err("Use a key such as ctrl-p, shift-enter or f3".into());
        }
        let text = raw.to_ascii_lowercase().replace('+', "-");
        let mut key = text.as_str();
        let mut modifiers = KeyModifiers::NONE;
        loop {
            let next = [
                ("ctrl-", KeyModifiers::CONTROL),
                ("control-", KeyModifiers::CONTROL),
                ("alt-", KeyModifiers::ALT),
                ("option-", KeyModifiers::ALT),
                ("shift-", KeyModifiers::SHIFT),
            ]
            .into_iter()
            .find_map(|(prefix, modifier)| key.strip_prefix(prefix).map(|rest| (rest, modifier)));
            let Some((rest, modifier)) = next else { break };
            if modifiers.contains(modifier) {
                return Err(format!("Repeated modifier in {raw}"));
            }
            modifiers.insert(modifier);
            key = rest;
        }
        let code = match key {
            "escape" | "esc" => KeyCode::Esc,
            "return" | "enter" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "del" | "delete" => KeyCode::Delete,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pgup" | "pageup" | "page-up" => KeyCode::PageUp,
            "pgdn" | "pagedown" | "page-down" => KeyCode::PageDown,
            "space" | "spacebar" => KeyCode::Char(' '),
            "minus" => KeyCode::Char('-'),
            key if key.len() == 1 => {
                let ch = key.chars().next().ok_or("Missing key")?;
                if ch.is_ascii_control() || ch.is_ascii_whitespace() || ch == '-' {
                    return Err(format!("Invalid key {raw}"));
                }
                KeyCode::Char(ch)
            }
            key => {
                let number = key
                    .strip_prefix('f')
                    .and_then(|n| n.parse::<u8>().ok())
                    .filter(|n| (1..=24).contains(n))
                    .ok_or_else(|| format!("Unknown key {raw}"))?;
                KeyCode::F(number)
            }
        };
        if modifiers == KeyModifiers::CONTROL
            && matches!(code, KeyCode::Char('i' | 'm' | '[' | '?'))
        {
            return Err("Use a named Backspace, Tab, Enter or Esc in a chord; terminal control aliases are ambiguous".into());
        }
        Ok(Self { code, modifiers })
    }

    pub fn label(self) -> String {
        let mut text = String::new();
        for (modifier, name) in [
            (KeyModifiers::CONTROL, "Ctrl+"),
            (KeyModifiers::ALT, "Alt+"),
            (KeyModifiers::SHIFT, "Shift+"),
        ] {
            if self.modifiers.contains(modifier) {
                text.push_str(name);
            }
        }
        text.push_str(&match self.code {
            KeyCode::Char(' ') => "Space".into(),
            KeyCode::Char(ch) => ch.to_ascii_uppercase().to_string(),
            KeyCode::F(n) => format!("F{n}"),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::Backspace => "Backspace".into(),
            KeyCode::Delete => "Delete".into(),
            KeyCode::Up => "Up".into(),
            KeyCode::Down => "Down".into(),
            KeyCode::Left => "Left".into(),
            KeyCode::Right => "Right".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            other => format!("{other:?}"),
        });
        text
    }

    pub fn prefix_allowed(self) -> bool {
        self.modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            || matches!(self.code, KeyCode::F(_))
    }

    pub fn cancels_chord(self) -> bool {
        self.code == KeyCode::Esc && self.modifiers.is_empty()
    }

    pub fn reserved(self) -> bool {
        (self.code == KeyCode::Char('a') && self.modifiers == KeyModifiers::CONTROL)
            || (self.code == KeyCode::Char('c')
                && self.modifiers.contains(KeyModifiers::CONTROL)
                && !self.modifiers.contains(KeyModifiers::ALT))
    }
}
