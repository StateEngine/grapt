//! A deliberately small, closed command grammar.

pub mod ephemeral;
pub mod help;
pub mod import;
pub mod login;
pub mod mcp;
pub mod plugin;
mod session;
pub mod terminal;

use std::path::PathBuf;

use grapt_types::{MessageId, ProjectId, SessionId};
use thiserror::Error;

use crate::bounds::{MAX_STDIN_BYTES, MAX_STDIN_RECORD_BYTES};
use crate::compatibility::{self, Refusal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Text,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSelector {
    New,
    /// An exact Session, optionally bound to the project it must belong to.
    Id {
        session: SessionId,
        project: Option<ProjectId>,
    },
    ResumeLast {
        project: Option<ProjectId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionChoice {
    Discover {
        service_root: PathBuf,
    },
    Remote {
        endpoint: String,
        credential_file: PathBuf,
    },
    Embedded {
        product_binary: PathBuf,
        service_root: PathBuf,
    },
    /// No connection was named: the user's default profile, through its
    /// running service or one this command owns until it exits.
    DefaultProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptSource {
    Argument(String),
    Stdin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    /// Prints the service URL, or `stopped`.
    Status,
    /// Ensures the service and prints its URL.
    Start,
}

/// The permission presets a run may choose, strictest first, as the product
/// names them (`--inspect`, `--author`, `--maintainer`).
pub const PRESETS: [&str; 4] = ["inspect", "author", "maintainer", "bypass"];

fn choose_preset(selected: &mut Option<&'static str>, name: &'static str) {
    let rank = |name: &str| PRESETS.iter().position(|preset| *preset == name);
    if selected.is_none_or(|held| rank(name) < rank(held)) {
        *selected = Some(name);
    }
}

/// How a run answers the permission requests its turn raises, as the
/// TypeScript CLI's client-side auto-reply did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord)]
pub enum AutoReply {
    /// Leave every request to another client.
    #[default]
    Off,
    /// `--auto` / `--yolo`: approve each request once, except one that would
    /// remove a file from its path (delete or move), which is refused.
    Auto,
    /// `--dangerously-skip-permissions`: approve every request once.
    Everything,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    Mcp(mcp::McpCommand),
    Plugin(plugin::PluginCommand),
    Import(import::ImportCommand),
    Help,
    FocusedHelp(help::Topic),
    Version,
    SessionProvision(crate::seat::ProvisionCommand),
    Connections(login::ConnectionCommand),
    SessionRename {
        connection: Option<ConnectionChoice>,
        session: SessionId,
        title: String,
    },
    Service {
        action: ServiceAction,
        service_root: PathBuf,
        product_binary: Option<PathBuf>,
    },
    ServiceStatus {
        service_root: PathBuf,
        output: OutputMode,
    },
    Launch {
        service_root: PathBuf,
        product_binary: PathBuf,
        output: OutputMode,
    },
    Attach {
        connection: ConnectionChoice,
        selector: SessionSelector,
        output: OutputMode,
    },
    Run {
        connection: ConnectionChoice,
        selector: SessionSelector,
        prompt: PromptSource,
        /// Caller-owned identity for the initial prompt only.
        input_id: Option<MessageId>,
        images: Vec<PathBuf>,
        /// The run's Session title, when `--title` chose one.
        title: Option<String>,
        output: OutputMode,
        /// Also render reasoning in text mode (`--thinking`).
        reasoning: bool,
        reasoning_effort: Option<String>,
        selection: crate::StartupSelection,
        max_steps: Option<std::num::NonZeroU32>,
        /// Fork the selected Session and run in the child (`--fork`).
        fork: bool,
        /// Keep session and harness state in memory for this foreground run.
        ephemeral: bool,
        background: bool,
        resident: bool,
        worktree: Option<PathBuf>,
        /// The Session's permission preset, when a flag chose one.
        preset: Option<&'static str>,
        auto: AutoReply,
    },
    Acp {
        connection: ConnectionChoice,
    },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CommandError {
    #[error(
        "a command is required: status, launch, attach, run, service, session, mcp, import, or acp (start the native terminal with grapt or grapt code; see grapt --help)"
    )]
    MissingCommand,
    #[error("{0}")]
    Mcp(&'static str),
    #[error("{0}")]
    Plugin(&'static str),
    #[error("{0}")]
    Import(&'static str),
    #[error("{0}")]
    Login(&'static str),
    #[error("`{}` is not part of the Grapt CLI contract: {}", .0.surface, .0.reason)]
    Unsupported(Refusal),
    #[error("--format accepts default or json, not {0:?}")]
    Format(String),
    #[error("unknown command {0:?}")]
    UnknownCommand(String),
    #[error("unknown option {0:?}")]
    UnknownOption(String),
    #[error("option {0} requires a value")]
    MissingValue(&'static str),
    #[error("option {0} appeared more than once")]
    Duplicate(&'static str),
    #[error("exactly one connection mode must be selected")]
    ConnectionMode,
    #[error("{0}")]
    Selection(&'static str),
    #[error("--model and --agent require a new session; omit --session, --continue and --fork")]
    SelectionTarget,
    #[error("exactly one session selector must be selected")]
    SessionSelector,
    #[error("run requires exactly one prompt argument or --stdin")]
    PromptSource,
    #[error("title must be a nonempty printable line of at most 100 characters")]
    Title,
    #[error("usage: grapt session rename <id> <title> [--service <root> | --remote <https-url> --credential <file>]")]
    RenameArguments,
    #[error("--fork requires --continue or --session")]
    ForkTarget,
    #[error("--input-id requires an exact --session and cannot be combined with --fork")]
    InputIdTarget,
    #[error("--input-id must be a valid msg_ identity")]
    InvalidInputId,
    #[error(
        "--ephemeral requires a new foreground headless run with --embedded --service <existing-provider-profile> --product <binary>; no existing session, project, fork, background, resident or worktree"
    )]
    EphemeralTarget,
    #[error(
        "--worktree requires a new session on a local service (no --session, --continue, --fork or --remote)"
    )]
    WorktreeTarget,
    #[error(
        "--background requires a persistent service and cannot answer permissions with --auto or --dangerously-skip-permissions"
    )]
    BackgroundTarget,
    #[error("--max-steps requires an integer from 1 to 4294967295 and cannot be combined with --resident")]
    MaxSteps,
    #[error("--resident requires --jsonl and cannot be combined with --background")]
    ResidentOutput,
    #[error("session identity is invalid")]
    InvalidSession,
    #[error("project identity is invalid")]
    InvalidProject,
    #[error("argument {argument} is {found} bytes; maximum is {maximum}")]
    ArgumentTooLong {
        argument: &'static str,
        maximum: usize,
        found: usize,
    },
}

impl CliCommand {
    pub fn parse<I, S>(arguments: I) -> Result<Self, CommandError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut arguments = arguments.into_iter().map(Into::into);
        let command = arguments.next().ok_or(CommandError::MissingCommand)?;
        let rest = arguments.collect::<Vec<_>>();
        if let Some(topic) = help::parse(&command, &rest) {
            return Ok(CliCommand::FocusedHelp(topic));
        }
        match command.as_str() {
            "--help" | "-h" | "help" => Ok(CliCommand::Help),
            "--version" if rest.is_empty() => Ok(CliCommand::Version),
            "session" if rest.first().is_some_and(|value| value == "rename") => {
                session::rename(&rest[1..])
            }
            "session" => {
                crate::seat::ProvisionCommand::parse(&rest).map(CliCommand::SessionProvision)
            }
            "status" => parse_status(&rest),
            "launch" => parse_launch(&rest),
            "attach" => parse_attach(&rest),
            "run" => parse_run(&rest),
            "mcp"
                if rest.first().is_some_and(|value| {
                    matches!(
                        value.as_str(),
                        "list" | "add" | "remove" | "login" | "logout"
                    )
                }) =>
            {
                mcp::parse(&rest)
            }
            "mcp" if rest.len() == 1 && matches!(rest[0].as_str(), "--help" | "-h" | "help") => {
                Ok(CliCommand::Help)
            }
            "import" if rest.len() == 1 && matches!(rest[0].as_str(), "--help" | "-h" | "help") => {
                Ok(CliCommand::Help)
            }
            "import" => import::parse(&rest),
            "plugin"
                if rest.first().is_none_or(|value| {
                    matches!(
                        value.as_str(),
                        "marketplace" | "install" | "--help" | "-h" | "help"
                    )
                }) =>
            {
                plugin::parse(&rest)
            }
            "login" => login::parse(&rest),
            "logout" => login::operator("logout", &rest),
            "auth"
                if rest
                    .first()
                    .is_none_or(|value| value == "status" || value.starts_with('-')) =>
            {
                login::operator("auth", &rest)
            }
            "service" => parse_service(&rest),
            "acp" => parse_acp(&rest),
            other => Err(match compatibility::command(other, None) {
                Some(refusal) => CommandError::Unsupported(refusal),
                // The oracle's default command took flags and a directory.
                None if other.starts_with('-') => {
                    CommandError::Unsupported(compatibility::interactive())
                }
                None => CommandError::UnknownCommand(command),
            }),
        }
    }
}

#[derive(Default)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct Parsed {
    service_root: Option<PathBuf>,
    product_binary: Option<PathBuf>,
    remote: Option<String>,
    credential_file: Option<PathBuf>,
    embedded: bool,
    session: Option<SessionId>,
    resume: bool,
    new_session: bool,
    project: Option<ProjectId>,
    prompt: Option<String>,
    input_id: Option<MessageId>,
    images: Vec<PathBuf>,
    /// The Session title (`--title`, run only).
    title: Option<String>,
    stdin: bool,
    json: bool,
    jsonl: bool,
    /// oracle-style message words (`run` only).
    message: Vec<String>,
    format: Option<String>,
    thinking: bool,
    reasoning_effort: Option<String>,
    selection: crate::StartupSelection,
    max_steps: Option<std::num::NonZeroU32>,
    fork: bool,
    ephemeral: bool,
    background: bool,
    resident: bool,
    worktree: Option<PathBuf>,
    preset: Option<&'static str>,
    auto: AutoReply,
}

impl Parsed {
    /// Options only `run` admits.
    fn has_run_only(&self) -> bool {
        !self.message.is_empty()
            || self.input_id.is_some()
            || !self.images.is_empty()
            || self.format.is_some()
            || self.title.is_some()
            || self.thinking
            || self.reasoning_effort.is_some()
            || !self.selection.is_empty()
            || self.max_steps.is_some()
            || self.fork
            || self.ephemeral
            || self.background
            || self.resident
            || self.worktree.is_some()
            || self.preset.is_some()
            || self.auto != AutoReply::Off
    }

    /// Several preset flags resolve to the strictest, so a careless
    /// `--inspect --maintainer` never grants more than was asked for.
    fn choose_preset(&mut self, name: &'static str) {
        choose_preset(&mut self.preset, name);
    }
}

/// Parses options. `positional` admits message words and a `--` separator,
/// which only `run` takes.
#[expect(clippy::too_many_lines, reason = "legacy")]
fn parse_options(arguments: &[String], positional: bool) -> Result<Parsed, CommandError> {
    let mut parsed = Parsed::default();
    let mut index = 0;
    while index < arguments.len() {
        let option = arguments[index].as_str();
        match option {
            "--" if positional => {
                parsed
                    .message
                    .extend(arguments[index + 1..].iter().cloned());
                break;
            }
            "--service" => assign(
                &mut parsed.service_root,
                path_value(arguments, &mut index, "--service")?,
                "--service",
            )?,
            "--product" => assign(
                &mut parsed.product_binary,
                path_value(arguments, &mut index, "--product")?,
                "--product",
            )?,
            "--remote" => assign(
                &mut parsed.remote,
                string_value(arguments, &mut index, "--remote")?,
                "--remote",
            )?,
            "--credential" => assign(
                &mut parsed.credential_file,
                path_value(arguments, &mut index, "--credential")?,
                "--credential",
            )?,
            "--session" | "-s" => {
                let raw = string_value(arguments, &mut index, "--session")?;
                assign(
                    &mut parsed.session,
                    raw.parse().map_err(|_| CommandError::InvalidSession)?,
                    "--session",
                )?;
            }
            "--project" => {
                let raw = string_value(arguments, &mut index, "--project")?;
                assign(
                    &mut parsed.project,
                    ProjectId::new(raw).ok_or(CommandError::InvalidProject)?,
                    "--project",
                )?;
            }
            "--prompt" => assign(
                &mut parsed.prompt,
                string_value(arguments, &mut index, "--prompt")?,
                "--prompt",
            )?,
            "--input-id" => {
                let raw = string_value(arguments, &mut index, "--input-id")?;
                assign(
                    &mut parsed.input_id,
                    raw.parse().map_err(|_| CommandError::InvalidInputId)?,
                    "--input-id",
                )?;
            }
            "--image" | "-i" => {
                if parsed.images.len() == 4 {
                    return Err(CommandError::UnknownOption(
                        "at most four --image files".into(),
                    ));
                }
                parsed
                    .images
                    .push(path_value(arguments, &mut index, "--image")?);
            }
            "--title" => assign(
                &mut parsed.title,
                string_value(arguments, &mut index, "--title")?,
                "--title",
            )?,
            "--embedded" => set_flag(&mut parsed.embedded, "--embedded")?,
            "--ephemeral" => set_flag(&mut parsed.ephemeral, "--ephemeral")?,
            "--resume" | "--continue" | "-c" => set_flag(&mut parsed.resume, "--resume")?,
            "--format" => assign(
                &mut parsed.format,
                string_value(arguments, &mut index, "--format")?,
                "--format",
            )?,
            "--reasoning-effort" => assign(
                &mut parsed.reasoning_effort,
                string_value(arguments, &mut index, "--reasoning-effort")?,
                "--reasoning-effort",
            )?,
            "--model" | "-m" => parsed
                .selection
                .set_model(&string_value(arguments, &mut index, "--model")?)?,
            "--agent" => parsed
                .selection
                .set_agent(&string_value(arguments, &mut index, "--agent")?)?,
            "--max-steps" => {
                let value = string_value(arguments, &mut index, "--max-steps")?;
                let maximum = value
                    .parse::<std::num::NonZeroU32>()
                    .map_err(|_| CommandError::MaxSteps)?;
                assign(&mut parsed.max_steps, maximum, "--max-steps")?;
            }
            "--thinking" => set_flag(&mut parsed.thinking, "--thinking")?,
            "--fork" => set_flag(&mut parsed.fork, "--fork")?,
            "--background" => set_flag(&mut parsed.background, "--background")?,
            "--worktree" => assign(
                &mut parsed.worktree,
                path_value(arguments, &mut index, "--worktree")?,
                "--worktree",
            )?,
            "--inspect" => parsed.choose_preset("inspect"),
            "--author" => parsed.choose_preset("author"),
            "--maintainer" => parsed.choose_preset("maintainer"),
            "--dangerously-bypass-permissions" => parsed.choose_preset("bypass"),
            "--auto" | "--yolo" => parsed.auto = parsed.auto.max(AutoReply::Auto),
            "--dangerously-skip-permissions" => parsed.auto = AutoReply::Everything,
            "--new" => set_flag(&mut parsed.new_session, "--new")?,
            "--stdin" => set_flag(&mut parsed.stdin, "--stdin")?,
            "--json" => set_flag(&mut parsed.json, "--json")?,
            "--jsonl" => set_flag(&mut parsed.jsonl, "--jsonl")?,
            "--resident" => set_flag(&mut parsed.resident, "--resident")?,
            _ if positional && !option.starts_with('-') => parsed.message.push(option.to_owned()),
            _ => {
                return Err(match compatibility::flag(option) {
                    Some(refusal) => CommandError::Unsupported(refusal),
                    None => CommandError::UnknownOption(option.to_owned()),
                });
            }
        }
        index += 1;
    }
    Ok(parsed)
}

fn parse_status(arguments: &[String]) -> Result<CliCommand, CommandError> {
    let parsed = parse_options(arguments, false)?;
    let service_root = parsed
        .service_root
        .clone()
        .ok_or(CommandError::MissingValue("--service"))?;
    reject_non_output(&parsed, &["service"])?;
    Ok(CliCommand::ServiceStatus {
        service_root,
        output: output_mode(&parsed)?,
    })
}

fn parse_launch(arguments: &[String]) -> Result<CliCommand, CommandError> {
    let parsed = parse_options(arguments, false)?;
    let service_root = parsed
        .service_root
        .clone()
        .ok_or(CommandError::MissingValue("--service"))?;
    let product_binary = parsed
        .product_binary
        .clone()
        .ok_or(CommandError::MissingValue("--product"))?;
    reject_non_output(&parsed, &["service", "product"])?;
    Ok(CliCommand::Launch {
        service_root,
        product_binary,
        output: output_mode(&parsed)?,
    })
}

fn parse_attach(arguments: &[String]) -> Result<CliCommand, CommandError> {
    let parsed = parse_options(arguments, false)?;
    if parsed.prompt.is_some() || parsed.stdin || parsed.has_run_only() {
        return Err(CommandError::UnknownOption(
            "attach-only option set".to_owned(),
        ));
    }
    Ok(CliCommand::Attach {
        connection: connection(&parsed)?,
        selector: selector(&parsed)?,
        output: output_mode(&parsed)?,
    })
}

fn parse_run(arguments: &[String]) -> Result<CliCommand, CommandError> {
    let parsed = parse_options(arguments, true)?;
    if parsed.input_id.is_some() && (parsed.session.is_none() || parsed.fork) {
        return Err(CommandError::InputIdTarget);
    }
    ephemeral::validate(&parsed)?;
    if !parsed.selection.is_empty() && (parsed.session.is_some() || parsed.resume || parsed.fork) {
        return Err(CommandError::SelectionTarget);
    }
    if parsed.max_steps.is_some() && parsed.resident {
        return Err(CommandError::MaxSteps);
    }
    let words = message(&parsed.message);
    let prompt = match (parsed.prompt.as_ref(), parsed.stdin, words.as_ref()) {
        (Some(value), false, None) | (None, false, Some(value)) => {
            check_len("--prompt", value, MAX_STDIN_BYTES)?;
            PromptSource::Argument(value.clone())
        }
        (None, true, None) => PromptSource::Stdin,
        (None, false, None) if !parsed.images.is_empty() => PromptSource::Argument(String::new()),
        _ => return Err(CommandError::PromptSource),
    };
    // As the TypeScript `run` did (run.ts): a fork needs a Session to fork.
    if parsed.fork && parsed.session.is_none() && !parsed.resume {
        return Err(CommandError::ForkTarget);
    }
    if parsed.worktree.is_some()
        && (parsed.session.is_some() || parsed.resume || parsed.fork || parsed.remote.is_some())
    {
        return Err(CommandError::WorktreeTarget);
    }
    if parsed.background && (parsed.embedded || parsed.auto != AutoReply::Off) {
        return Err(CommandError::BackgroundTarget);
    }
    let output = run_output_mode(&parsed)?;
    if parsed.resident && (output != OutputMode::Jsonl || parsed.background) {
        return Err(CommandError::ResidentOutput);
    }
    let title = parsed.title.as_deref().map(validate_title).transpose()?;
    Ok(CliCommand::Run {
        connection: connection(&parsed)?,
        selector: run_selector(&parsed)?,
        prompt,
        input_id: parsed.input_id.clone(),
        images: parsed.images.clone(),
        title,
        output,
        reasoning: parsed.thinking,
        reasoning_effort: parsed.reasoning_effort.clone(),
        selection: parsed.selection.clone(),
        max_steps: parsed.max_steps,
        fork: parsed.fork,
        ephemeral: parsed.ephemeral,
        background: parsed.background,
        resident: parsed.resident,
        worktree: parsed.worktree.clone(),
        preset: parsed.preset,
        auto: parsed.auto,
    })
}

/// Joins message words the way the TypeScript `run` did: a word containing a
/// space is quoted, with its own quotes escaped.
fn message(words: &[String]) -> Option<String> {
    let joined = words
        .iter()
        .map(|word| {
            if word.contains(' ') {
                format!("\"{}\"", word.replace('"', "\\\""))
            } else {
                word.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then_some(joined)
}

/// A run with no selector starts a new Session, as the TypeScript `run` did.
fn run_selector(parsed: &Parsed) -> Result<SessionSelector, CommandError> {
    let selected = parsed.session.is_some() || parsed.resume || parsed.new_session;
    if selected {
        selector(parsed)
    } else if parsed.project.is_some() {
        Err(CommandError::SessionSelector)
    } else {
        Ok(SessionSelector::New)
    }
}

fn run_output_mode(parsed: &Parsed) -> Result<OutputMode, CommandError> {
    match parsed.format.as_deref() {
        None => output_mode(parsed),
        Some("default") if !parsed.json && !parsed.jsonl => Ok(OutputMode::Text),
        Some("default") => Err(CommandError::Duplicate("output mode")),
        Some("json") => Err(CommandError::Unsupported(compatibility::FORMAT_JSON)),
        Some(other) => Err(CommandError::Format(other.to_owned())),
    }
}

fn parse_service(arguments: &[String]) -> Result<CliCommand, CommandError> {
    let (action, rest) = arguments
        .split_first()
        .ok_or_else(|| CommandError::UnknownCommand("service".to_owned()))?;
    let action = match action.as_str() {
        "status" => ServiceAction::Status,
        "start" => ServiceAction::Start,
        other => {
            return Err(match compatibility::command("service", Some(other)) {
                Some(refusal) => CommandError::Unsupported(refusal),
                None => CommandError::UnknownCommand(format!("service {other}")),
            });
        }
    };
    let parsed = parse_options(rest, false)?;
    let service_root = parsed
        .service_root
        .clone()
        .ok_or(CommandError::MissingValue("--service"))?;
    let product_binary = match action {
        ServiceAction::Status => {
            reject_non_output(&parsed, &["service"])?;
            None
        }
        ServiceAction::Start => {
            reject_non_output(&parsed, &["service", "product"])?;
            Some(
                parsed
                    .product_binary
                    .clone()
                    .ok_or(CommandError::MissingValue("--product"))?,
            )
        }
    };
    if parsed.json || parsed.jsonl {
        return Err(CommandError::UnknownOption(
            "service renders its URL as text".to_owned(),
        ));
    }
    Ok(CliCommand::Service {
        action,
        service_root,
        product_binary,
    })
}

fn parse_acp(arguments: &[String]) -> Result<CliCommand, CommandError> {
    let parsed = parse_options(arguments, false)?;
    if parsed.session.is_some()
        || parsed.resume
        || parsed.new_session
        || parsed.project.is_some()
        || parsed.prompt.is_some()
        || parsed.stdin
        || parsed.json
        || parsed.jsonl
        || parsed.has_run_only()
    {
        return Err(CommandError::UnknownOption(
            "option not admitted by acp".to_owned(),
        ));
    }
    Ok(CliCommand::Acp {
        connection: connection(&parsed)?,
    })
}

fn connection(parsed: &Parsed) -> Result<ConnectionChoice, CommandError> {
    if parsed.embedded {
        if parsed.remote.is_some() || parsed.credential_file.is_some() {
            return Err(CommandError::ConnectionMode);
        }
        let service_root = parsed
            .service_root
            .clone()
            .ok_or(CommandError::MissingValue("--service"))?;
        let product_binary = parsed
            .product_binary
            .clone()
            .ok_or(CommandError::MissingValue("--product"))?;
        return Ok(ConnectionChoice::Embedded {
            product_binary,
            service_root,
        });
    }
    if let Some(endpoint) = &parsed.remote {
        if parsed.service_root.is_some() || parsed.product_binary.is_some() {
            return Err(CommandError::ConnectionMode);
        }
        check_len("--remote", endpoint, MAX_STDIN_RECORD_BYTES)?;
        let credential_file = parsed
            .credential_file
            .clone()
            .ok_or(CommandError::MissingValue("--credential"))?;
        return Ok(ConnectionChoice::Remote {
            endpoint: endpoint.clone(),
            credential_file,
        });
    }
    if parsed.credential_file.is_some() {
        return Err(CommandError::ConnectionMode);
    }
    if parsed.product_binary.is_some() {
        return Err(CommandError::ConnectionMode);
    }
    Ok(parsed
        .service_root
        .clone()
        .map_or(ConnectionChoice::DefaultProfile, |service_root| {
            ConnectionChoice::Discover { service_root }
        }))
}

fn selector(parsed: &Parsed) -> Result<SessionSelector, CommandError> {
    let count = usize::from(parsed.session.is_some())
        + usize::from(parsed.resume)
        + usize::from(parsed.new_session);
    if count != 1 || (parsed.project.is_some() && parsed.new_session) {
        return Err(CommandError::SessionSelector);
    }
    if let Some(session) = &parsed.session {
        return Ok(SessionSelector::Id {
            session: session.clone(),
            project: parsed.project.clone(),
        });
    }
    if parsed.resume {
        return Ok(SessionSelector::ResumeLast {
            project: parsed.project.clone(),
        });
    }
    Ok(SessionSelector::New)
}

fn output_mode(parsed: &Parsed) -> Result<OutputMode, CommandError> {
    match (parsed.json, parsed.jsonl) {
        (false, false) => Ok(OutputMode::Text),
        (true, false) => Ok(OutputMode::Json),
        (false, true) => Ok(OutputMode::Jsonl),
        (true, true) => Err(CommandError::Duplicate("output mode")),
    }
}

fn reject_non_output(parsed: &Parsed, admitted: &[&str]) -> Result<(), CommandError> {
    let service_ok = admitted.contains(&"service") || parsed.service_root.is_none();
    let product_ok = admitted.contains(&"product") || parsed.product_binary.is_none();
    if !service_ok
        || !product_ok
        || parsed.remote.is_some()
        || parsed.credential_file.is_some()
        || parsed.embedded
        || parsed.session.is_some()
        || parsed.resume
        || parsed.new_session
        || parsed.project.is_some()
        || parsed.prompt.is_some()
        || parsed.stdin
        || parsed.has_run_only()
    {
        return Err(CommandError::UnknownOption(
            "option not admitted by command".to_owned(),
        ));
    }
    Ok(())
}

fn string_value(
    arguments: &[String],
    index: &mut usize,
    option: &'static str,
) -> Result<String, CommandError> {
    *index = index.saturating_add(1);
    arguments
        .get(*index)
        .cloned()
        .ok_or(CommandError::MissingValue(option))
}

fn path_value(
    arguments: &[String],
    index: &mut usize,
    option: &'static str,
) -> Result<PathBuf, CommandError> {
    string_value(arguments, index, option).map(PathBuf::from)
}

fn set_flag(flag: &mut bool, name: &'static str) -> Result<(), CommandError> {
    if *flag {
        return Err(CommandError::Duplicate(name));
    }
    *flag = true;
    Ok(())
}

fn assign<T>(slot: &mut Option<T>, value: T, name: &'static str) -> Result<(), CommandError> {
    if slot.replace(value).is_some() {
        return Err(CommandError::Duplicate(name));
    }
    Ok(())
}

fn check_len(argument: &'static str, value: &str, maximum: usize) -> Result<(), CommandError> {
    if value.len() > maximum {
        return Err(CommandError::ArgumentTooLong {
            argument,
            maximum,
            found: value.len(),
        });
    }
    Ok(())
}

/// The title rules the Session store applies, checked before any service work:
/// trimmed, nonempty, no control characters, at most
/// [`crate::bounds::MAX_TITLE_CHARACTERS`] characters.
fn validate_title(raw: &str) -> Result<String, CommandError> {
    let title = raw.trim();
    let within_bound = title.chars().count() <= crate::bounds::MAX_TITLE_CHARACTERS;
    let printable = title.chars().all(|ch| !ch.is_control());
    if title.is_empty() || !within_bound || !printable {
        return Err(CommandError::Title);
    }
    Ok(title.to_owned())
}
