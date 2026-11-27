//! Selection controls for elicitation forms.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Modified by Grapt: bounded multi-selection, explicit commit and canonical values.
use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
impl Draft {
    pub(super) fn choice_key(&mut self, key: KeyEvent) {
        let Some(field) = self.row.fields.get(self.field) else {
            return;
        };
        let options = self.choice_values(field);
        if options.is_empty() || !key.modifiers.is_empty() && key.modifiers != KeyModifiers::SHIFT {
            return;
        }
        let cursor = self.choices.entry(field.key.clone()).or_default();
        match key.code {
            KeyCode::Up => *cursor = (*cursor + options.len() - 1) % options.len(),
            KeyCode::Down => *cursor = (*cursor + 1) % options.len(),
            KeyCode::Home => *cursor = 0,
            KeyCode::End => *cursor = options.len() - 1,
            KeyCode::Delete | KeyCode::Backspace => {
                *cursor = 0;
                self.values.remove(&field.key);
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                let Some(value) = options.get(*cursor).map(String::as_str) else {
                    return;
                };
                if field.kind == "multiselect" {
                    let mut items = match self.values.get(&field.key) {
                        Some(FormValue::Texts(items)) => items.clone(),
                        _ => Vec::new(),
                    };
                    if items.iter().any(|item| item == value) {
                        items.retain(|item| item != value);
                    } else {
                        items.push(value.to_owned());
                    }
                    if items.len() > fields::MAX_OPTIONS
                        || items.iter().map(String::len).sum::<usize>() > MAX_ANSWER
                    {
                        self.note = "Selections exceed 64 KiB; draft kept".into();
                        return;
                    }
                    self.values
                        .insert(field.key.clone(), FormValue::Texts(items));
                } else {
                    self.values
                        .insert(field.key.clone(), FormValue::Text(value.to_owned()));
                }
            }
            _ => {
                self.note = "Choose an offered option with ↑/↓ and Space; Ctrl+S answers".into();
            }
        }
        let last = self.choice_values(field).len().saturating_sub(1);
        if let Some(cursor) = self.choices.get_mut(&field.key) {
            *cursor = (*cursor).min(last);
        }
        self.scroll = 0;
    }
}

impl Draft {
    pub(super) fn choice_values(&self, field: &grapt_client::wire::FormField) -> Vec<String> {
        let mut entries: Vec<_> = fields::options(field)
            .iter()
            .filter_map(|option| option["value"].as_str().map(str::to_owned))
            .collect();
        if fields::custom(field) {
            let selected = match self.values.get(&field.key) {
                Some(FormValue::Text(text)) => vec![text.clone()],
                Some(FormValue::Texts(items)) => items.clone(),
                _ => Vec::new(),
            };
            for text in selected.into_iter().take(fields::MAX_OPTIONS) {
                if !entries.contains(&text) {
                    entries.push(text);
                }
            }
        }
        entries
    }
    pub(super) fn commit_custom(&mut self) {
        let Some(field) = self.row.fields.get(self.field) else {
            return;
        };
        let text = self.editor.lines().join("\n");
        let value = if field.kind == "multiselect" {
            let mut items = match self.values.get(&field.key) {
                Some(FormValue::Texts(items)) => items.clone(),
                _ => Vec::new(),
            };
            if !items.contains(&text) {
                items.push(text.clone());
            }
            FormValue::Texts(items)
        } else {
            FormValue::Text(text.clone())
        };
        if matches!(&value, FormValue::Texts(items) if items.len() > fields::MAX_OPTIONS) {
            self.note = "Too many custom selections; draft kept".into();
            return;
        }
        if value.to_json().to_string().len() > MAX_ANSWER {
            self.note = "Custom selections exceed 64 KiB; draft kept".into();
            return;
        }
        self.inputs
            .insert(field.key.clone(), self.editor.lines().join("\n"));
        self.values.insert(field.key.clone(), value);
        self.custom_edit = false;
        let cursor = self
            .choice_values(field)
            .iter()
            .position(|entry| entry == &text)
            .unwrap_or(0);
        self.choices.insert(field.key.clone(), cursor);
        self.scroll = 0;
        self.note = "Custom value chosen; Ctrl+S answers, Ctrl+E edits custom text".into();
    }
}
