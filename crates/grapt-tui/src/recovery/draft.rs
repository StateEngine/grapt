//! Restore complete failed input for editing; sending stays an explicit key.
use grapt_client::wire::ExecutionWireId;

#[derive(Default)]
pub struct FailedDraft {
    seen: Option<(Option<ExecutionWireId>, usize)>,
    submitted: Option<String>,
}
impl FailedDraft {
    pub fn submitted(&mut self, text: &str) {
        self.submitted = grapt_tui_transcript::prompt::valid_prompt(text).then(|| text.to_owned());
    }

    pub fn restore(
        &mut self,
        execution: Option<&ExecutionWireId>,
        prompt: Option<(usize, &str)>,
        editor: &mut crate::editor::Editor<'static>,
    ) -> bool {
        let Some((ordinal, text)) = prompt else {
            return false;
        };
        // Failure feedback can precede the transcript's durable input refresh.
        // Never mistake the previous successful prompt for the acknowledged send.
        if self.submitted.as_deref().is_some_and(|sent| sent != text) {
            return false;
        }
        let key = (execution.cloned(), ordinal);
        if self.seen.as_ref() == Some(&key) {
            return false;
        }
        self.seen = Some(key);
        // An edited follow-up always belongs to the user, including whitespace.
        if !editor.lines().join("\n").is_empty() {
            return false;
        }
        *editor = crate::editor::Editor::new(text.split('\n').map(str::to_owned).collect());
        editor.set_max_histories(128);
        editor.set_wrap_mode(ratatui_textarea::WrapMode::None);
        editor.move_cursor(ratatui_textarea::CursorMove::Bottom);
        editor.move_cursor(ratatui_textarea::CursorMove::End);
        true
    }
}
