//! Apache-2.0, Copyright 2025 OpenAI.
//! Modified by Grapt: normalized retry/failure banner with no replay policy.
use crate::ui::Ui;
use grapt_client::cells::ellipsize_words as ellipsize;
#[cfg(test)]
use grapt_client::{cells, projection::feedback::Feedback};
use grapt_tui_transcript::markdown::literal;
mod draft;
pub use draft::FailedDraft;

fn routine_status(status: &str) -> bool {
    matches!(
        status,
        "Connected" | "Connecting…" | "Loading models…" | "Choose an agent and model"
    )
}

pub fn rows(ui: &Ui, width: usize) -> Vec<String> {
    let (title, message, action) = match &ui.feedback {
        Some(feedback) => feedback.banner(
            !crate::keys::draft(ui).is_empty(),
            ui.transcript.latest_is_automated(),
        ),
        None if !routine_status(&ui.status) => (
            if crate::keys::draft(ui).is_empty() {
                "Service notice"
            } else {
                "Service notice · draft kept"
            }
            .into(),
            ui.status.as_str(),
            "Details above; keep editing.",
        ),
        None => match &ui.command_notice {
            Some(message) => (
                "Conversation updated".into(),
                message.as_str(),
                "Keep editing; Enter sends when ready.",
            ),
            None => return Vec::new(),
        },
    };
    let safe = |text: &str| literal(text).replace('\n', " ");
    let mut rows = vec![
        ellipsize(&safe(&title), width),
        ellipsize(&safe(message), width),
    ];
    if ui.feedback.is_some() && !routine_status(&ui.status) && ui.status != message {
        rows.push(ellipsize(&safe(&ui.status), width));
    }
    rows.push(ellipsize(&safe(action), width));
    rows
}

#[cfg(test)]
mod tests;
