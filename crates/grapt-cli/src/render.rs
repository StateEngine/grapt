//! Human rendering of a run, projected from durable truth only.
//!
//! The frozen oracle `run` wrote each completed answer to stdout and its
//! progress, step labels and errors to stderr, so command substitution held the
//! answer alone. This keeps that division. It reads only durable message pages:
//! live events stay advisory and never become user-visible text.

use std::collections::BTreeSet;
use std::io::Write;

use grapt_api::u3a::{SessionMessageAssistantDtoContentItem, SessionMessageInfoDto};

use crate::{BoundedOutput, OutputError};

/// Which of this run's assistant messages a text-mode run has already shown.
#[derive(Debug)]
pub(crate) struct TextRendering {
    labelled: BTreeSet<String>,
    rendered: BTreeSet<String>,
    reasoning: bool,
}

impl TextRendering {
    pub(crate) fn new(reasoning: bool) -> Self {
        Self {
            labelled: BTreeSet::new(),
            rendered: BTreeSet::new(),
            reasoning,
        }
    }

    /// Labels each new assistant message once, and writes its answer once it
    /// completes. `run` is this run's rows, newest-first; `terminal` renders
    /// whatever this run produced, complete or not.
    pub(crate) fn render<O: Write, E: Write>(
        &mut self,
        run: &[SessionMessageInfoDto],
        output: &mut BoundedOutput<O, E>,
        terminal: bool,
    ) -> Result<(), OutputError> {
        // An answer is read oldest-first.
        for row in run.iter().rev() {
            if let Some((id, text)) = mcp_notice(row) {
                // A notice is the product's, not the answer's: stderr.
                if self.rendered.insert(id) {
                    output.diagnostic(&format!("[system] {text}"))?;
                }
                continue;
            }
            let SessionMessageInfoDto::Assistant(assistant) = row else {
                continue;
            };
            let id = assistant.id().get().as_inner().to_string();
            if self.rendered.contains(&id) {
                continue;
            }
            if self.labelled.insert(id.clone()) {
                output.diagnostic(&format!(
                    "> {} · {}",
                    assistant.agent().get().as_inner().as_str(),
                    assistant.model().get().id().get().as_inner().as_str()
                ))?;
            }
            // `finish` is present while a message streams (`unknown`); only
            // the durable completion time says the answer is final.
            let complete = assistant.time().get().completed().as_present().is_some();
            if !complete && !terminal {
                continue;
            }
            for item in assistant.content().get().as_slice() {
                match item {
                    SessionMessageAssistantDtoContentItem::Text(text) => {
                        let text = text.text().get().as_str().trim();
                        if !text.is_empty() {
                            output.text(text)?;
                        }
                    }
                    SessionMessageAssistantDtoContentItem::Reasoning(reasoning)
                        if self.reasoning =>
                    {
                        let text = reasoning.text().get().as_str().trim();
                        if !text.is_empty() {
                            output.text(&format!("Thinking: {text}"))?;
                        }
                    }
                    SessionMessageAssistantDtoContentItem::Reasoning(_)
                    | SessionMessageAssistantDtoContentItem::Tool(_) => {}
                }
            }
            self.rendered.insert(id);
        }
        Ok(())
    }
}

/// A durable notice row's id and text: an MCP notice, a not-run-yet one,
/// why an execution stopped on a repeating, full context or an unusable
/// summary, or a config key SE accepts but does not apply.
fn mcp_notice(row: &SessionMessageInfoDto) -> Option<(String, &str)> {
    let SessionMessageInfoDto::System(system) = row else {
        return None;
    };
    let notice = system
        .metadata()
        .as_present()
        .and_then(|metadata| serde_json::to_value(metadata).ok())
        .is_some_and(|metadata| {
            metadata["mcpUnavailable"] == true
                || metadata["notRunYet"] == true
                || metadata["contextRepeated"] == true
                || metadata["configIgnored"] == true
                || metadata["compactionFailed"] == true
        });
    notice.then(|| {
        (
            system.id().get().as_inner().to_string(),
            system.text().get().as_str(),
        )
    })
}

/// The failure this run's execution recorded durably, if any: an execution
/// failure notice or an assistant error among its rows (newest-first).
pub(crate) fn execution_failure(run: &[SessionMessageInfoDto]) -> Option<String> {
    run.iter().rev().find_map(|row| match row {
        SessionMessageInfoDto::System(system) => {
            let failed = system
                .metadata()
                .as_present()
                .and_then(|metadata| serde_json::to_value(metadata).ok())
                .is_some_and(|metadata| metadata["executionFailure"] == true);
            failed.then(|| system.text().get().as_str().to_owned())
        }
        SessionMessageInfoDto::Assistant(assistant) => assistant
            .error()
            .as_present()
            .map(|error| error.message().get().as_str().to_owned()),
        _ => None,
    })
}

/// The not-run-yet notice for `admitted`: its prompt stays queued because
/// something blocked its turn before it began, so history shows the notice
/// but not yet the prompt. Only an exact messageID match is this run's.
pub(crate) fn not_run_yet_for(rows: &[SessionMessageInfoDto], admitted: &str) -> Option<String> {
    rows.iter().find_map(|row| match row {
        SessionMessageInfoDto::System(system) => system
            .metadata()
            .as_present()
            .and_then(|metadata| serde_json::to_value(metadata).ok())
            .is_some_and(|metadata| {
                metadata["notRunYet"] == true && metadata["messageID"] == admitted
            })
            .then(|| system.text().get().as_str().to_owned()),
        _ => None,
    })
}

/// Where a page, newest-first, holds the user message this run was admitted
/// as: every row before it is this run's.
pub(crate) fn admission(rows: &[SessionMessageInfoDto], admitted: &str) -> Option<usize> {
    rows.iter().position(|row| {
        matches!(row, SessionMessageInfoDto::User(user)
            if user.id().get().as_inner().to_string() == admitted)
    })
}
