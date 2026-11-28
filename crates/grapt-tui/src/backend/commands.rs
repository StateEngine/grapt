//! Slash dispatch.
//! Clear-session behavior also.
//! Apache-2.0, Copyright 2025 OpenAI (licenses/openai-codex/).
//! Modified by Grapt: existing Client effects and native pickers, with
//! command completion separate from model submission and an editable init task.
use grapt_client::{
    composer_commands::{argument, INIT_PROMPT},
    view, Client, Connection,
};
mod export;
pub(super) mod mcp;
mod permissions;
mod plugin;
mod sandbox;
mod selection;
mod worktree;

#[derive(Debug)]
pub enum View {
    Status,
    Grants(crate::grants::Snapshot),
    Models,
    Sessions,
    Text(Vec<String>),
    Mcp {
        rows: Vec<String>,
        verbose: bool,
    },
    Notice(String),
    /// Existing wrapped result pane accompanied by a persistent recovery notice.
    NoticeText {
        notice: String,
        rows: Vec<String>,
    },
}

#[derive(Debug)]
pub struct Completion {
    pub text: String,
    pub session: Option<grapt_client::wire::SessionWireId>,
    pub consume: bool,
    pub replacement: Option<String>,
    pub view: Option<View>,
    pub notice: String,
    pub prepared_prompt: Option<String>,
    pub prepared_files: Vec<grapt_api::u3a::PromptInputFileAttachmentDto>,
}
impl Completion {
    pub(super) fn new(text: &str) -> Self {
        Self {
            text: text.into(),
            session: None,
            consume: false,
            replacement: None,
            view: None,
            notice: "Connected".into(),
            prepared_prompt: None,
            prepared_files: Vec::new(),
        }
    }
    /// A queued session selection may follow the command before publication.
    /// Its snapshot and draft completion must stay with the session that ran it.
    pub fn keep_for(&mut self, session: Option<&grapt_client::wire::SessionWireId>) -> bool {
        if self.session.as_ref() == session {
            return true;
        }
        self.consume = false;
        self.replacement = None;
        // Safety details describe a prior effect, not a draft for the new
        // Session. Navigation must not discard them before publication.
        if matches!(self.view, Some(View::NoticeText { .. })) {
            return false;
        }
        self.view = None;
        self.notice = "Session changed after the command; draft kept.".into();
        false
    }

    pub fn safety_only(&self) -> bool {
        !self.consume
            && self.replacement.is_none()
            && self.prepared_prompt.is_none()
            && matches!(self.view, Some(View::NoticeText { .. }))
    }

    fn finish(&mut self, result: Result<(), grapt_client::ClientError>) {
        match result {
            Ok(()) => self.consume = true,
            Err(error) => self.notice = error.to_string(),
        }
    }

    pub(super) fn worktree_failure(&mut self, coordinates: Vec<String>) {
        self.consume = false;
        self.replacement = None;
        let mut rows = vec!["WORKTREE REQUEST NOT COMPLETED".into()];
        rows.extend(coordinates);
        rows.push("No automatic retry. Inspect uncertain effects before continuing.".into());
        rows.extend(self.notice.lines().map(str::to_owned));
        self.view = Some(View::NoticeText {
            notice: "Worktree request requires attention; draft kept. Inspect the command result."
                .into(),
            rows,
        });
    }
}

/// Operator effects stay on the protocol worker and bind the selected Session.
pub(crate) fn revoke_grant<C: Connection>(
    client: &mut Client<C>,
    request: crate::grants::Request,
) -> Completion {
    let mut done = Completion::new("");
    done.session = client.selected().cloned();
    if client.selected() != Some(&request.session) {
        done.notice = "Session changed; no consent was withdrawn.".into();
        return done;
    }
    done.notice = match client.revoke_sandbox_grant(&request.grant) {
        Ok(message) => message,
        Err(error) => {
            format!("Revocation unconfirmed: {error}. Retry before relying on withdrawal.")
        }
    };
    done.view = crate::grants::Snapshot::read(client, done.notice.clone()).map(View::Grants);
    done
}

#[derive(Clone, Copy)]
enum Parsed {
    Help,
    Commands,
    Status,
    Model,
    Effort,
    Permissions,
    New,
    Resume,
    Diff,
    Context,
    Init,
    Mcp,
    Import,
    Plugin,
    Compact,
    Reconcile,
    Rename,
    Undo,
    Rewind,
    Fork,
    Side,
    Worktree,
    Sandbox,
    Export,
}

fn parse(text: &str) -> Option<Parsed> {
    let normalized = crate::actions::canonical(text);
    let text = normalized.as_deref().unwrap_or(text);
    let command = text.trim();
    Some(match command {
        "/help" => Parsed::Help,
        "/commands" => Parsed::Commands,
        "/status" => Parsed::Status,
        "/reconcile" => Parsed::Reconcile,
        _ if argument(text, "/import").is_some() => Parsed::Import,
        _ if argument(text, "/plugin").is_some() => Parsed::Plugin,
        _ if argument(text, "/rename").is_some() => Parsed::Rename,
        _ if argument(text, "/undo").is_some() => Parsed::Undo,
        _ if argument(text, "/rewind").is_some() => Parsed::Rewind,
        _ if argument(text, "/fork").is_some() => Parsed::Fork,
        _ if argument(text, "/side").is_some() => Parsed::Side,
        _ if argument(text, "/worktree").is_some() => Parsed::Worktree,
        _ if argument(text, "/sandbox").is_some() => Parsed::Sandbox,
        _ if argument(text, "/export").is_some() => Parsed::Export,
        _ if argument(text, "/model").is_some() => Parsed::Model,
        _ if argument(text, "/effort").is_some() => Parsed::Effort,
        _ if argument(text, "/permissions").is_some() => Parsed::Permissions,
        _ if argument(text, "/new").is_some() || argument(text, "/clear").is_some() => Parsed::New,
        _ if argument(text, "/resume").is_some() => Parsed::Resume,
        _ if argument(text, "/diff").is_some() => Parsed::Diff,
        _ if argument(text, "/context").is_some() => Parsed::Context,
        _ if argument(text, "/init").is_some() => Parsed::Init,
        _ if command == "/mcps" || argument(text, "/mcp").is_some() => Parsed::Mcp,
        _ if argument(text, "/compact").is_some() => Parsed::Compact,
        _ => return None,
    })
}

/// Read-only commands preserve the reading window; prompts return to live history.
pub fn submits_prompt(text: &str) -> bool {
    text.trim() == "/init" || parse(text).is_none()
}

/// None means ordinary prompt text. Every slash command is handled here;
/// Client remains the only owner of submissions and uncertain retries.
pub fn execute<C: Connection>(client: &mut Client<C>, text: &str) -> Option<Completion> {
    dispatch(client, text, false)
}

/// Image prompts use the same command expansion, then return to Submissions
/// before any prompt effect so files and uncertain identity stay together.
pub fn prepare<C: Connection>(client: &mut Client<C>, text: &str) -> Option<Completion> {
    dispatch(client, text, true)
}

fn dispatch<C: Connection>(
    client: &mut Client<C>,
    text: &str,
    defer_prompt: bool,
) -> Option<Completion> {
    let normalized = crate::actions::canonical(text);
    let mut done =
        dispatch_normalized(client, normalized.as_deref().unwrap_or(text), defer_prompt)?;
    done.text = text.to_owned();
    Some(done)
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn dispatch_normalized<C: Connection>(
    client: &mut Client<C>,
    text: &str,
    defer_prompt: bool,
) -> Option<Completion> {
    // A doubled slash is an explicit literal prompt, independent of the catalog.
    let start = text.len() - text.trim_start().len();
    if text[start..].starts_with("//") {
        let mut literal = text.to_owned();
        literal.remove(start);
        let mut done = Completion::new(text);
        submit(client, &literal, &mut done, defer_prompt);
        // Keep the escape on refusal so Enter retries the same literal text.
        return Some(done);
    }
    if text[start..].starts_with('/') {
        client.enable_commands();
    }
    let Some(parsed) = parse(text) else {
        return custom(client, text, defer_prompt);
    };
    let command = text.trim();
    let mut done = Completion::new(text);
    match parsed {
        Parsed::Export => export::execute(
            client,
            argument(text, "/export").unwrap_or_default(),
            &mut done,
        ),
        Parsed::Commands => list_commands(client, &mut done),
        Parsed::Help => {
            done.view = Some(View::Text(help_lines()));
            done.consume = true;
        }
        Parsed::Status => {
            client.refresh_status();
            done.view = Some(View::Status);
            done.consume = true;
        }
        Parsed::Model => {
            selection::model(
                client,
                argument(text, "/model").unwrap_or_default(),
                &mut done,
            );
        }
        Parsed::Effort => {
            selection::effort(
                client,
                argument(text, "/effort").unwrap_or_default(),
                &mut done,
            );
        }
        Parsed::Permissions => {
            permissions::execute(
                client,
                argument(text, "/permissions").unwrap_or_default(),
                &mut done,
            );
        }
        Parsed::New => {
            if matches!(command, "/new" | "/clear") {
                done.finish(client.create_session().map(|_| ()));
                if done.consume {
                    done.notice =
                        "New session started with the selected agent, model and effort.".into();
                }
            } else {
                let name = if argument(text, "/clear").is_some() {
                    "/clear"
                } else {
                    "/new"
                };
                done.notice = format!(
                    "Use {name} to start a session with the selected agent, model and effort."
                );
            }
        }
        Parsed::Resume => {
            resume(
                client,
                argument(text, "/resume").unwrap_or_default(),
                &mut done,
            );
        }
        Parsed::Diff => {
            if command == "/diff" {
                client.refresh_workspace_diff();
                done.view = Some(View::Text(view::workspace_diff_lines(client, 120)));
                done.consume = true;
            } else {
                done.notice = "Use /diff to inspect the current workspace.".into();
            }
        }
        Parsed::Context => {
            if command != "/context" {
                done.notice =
                    "Use /context to inspect last-captured instructions; no disk refresh.".into();
            } else {
                match client.instruction_context() {
                    Ok(lines) => {
                        done.view = Some(View::Text(lines));
                        done.consume = true;
                    }
                    Err(error) => done.notice = error.to_string(),
                }
            }
        }
        Parsed::Init => {
            if command != "/init" {
                done.notice = "Use /init to create repository guidance without replacing an existing AGENTS.md.".into();
            } else {
                let prompt = INIT_PROMPT.trim_end();
                submit(client, prompt, &mut done, defer_prompt);
                if !done.consume {
                    done.replacement = Some(prompt.into());
                }
            }
        }
        Parsed::Plugin => {
            plugin::run(client, text, &mut done);
        }
        Parsed::Import => {
            done.notice = "Import is available through the native workspace import flow.".into();
        }
        Parsed::Mcp => {
            mcp(client, command, &mut done, defer_prompt);
        }
        Parsed::Compact => {
            let instructions = argument(text, "/compact").unwrap_or_default().trim();
            done.finish(
                client
                    .compact_with_instructions((!instructions.is_empty()).then_some(instructions)),
            );
            if done.consume {
                done.notice = grapt_client::composer_commands::COMPACT_NOTICE.into();
            }
        }
        Parsed::Rename => {
            let title = argument(text, "/rename").unwrap_or_default().trim();
            if title.is_empty() {
                done.notice = "Use /rename <session title>.".into();
            } else {
                done.finish(client.rename(title));
                if done.consume {
                    done.notice = match client.refresh_sessions() {
                        Ok(()) => "Session renamed.",
                        Err(_) => "Session renamed; picker refresh failed. Titles may be stale.",
                    }
                    .into();
                }
            }
        }
        Parsed::Undo => {
            let result = match command {
                "/undo" => client.preview_file_undo(),
                "/undo apply" => client.apply_file_undo().map(|receipt| receipt.message),
                _ => Err(grapt_client::ClientError::NotOffered {
                    what:
                        "Use /undo to review files, then /undo apply; conversation stays unchanged",
                }),
            };
            match result {
                Ok(message) => {
                    done.consume = true;
                    done.view = Some(View::Text(vec!["FILE UNDO".into(), message.clone()]));
                    done.notice = message;
                }
                Err(error) => done.notice = error.to_string(),
            }
        }
        Parsed::Side => {
            let question = argument(text, "/side").unwrap_or_default().trim();
            match client.begin_side_question(question) {
                Ok(Some(answer)) => {
                    done.consume = true;
                    done.view = Some(View::Text(side_answer(question, answer)));
                    done.notice =
                        "Side answer opened; close the pane to return to the conversation.".into();
                }
                Ok(None) => done.notice = "Asking side question… draft kept.".into(),
                Err(error) => done.notice = format!("Side question failed: {error}. Draft kept."),
            }
        }
        Parsed::Worktree => worktree::execute(
            client,
            argument(text, "/worktree").unwrap_or_default(),
            &mut done,
        ),
        Parsed::Sandbox => sandbox::execute(
            client,
            argument(text, "/sandbox").unwrap_or_default(),
            &mut done,
        ),
        Parsed::Fork => {
            let args = argument(text, "/fork").unwrap_or_default().trim();
            let prompts = if args.is_empty() {
                Some(None)
            } else {
                args.parse::<u64>()
                    .ok()
                    .filter(|n| (1..=200).contains(n))
                    .map(Some)
            };
            if let Some(prompts) = prompts {
                match client.fork_session(prompts) {
                    Ok(fork) => {
                        done.consume = true;
                        done.replacement = if fork.attachment_error.is_none() { fork.prompt } else { None };
                        done.notice = match fork.attachment_error {
                            Some(error) => format!("Fork {} created; reopen it from Sessions: {error}", grapt_client::wire::Spelled::spelled(&fork.session)),
                            None => "Fork opened; parent and files unchanged. Edit the draft or send a new prompt.".into(),
                        };
                    }
                    Err(error) => done.notice = format!("Fork could not be confirmed: {error}. Draft kept; check Sessions before retrying."),
                }
            } else {
                done.notice = "Use /fork or /fork <1–200 earlier prompts>.".into();
            }
        }
        Parsed::Rewind => {
            let value = argument(text, "/rewind").unwrap_or_default().trim();
            let prompts = if value.is_empty() {
                Some(1)
            } else {
                value.parse::<u64>().ok()
            };
            if let Some(prompts) = prompts.filter(|count| (1..=200).contains(count)) {
                match client.rewind(prompts) {
                    Ok(edit) => {
                        done.replacement = Some(edit.prompt);
                        done.notice = match edit.history {
                            grapt_client::client::RewindHistory::Refreshed =>
                                "Conversation rewound; files unchanged. Edit the reopened prompt and send it when ready.".into(),
                            grapt_client::client::RewindHistory::Recovering =>
                                "Conversation rewound; files unchanged. Earlier messages reload automatically; keep editing.".into(),
                            grapt_client::client::RewindHistory::Failed(reason) =>
                                format!("Conversation rewound; files unchanged. History refresh failed: {reason}. Reopen the session."),
                        };
                        done.view = Some(View::Notice(done.notice.clone()));
                    }
                    Err(error) => done.notice = error.to_string(),
                }
            } else {
                done.notice =
                    "Use /rewind or /rewind <1–200 earlier prompts>. Files stay unchanged.".into();
            }
        }
        Parsed::Reconcile => {
            match client.reconcile_effects() {
                Ok(count) => {
                    done.consume = true;
                    done.notice = match count {
                        0 => "no lost tool runs to reconcile".into(),
                        1 => "recorded 1 lost tool run as unknown; check its effect before continuing"
                            .into(),
                        count => format!(
                            "recorded {count} lost tool runs as unknown; check their effects before continuing"
                        ),
                    };
                }
                Err(error) => done.notice = error.to_string(),
            }
            // Reconnection can replace the footer status. Keep this recovery
            // outcome readable until the user explicitly dismisses it.
            done.view = Some(View::Text(vec![
                "LOST TOOL RECOVERY".into(),
                done.notice.clone(),
            ]));
        }
    }
    if !done.consume && done.view.is_none() && done.notice != "Connected" {
        done.view = Some(View::Notice(done.notice.clone()));
    }
    done.session = client.selected().cloned();
    Some(done)
}

/// Use the ordinary first-prompt creation owner, only after a command has a task.
fn submit<C: Connection>(
    client: &mut Client<C>,
    prompt: &str,
    done: &mut Completion,
    defer_prompt: bool,
) {
    if defer_prompt {
        done.prepared_prompt = Some(prompt.to_owned());
        return;
    }
    match client.create_draft_session() {
        Ok(()) if matches!(client.launch_preset_ready(), Ok(false)) => {
            done.prepared_prompt = Some(prompt.to_owned());
        }
        Ok(()) => done.finish(crate::launch::submit(client, prompt)),
        Err(error) => done.notice = error,
    }
    done.session = client.selected().cloned();
}

fn custom<C: Connection>(
    client: &mut Client<C>,
    text: &str,
    defer_prompt: bool,
) -> Option<Completion> {
    let command = text.trim().strip_prefix('/')?;
    let (name, args) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    let mut done = Completion::new(text);
    done.session = client.selected().cloned();
    // /review is advertised with built-ins, but its server catalog owns the task.
    if crate::slash::builtin(name) && name != "review" {
        done.notice = format!("Use /{name} with the arguments shown in /help; draft kept.");
        done.view = Some(View::Notice(done.notice.clone()));
        return Some(done);
    }
    let Some(row) = client
        .commands()
        .iter()
        .find(|row| crate::actions::command_name(row) == name)
    else {
        done.notice = match client.commands_status() {
            Some(note) => format!("{note} Draft kept; use /commands to reload."),
            None => {
                format!("Unknown command /{name}. Use //{command} to send /{command} literally.")
            }
        };
        done.view = Some(View::Notice(done.notice.clone()));
        return Some(done);
    };
    if let Some(reason) = &row.unavailable {
        done.notice = format!("/{name} unavailable: {reason} ({})", row.source);
        done.view = Some(View::Notice(done.notice.clone()));
        return Some(done);
    }
    if let Some(binding) = row.mcp.clone() {
        if defer_prompt {
            done.notice = mcp::STAGED_IMAGES.into();
            done.view = Some(View::Notice(done.notice.clone()));
        } else {
            mcp::prompt(client, &binding, args.trim(), &mut done);
        }
        return Some(done);
    }
    if row.unsupported {
        done.notice = "This command requires agent/model, subtask or shell-template execution unavailable in R; draft kept.".into();
        return Some(done);
    }
    match crate::slash::expand::expand_with_paths(&row.template, args.trim(), row.claude.as_ref()) {
        Ok(prompt) => {
            if row.origin == grapt_client::wire::CommandOrigin::Builtin {
                submit(client, &prompt, &mut done, defer_prompt);
            } else {
                // Stage these exact bytes in the existing editor. No Session or
                // provider input exists until the user closes the preview and
                // presses Enter again; catalog reloads cannot replace the task.
                let origin = match row.origin {
                    grapt_client::wire::CommandOrigin::Project => "Project",
                    _ => "User",
                };
                let mut lines = vec![format!("{origin} command /{name}"), row.source.clone(),
                    "Preview only. Esc returns to the staged draft; edit it, then Enter to send. Clear the draft to cancel.".into(), String::new()];
                lines.extend(prompt.lines().map(str::to_owned));
                if lines.len() > 8_000
                    || lines
                        .iter()
                        .map(|v| {
                            grapt_tui_transcript::markdown::literal(v)
                                .len()
                                .saturating_add(1)
                        })
                        .sum::<usize>()
                        > 60_000
                {
                    done.notice =
                        "Command expansion exceeds the complete preview bound; draft kept.".into();
                    done.view = Some(View::Notice(done.notice.clone()));
                    return Some(done);
                }
                done.view = Some(View::Text(lines));
                done.notice = "Command preview only; nothing submitted.".into();
            }
            if !done.consume {
                let mut editable = prompt;
                let start = editable.len() - editable.trim_start().len();
                if editable[start..].starts_with('/') {
                    editable.insert(start, '/');
                }
                done.replacement = Some(editable);
            }
        }
        Err(error) => done.notice = error,
    }
    Some(done)
}

/// `/commands`: the workspace commands and their status, then a reload unless
/// the service was refused.
fn list_commands<C: Connection>(client: &mut Client<C>, done: &mut Completion) {
    let mut lines = vec!["WORKSPACE COMMANDS".into()];
    lines.extend(client.commands().iter().map(|row| {
        if let Some(reason) = &row.unavailable {
            return format!("/{} unavailable: {} ({})", row.name, reason, row.source);
        }
        format!(
            "/{} [{}]  {}{}",
            crate::actions::command_name(row),
            match row.origin {
                grapt_client::wire::CommandOrigin::Builtin => "server-owned",
                grapt_client::wire::CommandOrigin::Mcp => "MCP prompt",
                _ => "workspace template",
            },
            row.description,
            if row.unsupported {
                " (specialized execution unavailable in R)"
            } else {
                ""
            }
        )
    }));
    if let Some(note) = client.commands_status() {
        lines.push(note.into());
    }
    if !client.commands_refused() {
        lines.push("Tab completes a command. Enter runs built-ins and MCP prompts; workspace templates open a preview.".into());
        lines.push("Workspace preview: Esc back to the staged draft, edit, then Enter to send. Reloading workspace commands…".into());
        client.refresh_commands();
    }
    done.consume = true;
    done.view = Some(View::Text(lines));
}

fn resume<C: Connection>(client: &mut Client<C>, target: &str, done: &mut Completion) {
    if let Ok(id) = grapt_client::wire::session_id(target) {
        done.finish(client.resume_session(&id));
    } else if let Err(error) = client.refresh_sessions() {
        done.notice = error.to_string();
    } else if target.is_empty() {
        done.view = Some(View::Sessions);
        done.consume = true;
    } else {
        let mut named = client.sessions().iter().filter(|row| row.title == target);
        let selected = named.next().map(|row| row.id.clone());
        if named.next().is_some() {
            done.notice = "More than one session has that title; use /resume <session id>.".into();
        } else if let Some(id) = selected {
            done.finish(client.resume_session(&id));
        } else {
            done.notice =
                "Session not found. Use /resume to choose from available sessions.".into();
        }
    }
    if done.consume && done.view.is_none() {
        if client.agent().is_none() {
            done.notice = "Session resumed; its saved agent is unavailable.".into();
        } else if client.model().is_none() {
            done.view = Some(View::Models);
            done.notice = "Session resumed; choose an available model and effort.".into();
        } else {
            done.notice = "Session resumed with its saved model and effort.".into();
        }
    }
}

fn mcp<C: Connection>(
    client: &mut Client<C>,
    command: &str,
    done: &mut Completion,
    defer_prompt: bool,
) {
    if mcp::content_command(client, command, done, defer_prompt) {
        return;
    }
    let legacy = command == "/mcps";
    let args = argument(command, "/mcp").unwrap_or_default();
    let verbose = args.eq_ignore_ascii_case("verbose");
    if let Some((
        action @ ("login" | "logout" | "enable" | "disable" | "reconnect" | "approve" | "revoke"),
        server,
    )) = args
        .split_once(char::is_whitespace)
        .map(|(action, server)| (action, server.trim()))
        .filter(|(_, server)| !server.is_empty())
    {
        let result = match action {
            "login" => client.begin_mcp_sign_in(server),
            "logout" => client.begin_mcp_sign_out(server),
            "enable" => client.begin_mcp_enabled(server, true),
            "disable" => client.begin_mcp_enabled(server, false),
            "approve" => client.begin_mcp_approve(server),
            "revoke" => client.begin_mcp_revoke(server),
            _ => client.begin_mcp_reconnect(server),
        };
        match result {
            Ok(Some(lines)) => {
                done.consume = true;
                done.view = Some(View::Text(lines));
            }
            Ok(None) => done.notice = "MCP operation in progress; draft kept.".into(),
            Err(error) => done.notice = error.to_string(),
        }
        return;
    }
    if !legacy && !args.is_empty() && !verbose {
        done.notice = "Use /mcp [verbose | resources <server> | read <server> <uri> | pin/unpin <server> <uri> | enable <server> | disable <server> | reconnect <server> | login <server> | logout <server> | approve <server> | revoke <server>].".into();
        return;
    }
    let result = if legacy {
        client.refresh_mcp()
    } else {
        client.refresh_mcp_inspection()
    };
    if result.is_ok() && client.mcp_catalog_pending() {
        done.notice =
            "MCP inspection in progress; startup questions remain answerable from the command palette; draft kept."
                .into();
        return;
    }
    done.consume = true;
    done.view = Some(View::Mcp {
        rows: if verbose {
            view::mcp_detail_lines(client, 120)
        } else {
            view::mcp_summary_lines(client, 120)
        },
        verbose,
    });
    if let Err(error) = result {
        done.notice = error.to_string();
    } else if !legacy && !verbose {
        done.notice =
            "Use /mcp resources <server> to browse; /mcp read <server> <uri> to add context, pin/unpin to watch updates; /help for server management."
                .into();
    }
}

pub fn help_lines() -> Vec<String> {
    help_lines_for(&crate::keybindings::Bindings::default())
}
pub fn help_lines_for(bindings: &crate::keybindings::Bindings) -> Vec<String> {
    let mut rows = crate::actions::help(bindings);
    rows.push(String::new());
    rows.extend(HELP.lines().map(str::to_owned));
    rows
}

const HELP: &str = "CORE COMMANDS
? — shortcuts from an empty draft
/help — commands and shortcuts
/status — session, model, account and usage
/model [provider/model [effort]] — choose a model and effort
/effort [level|default|status] — choose or inspect reasoning effort
/permissions [inspect|author|maintainer] — inspect or change session permissions
/sandbox worktree-config — preview missing Windows Git compatibility file; no writes
/sandbox worktree-config create confirm — consent to create only the exact previewed empty file
Sandbox preview expires on session change or any create attempt; opening/closing its pane is not consent.
/compact — compact before your next message
/reconcile — record lost tool runs as unknown; inspect their effects before continuing
/new or /clear — fresh session with selected choices; saved history stays
/resume [id or title] — resume saved choices; no argument opens sessions
/rename <title> — rename the selected session
/export [markdown|json] [path] — save complete selected history to a new file
/undo — review patch files; /undo apply restores the reviewed files
/rewind [n] — reopen an earlier prompt for editing; files stay unchanged
/fork [n] — branch here, or before an earlier prompt and edit its copy
/side <question> — ask from this conversation without saving a turn
/worktree create <destination> [--copy <JSON array>] — detached checkout; explicit setup copies only
/worktree merge <checkout> — committed fast-forward only into selected source checkout
/worktree remove <checkout> — refuse dirty/untracked/ignored/unmerged/current/unrelated; never force
/worktree open <absolute backend checkout path> — NEW session; use create's printed path, never move the current one
Worktree paths may contain spaces or be JSON quoted. Copies are untracked/ignored files relative to repo root.
Setup copying uses guarded Unix/Windows access; unsupported platforms permit plain creation only.
Worktree failures/cancellation can leave effects: inspect checkouts and Sessions before retrying; no automatic retries.
/quit — close the terminal
/diff — read the selected workspace's changes
/plugin marketplace add <path> | /plugin install <name@marketplace>
/import [claude|codex] — preview setup; /import apply imports the previewed sources
/context — inspect last-captured instructions, not a disk refresh
/connect — connect or manage providers (Ctrl+A)
/mcp [verbose | resources <server> | read <server> <uri> | pin/unpin <server> <uri> | enable/disable/reconnect/login/logout/approve/revoke <server>] — inspect or manage servers
/mcps — list MCP servers
/init — ask the agent to prepare AGENTS.md, preserving an existing file
/commands — list or reload workspace prompt commands
/review [commit|branch|pr] — review changes; defaults to uncommitted
/help — this reference
//text — send /text literally, without command dispatch

Enter sends / steers · Shift+Enter newline
Tab queues a plain follow-up during work; Alt+Up edits it; Alt+Delete cancels it
Up/Down recall from an empty draft · Ctrl+R searches prompt history
Wheel/PgUp/PgDn scroll transcript · Ctrl+Home first · End/Ctrl+End live tail
Ctrl+O expands/collapses a focused or latest visible diff file
Shift+Tab focuses visible tools/files · Tab cycles focused tools/files · Enter/Space toggles details
Shift+drag selects text; use the terminal's native copy action (not Ctrl+C)
F8 expands/collapses an automated prompt visible in the transcript
Alt+R checks/retries the original uncertain submission; edits stay
Esc clears tool focus or closes a picker/result; otherwise interrupts a response or opens sessions
Ctrl+C interrupts a response or clears the draft; press twice within 1.5s to exit (not copy)
Quitting stops the session's current response before closing the terminal";

#[cfg(test)]
mod tests;

pub(super) fn side_answer(question: &str, answer: String) -> Vec<String> {
    vec!["SIDE QUESTION · not saved".into(), question.into(), answer]
}
