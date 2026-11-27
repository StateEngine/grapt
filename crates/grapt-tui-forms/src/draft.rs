use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_textarea::{CursorMove, TextArea};

pub(super) struct Draft {
    pub target: Option<Target>,
    pub row: FormRow,
    pub field: usize,
    pub scroll: usize,
    pub values: BTreeMap<String, FormValue>,
    pub inputs: BTreeMap<String, String>,
    pub choices: BTreeMap<String, usize>,
    pub custom_edit: bool,
    pub editor: TextArea<'static>,
    pub listed: bool,
    pub pending: bool,
    pub decision: Option<Decision>,
    pub state: Option<FormState>,
    pub note: String,
}
impl Draft {
    pub fn new(target: Option<Target>, row: FormRow) -> Self {
        let mut editor = TextArea::default();
        editor.set_max_histories(32);
        let mut draft = Self {
            target,
            row,
            field: 0,
            scroll: 0,
            values: BTreeMap::new(),
            inputs: BTreeMap::new(),
            choices: BTreeMap::new(),
            custom_edit: false,
            editor,
            listed: true,
            pending: false,
            decision: None,
            state: None,
            note: "Answer explicitly with Ctrl+S; Ctrl+R declines the question".into(),
        };
        if draft
            .row
            .metadata
            .get("kind")
            .and_then(serde_json::Value::as_str)
            == Some("mcp-url-elicitation")
        {
            draft.note =
                "Choose accept or decline, then Ctrl+S. Ctrl+R cancels; Esc only closes.".into();
        }
        draft.seed_defaults();
        draft
    }
    fn addressed(&self) -> bool {
        self.target
            .as_ref()
            .is_some_and(|target| target.owner_matches(&self.row))
    }
    fn editable(&self) -> bool {
        self.addressed()
            && self.listed
            && !self.pending
            && self.decision.is_none()
            && self.state.as_ref().is_none_or(FormState::is_open)
    }
    fn save(&mut self) {
        if self.custom_edit {
            if let Some(field) = self.row.fields.get(self.field) {
                self.inputs
                    .insert(field.key.clone(), self.editor.lines().join("\n"));
            }
            return;
        }
        if let Some(field) = self.row.fields.get(self.field).filter(|field| {
            matches!(
                fields::input(field),
                Ok(fields::Input::Text | fields::Input::Number)
            )
        }) {
            let text = self.editor.lines().join("\n");
            self.inputs.insert(field.key.clone(), text.clone());
            let value = if text.is_empty() {
                self.values
                    .get(&field.key)
                    .filter(|value| matches!(value, FormValue::Text(text) if text.is_empty()))
                    .cloned()
            } else if field.kind == "string" {
                Some(FormValue::Text(text))
            } else {
                fields::parse_number(&text).ok().map(FormValue::Number)
            };
            if let Some(value) = value {
                self.values.insert(field.key.clone(), value);
            } else {
                self.values.remove(&field.key);
            }
        }
    }
    pub(super) fn restore(&mut self) {
        if self.custom_edit {
            if let Some(field) = self.row.fields.get(self.field) {
                if let Some(FormValue::Text(value)) = self.values.get(&field.key) {
                    self.inputs
                        .entry(field.key.clone())
                        .or_insert_with(|| value.clone());
                }
            }
        }
        let text = self
            .row
            .fields
            .get(self.field)
            .and_then(|field| self.inputs.get(&field.key))
            .map(String::as_str)
            .unwrap_or("");
        self.editor = TextArea::new(text.split('\n').map(str::to_owned).collect());
        self.editor.set_max_histories(32);
        self.editor.set_wrap_mode(ratatui_textarea::WrapMode::None);
        self.editor.move_cursor(CursorMove::Bottom);
        self.editor.move_cursor(CursorMove::End);
    }
    fn request(&mut self, decision: Option<Decision>) -> Option<Request> {
        if !self.addressed() {
            self.note = "Unattributed question; R cannot address this owner".into();
            return None;
        }
        if self.pending {
            return None;
        }
        self.pending = true;
        self.note = if decision.is_some() {
            "Sending once; answer draft kept until state confirms settlement"
        } else {
            "Reading form state; draft kept"
        }
        .into();
        Some(Request {
            target: self.target.clone()?,
            form: self.row.clone(),
            decision,
        })
    }
    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn key(&mut self, key: KeyEvent) -> Option<Request> {
        if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            self.scroll = if key.code == KeyCode::PageUp {
                self.scroll.saturating_sub(1)
            } else {
                self.scroll.saturating_add(1)
            };
            return None;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('o') {
            return self.request(None);
        }
        if control && matches!(key.code, KeyCode::Char('s' | 'r')) {
            if self.state.as_ref().is_some_and(|state| !state.is_open()) {
                self.note = "Question already settled; no reply sent".into();
                return None;
            }
            if !self.listed && self.decision.is_none() {
                self.note = "Question left the list; Ctrl+O checks its state".into();
                return None;
            }
            let decision = if let Some(previous) = &self.decision {
                if matches!(
                    (key.code, previous),
                    (KeyCode::Char('s'), Decision::Answer(_))
                        | (KeyCode::Char('r'), Decision::Cancel)
                ) {
                    previous.clone()
                } else {
                    self.note = "Original decision unconfirmed; Ctrl+O checks it before changing the answer".into();
                    return None;
                }
            } else if key.code == KeyCode::Char('r') {
                Decision::Cancel
            } else {
                if self.custom_edit {
                    self.note = "Enter chooses custom text; Ctrl+E returns to options".into();
                    return None;
                }
                self.save();
                let (visible, values) = match conditions::project(&self.row, &self.values) {
                    Ok(projected) => projected,
                    Err(error) => {
                        self.note = error;
                        return None;
                    }
                };
                for field in visible
                    .iter()
                    .filter_map(|index| self.row.fields.get(*index))
                {
                    if field.kind == "number" || field.kind == "integer" {
                        if let Some(text) =
                            self.inputs.get(&field.key).filter(|text| !text.is_empty())
                        {
                            if let Err(error) = fields::parse_number(text) {
                                self.note = format!(
                                    "{error}: {}",
                                    grapt_tui_transcript::markdown::literal(&field.key)
                                );
                                return None;
                            }
                        }
                    }
                }
                if let Err(error) = validate(&self.row, &values) {
                    self.note = error;
                    return None;
                }
                Decision::Answer(values)
            };
            self.decision = Some(decision.clone());
            return self.request(Some(decision));
        }
        if !self.editable() {
            self.note =
                "Draft kept; Ctrl+O checks state. Retry only the original decision explicitly"
                    .into();
            return None;
        }
        let visible = self.visible_fields();
        let count = visible.len();
        if count > 0 && matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.save();
            self.custom_edit = false;
            let current = visible
                .iter()
                .position(|index| *index == self.field)
                .unwrap_or(0);
            self.field = visible[if key.code == KeyCode::BackTab {
                (current + count - 1) % count
            } else {
                (current + 1) % count
            }];
            self.restore();
            self.scroll = 0;
            return None;
        }
        let field = self.row.fields.get(self.field)?;
        if control
            && key.code == KeyCode::Char('e')
            && fields::custom(field)
            && matches!(
                fields::input(field),
                Ok(fields::Input::Choice | fields::Input::Multi)
            )
        {
            self.save();
            self.custom_edit = !self.custom_edit;
            self.restore();
            self.scroll = 0;
            self.note = if self.custom_edit {
                "Type custom text; Enter chooses it explicitly"
            } else {
                "Custom draft kept; choose an option or Ctrl+E edits it"
            }
            .into();
            return None;
        }
        if self.custom_edit && key.code == KeyCode::Enter && key.modifiers.is_empty() {
            self.commit_custom();
            return None;
        }
        if self.custom_edit {
            let previous = self.editor.clone();
            self.editor.input(key);
            if self.editor.lines().join("\n").len() > MAX_FIELD {
                self.editor = previous;
                self.note = "Custom text exceeds 16 KiB; edit refused".into();
            }
            self.save();
            return None;
        }
        if let Err(error) = fields::input(field) {
            self.note = error;
        } else if matches!(
            fields::input(field),
            Ok(fields::Input::Choice | fields::Input::Multi)
        ) {
            self.choice_key(key);
        } else if field.kind == "boolean" {
            if key.code == KeyCode::Char(' ') && !control {
                let value = !matches!(self.values.get(&field.key), Some(FormValue::Flag(true)));
                self.values
                    .insert(field.key.clone(), FormValue::Flag(value));
            } else if matches!(key.code, KeyCode::Delete | KeyCode::Backspace) {
                self.values.remove(&field.key);
            } else {
                self.note = "Boolean field: Space chooses true or false".into();
            }
        } else if matches!(
            fields::input(field),
            Ok(fields::Input::Text | fields::Input::Number)
        ) {
            // Clone before edits so paste, undo and newline paths share a hard byte bound.
            let previous = self.editor.clone();
            if matches!(key.code, KeyCode::Delete | KeyCode::Backspace)
                && self.editor.lines().iter().all(String::is_empty)
            {
                self.values.remove(&field.key);
            }
            self.editor.input(key);
            if self.editor.lines().join("\n").len() > MAX_FIELD {
                self.editor = previous;
                self.note = "Field exceeds 16 KiB; edit refused".into();
            }
            self.save();
        } else {
            self.note = "Unsupported field type; question remains unresolved".into();
        }
        None
    }
    pub fn paste(&mut self, text: &str) {
        if !self.editable()
            || !self.row.fields.get(self.field).is_some_and(|field| {
                self.custom_edit
                    || matches!(
                        fields::input(field),
                        Ok(fields::Input::Text | fields::Input::Number)
                    )
            })
        {
            self.note = "Paste needs an editable text or numeric field; draft kept".into();
            return;
        }
        if text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
            || self
                .editor
                .lines()
                .join("\n")
                .len()
                .saturating_add(text.len())
                > MAX_FIELD
        {
            self.note = "Paste is not bounded text (16 KiB); nothing inserted".into();
            return;
        }
        self.editor.insert_str(text);
        self.save();
    }
}
