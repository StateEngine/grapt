//! One bounded import job keeps filesystem and HTTP work off the protocol loop.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
use super::{commands, first_prompt::Submissions, CommandId};
use crate::config::Config;
use grapt_cli::{
    command::import::{ImportCommand, ImportKind, ImportSource},
    CliLimits,
};
use grapt_client::{wire::SessionWireId, Client, Connection};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender},
};

#[derive(Clone, PartialEq, Eq)]
struct Selection {
    directory: String,
    root: PathBuf,
    sources: Vec<ImportSource>,
}
struct Pending {
    result: Receiver<(Vec<String>, bool)>,
    selection: Selection,
    text: String,
    session: Option<SessionWireId>,
    apply: bool,
    /// The command that started the job, kept until its result settles.
    origin: CommandId,
}
#[derive(Default)]
pub(super) struct Import {
    pending: Option<Pending>,
    preview: Option<Selection>,
}

pub(super) fn matches(text: &str) -> bool {
    grapt_client::composer_commands::argument(text, "/import").is_some()
}

impl Import {
    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn start<C: Connection>(
        &mut self,
        client: &Client<C>,
        config: &Config,
        text: String,
        wake: SyncSender<()>,
        (ready, origin): (bool, CommandId),
    ) -> commands::Completion {
        let mut done = commands::Completion::new(&text);
        done.session = client.selected().cloned();
        let request = (|| {
            if self.busy() {
                return Err("An import is already running; draft kept.");
            }
            if !ready {
                return Err("Use /import when the service is connected and this session is idle.");
            }
            let words: Vec<_> = text.split_whitespace().skip(1).collect();
            let (sources, apply) = match words.as_slice() {
                [] => (vec![ImportSource::Claude, ImportSource::Codex], false),
                ["claude"] => (vec![ImportSource::Claude], false),
                ["codex"] => (vec![ImportSource::Codex], false),
                ["apply"] => (
                    self.preview
                        .as_ref()
                        .ok_or("Preview with /import before applying.")?
                        .sources
                        .clone(),
                    true,
                ),
                _ => return Err("Use /import [claude|codex] to preview, then /import apply."),
            };
            let directory = client
                .form_location()
                .ok_or("Select a workspace before importing.")?
                .directory;
            let selection = Selection {
                directory,
                root: config.root.clone(),
                sources,
            };
            if apply && self.preview.as_ref() != Some(&selection) {
                return Err("Workspace changed; run /import again before applying.");
            }
            Ok((selection, apply))
        })();
        let (selection, apply) = match request {
            Ok(request) => request,
            Err(error) => {
                done.notice = error.into();
                return done;
            }
        };
        let (sender, result) = mpsc::sync_channel(1);
        let selected = selection.clone();
        let spawned = std::thread::Builder::new().name("tui-import".into()).spawn(move || {
            let report = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime.block_on(async {
                    match tokio::time::timeout(std::time::Duration::from_secs(120), run(&selected, apply)).await {
                        Ok(result) => result, Err(_) => (vec!["Import timed out; earlier items may have been imported. Preview again before retrying.".into()], false),
                    }
                }),
                Err(_) => (vec!["Import worker could not start.".into()], false),
            };
            let _ = sender.send(report);
            let _ = wake.try_send(());
        });
        if spawned.is_err() {
            done.notice = "Import worker could not start; draft kept.".into();
            return done;
        }
        self.preview = None;
        self.pending = Some(Pending {
            result,
            selection,
            text,
            session: client.selected().cloned(),
            apply,
            origin,
        });
        done.notice = if apply {
            "Importing selected setup…"
        } else {
            "Reading import preview…"
        }
        .into();
        done
    }

    pub(super) fn advance(&mut self, submissions: &mut Submissions) -> Option<String> {
        if submissions.command.is_some() {
            return None;
        }
        let pending = self.pending.as_ref()?;
        let (mut rows, success) = match pending.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => (
                vec!["Import worker stopped; inspect earlier items before retrying.".into()],
                false,
            ),
        };
        let pending = self.pending.take()?;
        let mut done = commands::Completion::new(&pending.text);
        done.session = pending.session;
        done.consume = success;
        if success && !pending.apply {
            self.preview = Some(pending.selection);
            rows.push("Close this pane, then /import apply to import the selected sources. Existing declarations win; hooks enable future command execution.".into());
        }
        done.notice = if success {
            if pending.apply {
                "Import finished. Reopen the session to reload setup."
            } else {
                "Import preview ready; no setup written."
            }
        } else {
            "Import stopped; see the result. Draft kept."
        }
        .into();
        done.view = Some(commands::View::Text(rows));
        let notice = done.notice.clone();
        submissions.command = Some((pending.origin, done));
        Some(notice)
    }
}

async fn run(selection: &Selection, apply: bool) -> (Vec<String>, bool) {
    let mut rows = vec![format!(
        "IMPORT {}",
        if apply {
            "APPLY"
        } else {
            "PREVIEW — no setup written"
        }
    )];
    let mut success = true;
    let limits = CliLimits::production();
    for source in &selection.sources {
        for kind in [
            ImportKind::Mcp,
            ImportKind::Instructions,
            ImportKind::Skills,
            ImportKind::Hooks,
        ] {
            if kind == ImportKind::Hooks && *source == ImportSource::Codex {
                continue;
            }
            let command = ImportCommand {
                kind,
                source: *source,
                service_root: Some(selection.root.clone()),
                directory: Some(selection.directory.clone()),
                apply,
            };
            match grapt_cli::migration::run(command, limits).await {
                Ok(report) => rows.extend(report),
                Err((_, reason)) => {
                    rows.push(format!("{} {}: {reason}", source.label(), kind.label()));
                    success = false;
                    if apply {
                        rows.push(
                            "Earlier items may have been imported; no later items were applied."
                                .into(),
                        );
                        return (rows, false);
                    }
                }
            }
        }
        if *source == ImportSource::Claude {
            match grapt_cli::migration::agents::report(
                &selection.root,
                &selection.directory,
                limits,
            )
            .await
            {
                Ok(report) => rows.extend(report),
                Err(reason) => {
                    rows.push(reason);
                    success = false;
                }
            }
        }
    }
    (rows, success)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn import_apply_requires_a_preview_and_stays_out_of_model_submission() {
        let client = Client::new(
            grapt_client::ContractDouble::new(),
            grapt_client::Bounds::PRODUCTION,
        );
        let config = Config {
            root: "/unused-import-profile".into(),
            directory: None,
            session: None,
            expected: None,
            reasoning_effort: None,
            light: false,
            notifications: false,
            prompt: None,
            preset: None,
        };
        let (wake, _ready) = mpsc::sync_channel(1);
        let mut imports = Import::default();
        let origin = crate::backend::CommandId::TEST;
        let done = imports.start(
            &client,
            &config,
            "/import apply".into(),
            wake,
            (true, origin),
        );
        assert!(done.notice.contains("Preview with /import"));
        assert!(!done.consume);
        assert!(!imports.busy());
        assert!(!super::super::commands::submits_prompt("/import"));
        assert!(!super::super::commands::submits_prompt("/import apply"));
        assert!(!matches("/important"));
    }
}
