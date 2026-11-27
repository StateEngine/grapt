// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Modified to use existing native
// action owners, modal precedence and caller-supplied elapsed time.
mod config;
mod stroke;
use crate::{actions::Action, ui::Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::{path::Path, time::Duration};
use stroke::Stroke;

#[derive(Clone)]
struct Binding {
    action: Action,
    first: Stroke,
    then: Option<Stroke>,
}
impl Binding {
    fn conflicts(&self, other: &Self) -> bool {
        self.first == other.first
            && (self.then.is_none() || other.then.is_none() || self.then == other.then)
    }
    fn label(&self) -> String {
        self.then.map_or_else(
            || self.first.label(),
            |then| format!("{} {}", self.first.label(), then.label()),
        )
    }
}
pub struct Bindings {
    rows: Vec<Binding>,
    timeout: Duration,
}
impl Default for Bindings {
    fn default() -> Self {
        let mut rows: Vec<_> = crate::actions::SPECS
            .iter()
            .filter_map(|spec| {
                Some(Binding {
                    action: spec.action,
                    first: Stroke {
                        code: spec.key?,
                        modifiers: spec.modifiers,
                    },
                    then: None,
                })
            })
            .collect();
        rows.push(Binding {
            action: Action::Sessions,
            first: Stroke {
                code: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL,
            },
            then: Some(Stroke {
                code: KeyCode::Char('l'),
                modifiers: KeyModifiers::NONE,
            }),
        });
        for slot in 1..=9 {
            rows.push(Binding {
                action: Action::SessionSlot(slot),
                first: Stroke {
                    code: KeyCode::Char('x'),
                    modifiers: KeyModifiers::CONTROL,
                },
                then: Some(Stroke {
                    code: KeyCode::Char(char::from(b'0' + slot)),
                    modifiers: KeyModifiers::NONE,
                }),
            });
        }
        Self {
            rows,
            timeout: Duration::from_secs(1),
        }
    }
}
impl Bindings {
    pub fn load(root: &Path) -> Result<Self, String> {
        config::load(root).map_err(|error| grapt_tui_transcript::markdown::literal(&error))
    }
    pub fn label(&self, action: Action) -> String {
        self.shortcut(action).unwrap_or_else(|| "Unbound".into())
    }
    pub fn shortcut(&self, action: Action) -> Option<String> {
        let labels: Vec<_> = self
            .rows
            .iter()
            .filter(|r| r.action == action)
            .map(Binding::label)
            .collect();
        if labels.is_empty() {
            None
        } else {
            Some(labels.join(" / "))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Main,
    Overlay,
    Models,
    Sessions,
    Recall,
    Grants,
    Palette,
    Questions,
    Exclusive,
}
impl Context {
    fn allows(self, action: Action) -> bool {
        match self {
            Self::Main => true,
            Self::Overlay | Self::Models | Self::Recall | Self::Grants | Self::Palette => {
                matches!(action, Action::Palette | Action::Help)
            }
            Self::Sessions => matches!(
                action,
                Action::Palette | Action::Help | Action::SessionSlot(_)
            ),
            Self::Questions => action == Action::Questions,
            Self::Exclusive => false,
        }
    }
    fn owns(self, key: Stroke) -> bool {
        match self {
            Self::Models => {
                key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::CONTROL
            }
            Self::Sessions => {
                key.modifiers == KeyModifiers::CONTROL
                    && matches!(key.code, KeyCode::Char('b' | 'd' | 'g' | 'r' | 'u'))
            }
            Self::Recall => {
                key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('r' | 's' | 'c'))
            }
            Self::Grants => {
                key.modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && matches!(key.code, KeyCode::Char(_))
                    && !(key.code == KeyCode::Char('p') && key.modifiers == KeyModifiers::CONTROL)
            }
            _ => false,
        }
    }
}
pub fn context(ui: &Ui) -> Context {
    if ui.permission.is_some() || ui.connect.visible() || ui.settings.is_some() {
        Context::Exclusive
    } else if ui.command_palette.is_some() {
        Context::Palette
    } else if ui.grants_pane.is_some() {
        Context::Grants
    } else if ui.models.is_some() {
        Context::Models
    } else if let Some(picker) = &ui.picker {
        if picker.editing() {
            Context::Exclusive
        } else {
            Context::Sessions
        }
    } else if ui.command_pane.is_some() || ui.usage_pane.is_some() || ui.agent_picker.is_some() {
        Context::Overlay
    } else if ui.forms.focused {
        Context::Questions
    } else if ui.recall.searching() {
        Context::Recall
    } else {
        Context::Main
    }
}

#[derive(Default)]
pub struct Matcher {
    pending: Option<(Stroke, Context, Duration)>,
    hint: Option<String>,
}
pub enum Match {
    Pass,
    Consumed,
    Action(Action),
}
impl Matcher {
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }
    pub fn cancel(&mut self) -> bool {
        let changed = self.pending.take().is_some();
        self.hint = None;
        changed
    }
    pub fn expire(&mut self, context: Context, now: Duration, bindings: &Bindings) -> bool {
        if self.pending.is_some_and(|(_, old, at)| {
            old != context || now.saturating_sub(at) >= bindings.timeout
        }) {
            return self.cancel();
        }
        false
    }
    pub fn advance(
        &mut self,
        event: KeyEvent,
        bindings: &Bindings,
        context: Context,
        now: Duration,
    ) -> Match {
        self.expire(context, now, bindings);
        let stroke = Stroke::event(event);
        if context.owns(stroke) {
            self.cancel();
            return Match::Pass;
        }
        // Form fields retain their editing, answer and decline controls. Only
        // a function key can start a configurable shortcut inside the board.
        if context == Context::Questions
            && ((self.pending.is_none() && !matches!(stroke.code, KeyCode::F(_)))
                || (stroke.modifiers == KeyModifiers::CONTROL
                    && matches!(stroke.code, KeyCode::Char('s' | 'r' | 'e' | 'o'))))
        {
            self.cancel();
            return Match::Pass;
        }
        if event.kind != KeyEventKind::Press {
            return if self.pending.is_some()
                || bindings
                    .rows
                    .iter()
                    .any(|row| context.allows(row.action) && row.first == stroke)
            {
                Match::Consumed
            } else {
                Match::Pass
            };
        }
        if self.pending.is_some() && stroke.cancels_chord() {
            self.cancel();
            return Match::Consumed;
        }
        if let Some((prefix, _, _)) = self.pending.take() {
            self.hint = None;
            if let Some(binding) = bindings.rows.iter().find(|row| {
                context.allows(row.action) && row.first == prefix && row.then == Some(stroke)
            }) {
                return Match::Action(binding.action);
            }
        }
        if let Some(binding) = bindings
            .rows
            .iter()
            .find(|row| context.allows(row.action) && row.first == stroke)
        {
            if binding.then.is_none() {
                return Match::Action(binding.action);
            }
            self.pending = Some((stroke, context, now));
            let completions: Vec<_> = bindings
                .rows
                .iter()
                .filter(|row| context.allows(row.action) && row.first == stroke)
                .filter_map(|row| {
                    row.then
                        .map(|then| format!("{} {}", then.label(), row.action.id()))
                })
                .take(6)
                .collect();
            self.hint = Some(format!(
                "{} then {} · Esc cancel",
                stroke.label(),
                completions.join(" · ")
            ));
            return Match::Consumed;
        }
        Match::Pass
    }
}

#[cfg(test)]
mod tests;
