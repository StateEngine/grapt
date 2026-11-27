//! Keep submitted drafts and command completions through bounded protocol work.
//! Startup intent.
// Copyright 2025 OpenAI. Modified for the existing Session and command owners.
use super::{commands, Command, CommandId};
use crate::images::Settlement;
use grapt_client::{wire::SessionWireId, Client, Connection};

type Navigation = (grapt_client::client::HistoryMove, bool);
pub(super) struct Dispatch {
    pub notice: String,
    pub navigation: Option<Navigation>,
    pub models: Option<Vec<grapt_client::wire::ModelRow>>,
}

struct Prepared {
    session: SessionWireId,
    prompt: String,
    command: Option<commands::Completion>,
    startup: bool,
    files: Vec<grapt_api::u3a::PromptInputFileAttachmentDto>,
    image: bool,
    /// The command that started this held prompt, kept until it settles.
    origin: CommandId,
}

struct OperatorCommand {
    done: commands::Completion,
    location: Option<grapt_client::wire::FormLocation>,
    catalog: bool,
    origin: CommandId,
}

#[derive(Default)]
pub(super) struct Submissions {
    prepared: Option<Prepared>,
    /// Each result names the command that started the work it settles.
    pub outcome: Option<(CommandId, (String, bool, bool))>,
    pub command: Option<(CommandId, commands::Completion)>,
    operator: Option<OperatorCommand>,
    pub image_result: Option<(CommandId, Settlement)>,
    retry_image: Option<(SessionWireId, CommandId)>,
}

impl Submissions {
    pub fn preparing(&self) -> bool {
        self.prepared.is_some()
    }

    pub fn submit<C: Connection>(
        &mut self,
        client: &mut Client<C>,
        text: String,
        startup: bool,
        origin: CommandId,
    ) -> Dispatch {
        self.submit_with_files(client, text, startup, Vec::new(), false, origin)
    }

    pub fn submit_image<C: Connection>(
        &mut self,
        client: &mut Client<C>,
        prompt: crate::images::Prompt,
        origin: CommandId,
    ) -> Dispatch {
        self.submit_with_files(client, prompt.text, false, prompt.files, true, origin)
    }

    fn submit_with_files<C: Connection>(
        &mut self,
        client: &mut Client<C>,
        text: String,
        startup: bool,
        files: Vec<grapt_api::u3a::PromptInputFileAttachmentDto>,
        image: bool,
        origin: CommandId,
    ) -> Dispatch {
        let mut dispatched = Dispatch {
            notice: "Preparing launch permissions; draft kept".into(),
            navigation: None,
            models: None,
        };
        if self.preparing() {
            return dispatched;
        }
        if self.operator.is_some()
            && (text.trim() == "/mcps"
                || grapt_client::composer_commands::argument(&text, "/mcp").is_some()
                || grapt_client::composer_commands::argument(&text, "/worktree").is_some())
        {
            dispatched.notice = "An operator command is still completing; draft kept.".into();
            self.refuse(origin, (text, !startup), image);
            return dispatched;
        }
        if startup || commands::submits_prompt(&text) {
            let movement = grapt_client::client::HistoryMove::Latest;
            dispatched.navigation = Some((
                movement,
                matches!(client.browse_history(movement), Ok(true)),
            ));
        }
        if !startup {
            let operator_pending = client.operator_request_pending();
            let done = if image {
                commands::prepare(client, &text)
            } else {
                commands::execute(client, &text)
            };
            if let Some(mut done) = done {
                if let Some(prompt) = done.prepared_prompt.take() {
                    if image && self.images_blocked(client) {
                        done.notice = image_conflict();
                        dispatched.notice = done.notice.clone();
                        self.image_result = rejected(image, origin);
                        self.command = Some((origin, done));
                    } else if let Err(error) = client.create_draft_session() {
                        done.notice = error;
                        self.image_result = rejected(image, origin);
                        self.command = Some((origin, done));
                    } else if matches!(client.launch_preset_ready(), Ok(false)) {
                        self.hold(client, prompt, Some(done), false, files, (image, origin));
                    } else {
                        dispatched.notice =
                            self.finish(client, prompt, Some(done), false, files, (image, origin));
                    }
                } else {
                    self.image_result = rejected(image, origin);
                    if matches!(done.view, Some(commands::View::Models)) {
                        dispatched.models = Some(client.models().to_vec());
                    }
                    dispatched.notice = done.notice.clone();
                    if !operator_pending
                        && (client.operator_request_pending() || client.mcp_catalog_pending())
                    {
                        // The request owns its later completion; release the
                        // composer now so commands and Session changes stay live.
                        self.outcome = Some((origin, (done.text.clone(), false, false)));
                        self.operator = Some(OperatorCommand {
                            done,
                            location: client.form_location(),
                            catalog: client.mcp_catalog_pending(),
                            origin,
                        });
                    } else {
                        self.command = Some((origin, done));
                    }
                }
                return dispatched;
            }
        }
        if image && self.images_blocked(client) {
            self.refuse(origin, (text, true), image);
            dispatched.notice = image_conflict();
            return dispatched;
        }
        if let Err(error) = client.create_draft_session() {
            self.refuse(origin, (text, !startup), image);
            dispatched.notice = error;
        } else if matches!(client.launch_preset_ready(), Ok(false)) {
            self.hold(client, text, None, startup, files, (image, origin));
        } else {
            dispatched.notice = self.finish(client, text, None, startup, files, (image, origin));
        }
        dispatched
    }

    /// A prompt that will not be sent, and its image, settled for the
    /// command that sent it.
    fn refuse(&mut self, origin: CommandId, (text, human): (String, bool), image: bool) {
        self.outcome = Some((origin, (text, false, human)));
        self.image_result = rejected(image, origin);
    }

    fn images_blocked<C: Connection>(&self, client: &Client<C>) -> bool {
        // One uncertain image snapshot follows the global draft. Other Sessions
        // may navigate/read, but cannot replace it with a second image send.
        self.retry_image.is_some() || client.has_unconfirmed_prompt()
    }

    fn hold<C: Connection>(
        &mut self,
        client: &Client<C>,
        prompt: String,
        command: Option<commands::Completion>,
        startup: bool,
        files: Vec<grapt_api::u3a::PromptInputFileAttachmentDto>,
        (image, origin): (bool, CommandId),
    ) {
        if let Some(session) = client.selected() {
            self.prepared = Some(Prepared {
                session: session.clone(),
                prompt,
                command,
                startup,
                files,
                image,
                origin,
            });
        } else {
            self.outcome = Some((
                origin,
                (command.map_or(prompt, |done| done.text), false, !startup),
            ));
            self.image_result = rejected(image, origin);
        }
    }

    /// The HTTP completion wakes the existing driver; no polling timer or retry.
    pub fn advance<C: Connection>(&mut self, client: &mut Client<C>) -> Option<String> {
        if let Some(notice) = self.advance_operator(client) {
            return Some(notice);
        }
        let held = self.prepared.as_ref()?;
        if client.selected() == Some(&held.session)
            && matches!(client.launch_preset_ready(), Ok(false))
        {
            return None;
        }
        let held = self.prepared.take()?;
        let origin = held.origin;
        if client.selected() != Some(&held.session) {
            self.outcome = Some((
                origin,
                (
                    held.command.map_or(held.prompt, |done| done.text),
                    false,
                    !held.startup,
                ),
            ));
            self.image_result = rejected(held.image, origin);
            return Some("Session changed; first prompt kept unsent".into());
        }
        Some(self.finish(
            client,
            held.prompt,
            held.command,
            held.startup,
            held.files,
            (held.image, origin),
        ))
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn advance_operator<C: Connection>(&mut self, client: &mut Client<C>) -> Option<String> {
        if self.command.is_some() {
            return None;
        }
        let held = self.operator.take()?;
        let origin = held.origin;
        let mut done = held.done;
        if !done.keep_for(client.selected()) || held.location != client.form_location() {
            let worktree =
                grapt_client::composer_commands::argument(&done.text, "/worktree").is_some();
            client.cancel_operator_request();
            client.cancel_mcp_catalog();
            done.consume = false;
            done.view = None;
            done.notice = if worktree {
                format!(
                    "Session or location changed; draft kept. {}",
                    client.worktree_uncertainty()
                )
            } else {
                "Session or location changed; operator result discarded; draft kept.".into()
            };
            if worktree {
                // Keep the safety notice through publication's second fence;
                // the old command still cannot consume or replace the draft.
                done.session = client.selected().cloned();
                done.replacement = None;
                done.worktree_failure(client.worktree_request_details());
            }
            let notice = done.notice.clone();
            self.command = Some((origin, done));
            return Some(notice);
        }
        if client.mcp_content_pending() {
            match client.poll_mcp_content() {
                Ok(None) => {
                    self.operator = Some(OperatorCommand { done, ..held });
                    return None;
                }
                Ok(Some(content)) => {
                    done.view = None;
                    commands::mcp::complete(client, content, &mut done);
                    if let Some(prompt) = done.prepared_prompt.take() {
                        if matches!(client.launch_preset_ready(), Ok(false)) {
                            self.hold(
                                client,
                                prompt,
                                Some(done),
                                false,
                                Vec::new(),
                                (false, origin),
                            );
                            return Some("Preparing MCP content for submission".into());
                        }
                        return Some(self.finish(
                            client,
                            prompt,
                            Some(done),
                            false,
                            Vec::new(),
                            (false, origin),
                        ));
                    }
                }
                Err(error) => {
                    done.notice = error.to_string();
                    done.view = Some(commands::View::Notice(done.notice.clone()));
                }
            }
            let notice = done.notice.clone();
            self.command = Some((origin, done));
            return Some(notice);
        }
        let side = client.side_question_pending();
        let worktree = client.worktree_pending();
        let result = if client.claude_plugin_pending() {
            client.poll_claude_plugin()
        } else if worktree {
            client.poll_worktree()
        } else if side {
            client.poll_side_question().map(|answer| {
                answer.map(|answer| {
                    let question = grapt_client::composer_commands::argument(&done.text, "/side")
                        .unwrap_or_default()
                        .trim();
                    commands::side_answer(question, answer)
                })
            })
        } else if held.catalog {
            if client.mcp_catalog_pending() {
                Ok(None)
            } else if let Some(error) = client.mcp_refusal() {
                Err(grapt_client::ClientError::Mcp(error.into()))
            } else {
                let verbose = grapt_client::composer_commands::argument(&done.text, "/mcp")
                    .is_some_and(|args| args.eq_ignore_ascii_case("verbose"));
                Ok(Some(if verbose {
                    grapt_client::view::mcp_detail_lines(client, 120)
                } else {
                    grapt_client::view::mcp_summary_lines(client, 120)
                }))
            }
        } else if client.mcp_auth_pending() {
            client.poll_mcp_auth()
        } else if grapt_client::composer_commands::argument(&done.text, "/worktree").is_some() {
            Err(grapt_client::ClientError::Worktree(format!(
                "Worktree request canceled; draft kept. {}",
                client.worktree_uncertainty()
            )))
        } else {
            Err(grapt_client::ClientError::Mcp(
                "Operator request canceled after Session or project change.".into(),
            ))
        };
        // /worktree open may select its NEW session before a later attachment
        // read fails. Preserve that uncertainty notice under the resulting
        // selection, after the captured source/session fence already passed.
        if worktree {
            done.session = client.selected().cloned();
        }
        match result {
            Ok(None) => {
                self.operator = Some(OperatorCommand { done, ..held });
                return None;
            }
            Ok(Some(lines)) => {
                done.consume = true;
                done.view = Some(if held.catalog {
                    commands::View::Mcp {
                        rows: lines,
                        verbose: grapt_client::composer_commands::argument(&done.text, "/mcp")
                            .is_some_and(|args| args.eq_ignore_ascii_case("verbose")),
                    }
                } else {
                    commands::View::Text(lines)
                });
                done.notice = if side {
                    "Side answer opened; close the pane to return to the conversation."
                } else {
                    "Connected"
                }
                .into();
            }
            Err(error) => {
                done.notice = error.to_string();
                if grapt_client::composer_commands::argument(&done.text, "/worktree").is_some() {
                    done.worktree_failure(client.worktree_request_details());
                } else {
                    done.view = Some(commands::View::Notice(done.notice.clone()));
                }
            }
        }
        let notice = done.notice.clone();
        self.command = Some((origin, done));
        Some(notice)
    }

    pub fn retry_result(&mut self, session: Option<&SessionWireId>, accepted: bool) {
        if accepted
            && self
                .retry_image
                .as_ref()
                .is_some_and(|(held, _)| Some(held) == session)
        {
            self.image_result = self
                .retry_image
                .take()
                .map(|(_, origin)| (origin, Settlement::Accepted));
        }
    }

    pub fn cancel_for(&mut self, command: &Command) {
        if matches!(
            command,
            Command::Select(_)
                | Command::Interrupt(_)
                | Command::InterruptAndResume(_)
                | Command::Quit(_)
        ) {
            if let Some(held) = self.prepared.take() {
                let origin = held.origin;
                self.image_result = rejected(held.image, origin);
                self.outcome = Some((
                    origin,
                    (
                        held.command.map_or(held.prompt, |done| done.text),
                        false,
                        !held.startup,
                    ),
                ));
            }
        }
    }

    fn finish<C: Connection>(
        &mut self,
        client: &mut Client<C>,
        prompt: String,
        command: Option<commands::Completion>,
        startup: bool,
        files: Vec<grapt_api::u3a::PromptInputFileAttachmentDto>,
        (image, origin): (bool, CommandId),
    ) -> String {
        let mut files = files;
        if let Some(done) = &command {
            files.extend_from_slice(&done.prepared_files);
        }
        let result = if startup {
            crate::launch::submit_startup(client, &prompt)
        } else if files.is_empty() {
            crate::launch::submit(client, &prompt)
        } else {
            crate::launch::submit_files(client, &prompt, &files)
        };
        self.image_result = image.then(|| {
            let settlement = if result.is_ok() {
                Settlement::Accepted
            } else if client.has_unconfirmed_prompt() {
                self.retry_image = client.selected().map(|id| (id.clone(), origin));
                Settlement::Unconfirmed
            } else {
                Settlement::Rejected
            };
            (origin, settlement)
        });
        let notice = result
            .as_ref()
            .map_or_else(ToString::to_string, |()| "Connected".into());
        if let Some(mut done) = command {
            done.session = client.selected().cloned();
            done.consume = result.is_ok();
            if done.consume {
                done.replacement = None;
            }
            done.notice = notice.clone();
            if result.is_err() {
                done.view = Some(commands::View::Notice(notice.clone()));
            }
            self.command = Some((origin, done));
        } else {
            self.outcome = Some((origin, (prompt, result.is_ok(), !startup)));
        }
        notice
    }
}

pub(super) fn ready<C: Connection>(client: &Client<C>, startup: crate::launch::Readiness) -> bool {
    startup.can_submit()
        && client.ready_to_chat()
        && matches!(client.launch_preset_ready(), Ok(true))
        && !crate::launch::protected(client)
}

pub(super) fn notice<C: Connection>(client: &Client<C>, status: &str) -> String {
    if let Some(reason) = client.service_stop_notice() {
        return reason.to_owned();
    }
    let status = if status != "Connected" || client.ready_to_chat() {
        status.to_owned()
    } else if client.catalog_pending() {
        "Loading models…".into()
    } else if client.models().is_empty() {
        format!(
            "No model available: {}",
            client
                .catalog_status()
                .unwrap_or("choose a configured provider")
        )
    } else {
        "Choose an agent and model".into()
    };
    // A live-event gap recovers by itself and is shown as its own neutral,
    // self-clearing line. Routing it through `status` turned it into a pinned
    // warning block with filler, which outlived the condition it described.
    status
}

#[cfg(test)]
mod tests;

fn image_conflict() -> String {
    "Resolve the earlier submission with Alt+R in its Session before sending more images; draft kept".into()
}

/// An image prompt that will not be sent, settled for the command that sent it.
fn rejected(image: bool, origin: CommandId) -> Option<(CommandId, Settlement)> {
    image.then_some((origin, Settlement::Rejected))
}
