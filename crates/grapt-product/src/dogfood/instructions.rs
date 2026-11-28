//! Effective instruction inputs for the ordinary provider plan.
//! Override-first file selection.
// Copyright 2025 OpenAI. Modified by Grapt for bounded ambient and scoped discovery.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use grapt_inbox::Inbox;
use grapt_instruction::{InstructionService, InstructionValue, RefreshInput, SourceObservation};
use grapt_provider::{CodexInputItem, InputMessage, MessageRole};
use grapt_types::{ClockAuthority, EventId, IdAuthority, InstructionKey, SessionId};

const BASE: &str = "You are Grapt, a coding assistant working in the user's repository. \
Inspect the project before changing it.

Using tools:
- Find and read code with the dedicated tools rather than the shell: glob finds files by name, \
grep searches contents (regex=true for patterns), list shows one directory and read reads \
files with line numbers. Use shell for builds, tests, git and other programs.
- Read the part of a file you are about to change first. Use edit for one exact replacement, \
with enough surrounding text in old_text to make it unique; use the offered patch tool for several changes or files \
in one step; write to create a new file or to replace a whole file.
- Before grouped tool calls, always write a brief 1–2 sentence user-facing preamble as ordinary \
assistant text, then call the tools. \
On long tasks, share short progress updates when you learn something or change direction; \
do not narrate every call.
- Use a tool's optional description field for a concise action label when offered.
- When calls do not depend on each other, make them together in one response.
- When a tool refuses or fails, read its message and adjust; do not repeat the same call unchanged.

Doing the work:
- Keep working until the request is fully resolved before you end your turn. Finish every \
part it names, and follow each change through the code that depends on it: a new option or \
setting must reach the code that reads it.
- When the request asks for a file that does not exist yet, create it with write.
- Keep changes focused on the request and consistent with the surrounding code. \
Do not revert changes you did not make.
- After changing code, run the relevant tests or checks when the project has them, and fix what you broke.
- When existing checks do not exercise changed behavior, add a regression using the actual public API input shape.
- Respect permission decisions.
- Report what you changed and what you actually verified, and say plainly what you could not verify. \
Keep user-facing explanations concise.";
pub(super) const EPOCH_CONTEXT_BOUND: &str =
    "instruction epoch exceeds bounded request context; compaction required";
const MAX_FILES: usize = 64;
const MAX_FILE_BYTES: usize = 65_536;
const MAX_INSTRUCTION_BYTES: usize = 1_048_576;
/// Aggregate bound for scoped instructions attached to one read.
const MAX_SCOPED_BYTES: usize = 65_536;
/// The discovery order is the oracle's (most specific first), so the model is
/// told which scope wins rather than left to infer it from position.
macro_rules! precedence {
    () => {
        "AGENTS.md precedence: a file in a more specific directory overrides one in a parent directory, and project files override the global file. Instructions the user gives in this conversation override every AGENTS.md file. In each directory, select AGENTS.override.md first, then AGENTS.md, and use CLAUDE.md only when both are absent."
    };
}
const PRECEDENCE: &str = precedence!();
/// Scoped files can reach a request with no ambient file at all, so they carry
/// the same contract themselves.
pub(super) const SCOPED_PRECEDENCE: &str = concat!(
    precedence!(),
    " The scoped files below govern the file just read and are listed most specific first."
);

#[derive(Debug, Clone)]
pub(super) struct InstructionRoots {
    directory: PathBuf,
    project: PathBuf,
    config: PathBuf,
    /// The skills offered for this prompt, read once at the async boundary
    /// before planning. A catalog that cannot be read is carried as its
    /// reason, so guidance becomes Unavailable rather than silently empty.
    skills: Result<Vec<super::catalogs::SkillSummary>, String>,
    skill_warnings: Vec<String>,
    agents: String,
}

struct AmbientObservation {
    source: SourceObservation,
    failure: Option<String>,
}

mod imports;
mod rules;
pub(super) mod source;
use source::read_file;

impl InstructionRoots {
    /// The roots for a turn at `location`, which the Session resolved once
    /// and reuses (SC-REL-11): no turn spawns Git to find them.
    pub(super) async fn discover(
        location: &super::locations::ResolvedLocation,
        plugins: &Arc<super::plugins::PluginRuntime>,
        agent: &str,
    ) -> Result<Self, String> {
        let environment = super::catalog_config::CatalogEnvironment::read(plugins.service_root())?;
        let directory = PathBuf::from(&location.directory);
        let (skills, skill_warnings) =
            match super::catalogs::current_skills(plugins, &directory).await {
                Ok(catalog) => (
                    Ok(super::catalogs::offered(&catalog.entries)),
                    catalog.warnings,
                ),
                Err(error) => (Err(bounded_reason(&error)), Vec::new()),
            };
        let agents = super::agent_definitions::Catalog::read(plugins.service_root(), &directory)?
            .guidance(agent)?;
        Ok(Self {
            directory,
            project: PathBuf::from(&location.project_directory),
            agents,
            config: environment.config,
            skills,
            skill_warnings,
        })
    }

    /// `core/skill-guidance`: skill metadata only, so the model can choose one
    /// to load without every skill's content entering every request.
    fn skill_guidance(&self) -> Result<(SourceObservation, Option<String>), String> {
        let key = key("core/skill-guidance")?;
        Ok(match &self.skills {
            Ok(skills) if skills.is_empty() => (SourceObservation::Removed { key }, self.skill_warning()),
            Ok(skills) => (
                SourceObservation::Available {
                    key,
                    value: InstructionValue::Text(render_skills(skills)),
                },
                self.skill_warning(),
            ),
            Err(reason) => (
                SourceObservation::Unavailable { key },
                Some(format!(
                    "Instruction source core/skill-guidance is temporarily unavailable: {reason}. Continuing with its last known value."
                )),
            ),
        })
    }

    fn skill_warning(&self) -> Option<String> {
        (!self.skill_warnings.is_empty()).then(|| self.skill_warnings.join("\n"))
    }

    fn ambient(&self) -> Result<AmbientObservation, String> {
        let key = key("core/instructions")?;
        let sample = || -> Result<String, String> {
            // The global file is the user's own configuration and may be a
            // symlink into a dotfiles tree; project files must stay inside the
            // project they claim to govern.
            let mut paths = vec![(self.config.clone(), None)];
            if self.directory.starts_with(&self.project) {
                let project = std::fs::canonicalize(&self.project)
                    .map_err(|error| format!("project root cannot be resolved: {error}"))?;
                for ancestor in self.directory.ancestors() {
                    if paths.len() >= MAX_FILES {
                        return Err("instruction discovery exceeds 64 paths".to_owned());
                    }
                    let path = ancestor.to_owned();
                    if !paths.iter().any(|(known, _)| known == &path) {
                        paths.push((path, Some(project.clone())));
                    }
                    if ancestor == self.project {
                        break;
                    }
                }
            }
            let mut rendered = String::new();
            for (directory, scope) in paths {
                let (path, content) = read_instruction_file(&directory, scope.as_deref());
                let content = content?;
                if let Some(content) = content {
                    let heading = format!("Instructions from: {}\n", path.display());
                    if rendered.len() + heading.len() + content.len() + 2 > MAX_INSTRUCTION_BYTES {
                        return Err("ambient instructions exceed 1 MiB".to_owned());
                    }
                    if !rendered.is_empty() {
                        rendered.push_str("\n\n");
                    }
                    rendered.push_str(&heading);
                    rendered.push_str(&content);
                }
            }
            if !rendered.is_empty() {
                rendered = format!("{PRECEDENCE}\n\n{rendered}");
            }
            Ok(rendered)
        };
        Ok(match sample() {
            Ok(text) if text.is_empty() => AmbientObservation {
                source: SourceObservation::Removed { key },
                failure: None,
            },
            Ok(text) => AmbientObservation {
                source: SourceObservation::Available {
                    key,
                    value: InstructionValue::Text(text),
                },
                failure: None,
            },
            Err(error) => AmbientObservation {
                source: SourceObservation::Unavailable { key },
                failure: Some(bounded_reason(&error)),
            },
        })
    }
}

pub(super) struct EffectiveInstructions {
    pub(super) roots: InstructionRoots,
    pub(super) inbox: Arc<Mutex<Inbox>>,
    pub(super) session: SessionId,
    pub(super) ids: Arc<dyn IdAuthority + Send + Sync>,
    pub(super) clock: Arc<dyn ClockAuthority + Send + Sync>,
}

pub(super) struct PreparedInstructions {
    pub(super) frozen_tools: Option<Vec<grapt_provider::ToolDefinition>>,
    pub(super) current: Vec<CodexInputItem>,
    pub(super) initial: Vec<CodexInputItem>,
    pub(super) updates: Vec<grapt_instruction::InstructionUpdate>,
    pub(super) notices: Vec<CodexInputItem>,
    initial_session_index: Option<usize>,
    current_session_index: Option<usize>,
    subagent_context: Option<String>,
}
mod builtin_epoch;
pub(in crate::dogfood) use builtin_epoch::BuiltinEpoch;
mod epoch;
mod refresh_error;
pub(super) use refresh_error::RefreshError;
mod tool_epoch;
pub(super) use epoch::update as epoch_update;

impl EffectiveInstructions {
    #[cfg(test)]
    pub(super) fn refresh(&self) -> Result<PreparedInstructions, RefreshError> {
        self.refresh_with_tools(None)
    }

    #[cfg(test)]
    pub(super) fn refresh_with_tools(
        &self,
        tools: Option<(&[grapt_provider::ToolDefinition], bool)>,
    ) -> Result<PreparedInstructions, RefreshError> {
        self.refresh_with_catalog(tools, None)
    }

    /// Which built-in tools this Session advertises.
    pub(super) fn builtin_epoch(&self) -> Result<BuiltinEpoch, String> {
        let inbox = self
            .inbox
            .lock()
            .map_err(|_| "instruction owner lock poisoned")?;
        BuiltinEpoch::of(inbox.store(), &self.session)
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn refresh_with_catalog(
        &self,
        tools: Option<(&[grapt_provider::ToolDefinition], bool)>,
        mcp_guidance: Option<&str>,
    ) -> Result<PreparedInstructions, RefreshError> {
        // File I/O precedes the synchronous Session-owned journal transaction.
        let ambient = self.roots.ambient()?;
        let ambient_notice = ambient.failure.as_ref().map(|reason| {
            format!(
                "Instruction source core/instructions is temporarily unavailable: {reason}. Continuing with its last known value; repair the source and retry refresh."
            )
        });
        let (skill_guidance, skill_notice) = self.roots.skill_guidance()?;
        let mut inbox = self
            .inbox
            .lock()
            .map_err(|_| "instruction owner lock poisoned")?;
        let entries = InstructionService::entries(inbox.store(), &self.session)
            .map_err(|error| error.to_string())?;
        let entries_key = key("core/session-entries")?;
        let mut entry_text = String::new();
        for entry in entries {
            if entry_text.len() + entry.key.as_str().len() + entry.value.len() + 4
                > MAX_INSTRUCTION_BYTES
            {
                return Err("Session instructions exceed 1 MiB".into());
            }
            let mut value: serde_json::Value = serde_json::from_str(&entry.value)
                .map_err(|_| "Session instruction entry is not valid JSON")?;
            if entry.key.as_str() == "stateengine-identity" {
                if value.get("scope").and_then(serde_json::Value::as_str) != Some("incognito") {
                    entry_text.push_str(super::stateengine_resume::BOUND_RESUME_GUIDANCE);
                    entry_text.push('\n');
                }
                if let Some(value) = value.as_object_mut() {
                    // The bridge's old packet is a snapshot, not effective system
                    // policy. Fresh continuity is acquired by the request owner.
                    for key in ["assembled_model_input", "packet_id", "generated_at"] {
                        value.remove(key);
                    }
                }
            }
            let rendered = match value {
                serde_json::Value::String(text) => text,
                value => serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?,
            };
            if entry_text.len() + rendered.len() + entry.key.as_str().len() + 32
                > MAX_INSTRUCTION_BYTES
            {
                return Err("rendered Session instructions exceed 1 MiB".into());
            }
            entry_text.push_str("Session context ");
            entry_text.push_str(entry.key.as_str());
            entry_text.push_str(":\n");
            entry_text.push_str(&rendered);
            entry_text.push('\n');
        }
        let event: EventId = format!("evt_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|error| format!("instruction event identity: {error}"))?;
        let now = self.clock.now().as_millis();
        let date: Option<String> = inbox
            .store()
            .connection()
            .query_row("SELECT date(?1 / 1000, 'unixepoch')", [now], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        let date = date.ok_or("injected clock is outside the supported calendar")?;
        // Only the Session ID differs between two fresh Sessions of one
        // project. It is its own source, session/id, which sorts after every
        // core/* source (base, date, environment, project instructions,
        // Session entries, skill guidance), so everything before it is a
        // prefix the provider can reuse across Sessions; nothing reads it
        // from here.
        let environment = format!(
            "Working directory: {}\nWorkspace root: {}\nPlatform: {}",
            self.roots.directory.display(),
            self.roots.project.display(),
            std::env::consts::OS
        );
        let session_line = format!("Session ID: {}", self.session.as_str());
        let builtin_epoch = BuiltinEpoch::of(inbox.store(), &self.session)?;
        InstructionService::refresh(
            inbox.store_mut(),
            RefreshInput {
                event,
                session: self.session.clone(),
                created_ms: now,
                required: vec![
                    key("core/base")?,
                    key("core/environment")?,
                    key("core/date")?,
                    key("core/instructions")?,
                    key("session/id")?,
                    // core/skill-guidance is observed but not required: a
                    // skill catalog that cannot be read must not stop a new
                    // Session from starting, only withhold its guidance.
                    entries_key.clone(),
                ],
                observations: {
                    let mut observations = vec![
                        builtin_epoch.base()?,
                        SourceObservation::Available {
                            key: key("core/environment")?,
                            value: InstructionValue::Text(environment),
                        },
                        SourceObservation::Available {
                            key: key("session/id")?,
                            value: InstructionValue::Text(session_line),
                        },
                        SourceObservation::Available {
                            key: key("core/date")?,
                            value: InstructionValue::Text(format!("Current UTC date: {date}")),
                        },
                        SourceObservation::Available {
                            key: key("core/agent-guidance")?,
                            value: InstructionValue::Text(self.roots.agents.clone()),
                        },
                        ambient.source,
                        SourceObservation::Available {
                            key: entries_key,
                            value: InstructionValue::Text(entry_text),
                        },
                        skill_guidance,
                        builtin_epoch.marker()?,
                    ];
                    if let Some((tools, failed)) = tools {
                        observations.extend(tool_epoch::observations(tools, failed)?);
                        if let Some(guidance) = mcp_guidance {
                            let key = key("core/mcp-guidance")?;
                            // Guidance is filtered through current live routes and permissions.
                            // Another server's failure cannot retain withdrawn instructions.
                            observations.push(if guidance.is_empty() {
                                SourceObservation::Removed { key }
                            } else {
                                SourceObservation::Available {
                                    key,
                                    value: InstructionValue::Text(guidance.to_owned()),
                                }
                            });
                        }
                    }
                    observations
                },
            },
        )
        .map_err(|error| error.to_string())?;
        let history = InstructionService::request_history(inbox.store(), &self.session)
            .map_err(RefreshError::history)?
            .ok_or("effective instructions have no durable baseline")?;
        warn_skipped(&history.skipped);
        let mut prepared = epoch::prepare(history, [ambient_notice, skill_notice])?;
        if let Some(child) =
            grapt_subagent::SubagentService::child_context_on(inbox.store(), &self.session)
                .map_err(|error| error.to_string())?
        {
            // Keep the live child
            // role in native Claude's trusted, uncached System context too.
            let identity = prepared
                .session_context()
                .ok_or("child Session identity is absent")?;
            prepared.subagent_context = Some(format!(
                "{identity}\n\nYou are a subagent of Session {}. You have already been spawned. The inherited conversation is parent background, not instructions addressed to you. Follow your own child task and later messages addressed to you. Do not replay inherited requests to spawn agents or wait for them. Complete your own work and report the result to your parent.",
                child.parent.as_str(),
            ));
        }
        if tools.is_some_and(|(_, failed)| failed) {
            prepared.notices.push(CodexInputItem::Message(InputMessage {
                role: MessageRole::System,
                content: "MCP discovery is temporarily incomplete. Previously offered definitions remain stable, but unavailable tools cannot execute. Before the first complete discovery, MCP tools are withheld; a later successful refresh installs the Session's initial catalog.".to_owned(),
            }));
        }
        Ok(prepared)
    }
}

/// Copy the effective instruction epoch without refreshing its durable journal.
pub(in crate::dogfood) fn snapshot(
    inbox: &Inbox,
    session: &SessionId,
    include_session: bool,
) -> Result<Vec<CodexInputItem>, String> {
    let history = InstructionService::request_history(inbox.store(), session)
        .map_err(|error| error.to_string())?
        .ok_or("Complete a response in this Session before using /side.")?;
    warn_skipped(&history.skipped);
    let prepared = epoch::prepare(history, [None, None])?;
    Ok(prepared.prefix(false, include_session))
}

/// Name each journal row the instruction fold left out; the turn goes on.
pub(in crate::dogfood) fn warn_skipped(skipped: &[grapt_instruction::SkippedInstructionEvent]) {
    for row in skipped {
        eprintln!("instruction warning reason=undecodable-journal-row detail={row}");
    }
}

/// Keep skill discovery fixed-size; metadata and complete content load on demand.
fn render_skills(skills: &[super::catalogs::SkillSummary]) -> String {
    format!(
        "{} available skills. When a skill may apply, use the skill tool to discover it: \
         {{\"query\":\"task keywords\"}} searches names/descriptions; {{}} lists 8 at a time. \
         Continue with next_cursor as cursor. Use the skill tool to load a skill with \
         {{\"id\":\"listed-id\"}}, then add file for a relative resource. \
         Discovery returns metadata only; loading returns full instructions.",
        skills.len()
    )
}

fn bounded_reason(reason: &str) -> String {
    const MAX_REASON_BYTES: usize = 512;
    let mut safe = String::with_capacity(reason.len().min(MAX_REASON_BYTES));
    for character in reason.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if safe.len().saturating_add(character.len_utf8()) > MAX_REASON_BYTES {
            break;
        }
        safe.push(character);
    }
    if safe.trim().is_empty() {
        "source read failed".to_owned()
    } else {
        safe
    }
}

fn key(value: &str) -> Result<InstructionKey, String> {
    InstructionKey::new(value).ok_or_else(|| "instruction source key invalid".to_owned())
}

/// One directory instruction file or matching Markdown rule for a file read.
pub(super) struct ScopedInstruction {
    /// Path relative to the Session directory, `/`-separated.
    pub(super) path: String,
    pub(super) content: Result<String, String>,
}

/// The scoped instructions for one read and whether their walk was complete.
#[derive(Default)]
pub(super) struct ScopedRead {
    pub(super) instructions: Vec<ScopedInstruction>,
    /// The walk stopped at its bound short of the Session directory: the first
    /// directory, relative and `/`-separated, whose instructions were not checked.
    pub(super) unchecked: Option<String>,
}

impl ScopedRead {
    pub(super) fn is_empty(&self) -> bool {
        self.instructions.is_empty() && self.unchecked.is_none()
    }

    /// An unchecked walk is not an absence of instructions; say so.
    pub(super) fn incomplete(&self) -> Option<String> {
        self.unchecked.as_ref().map(|directory| {
            format!(
                "scoped instruction discovery stops after {MAX_FILES} descendant directories, so {directory} and the directories above it up to the session directory were not checked; read their instruction files and .grapt/rules directly before relying on this file's scope"
            )
        })
    }

    /// The `read` rendering: contract, files most specific first, then any bound.
    pub(super) fn render_text(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut rendered = format!("\n\n[{SCOPED_PRECEDENCE}]");
        for scoped in &self.instructions {
            rendered.push_str(&match &scoped.content {
                Ok(content) => format!("\n\n[Instructions from: {}]\n{content}", scoped.path),
                Err(reason) => format!(
                    "\n\n[Instructions from: {} are unavailable: {reason}]",
                    scoped.path
                ),
            });
        }
        if let Some(incomplete) = self.incomplete() {
            rendered.push_str(&format!(
                "\n\n[Scoped instructions incomplete: {incomplete}]"
            ));
        }
        rendered
    }
}

/// AGENTS.md files in the directories between the Session directory
/// (exclusive; it and its ancestors are already ambient) and `file`'s directory,
/// most specific first. They govern only their own subtree, so they reach the
/// model on the read that entered it rather than as Session-wide instructions.
/// Absent files are skipped; present-but-unusable files are reported, not dropped.
/// At most [`MAX_FILES`] directories are checked; a deeper file gets an honest
/// `unchecked` marker rather than a silently shortened scope.
/// Matching `.grapt/rules/*.md` files use the same walk, plus the Session root;
/// unlike its selected ambient instruction file, its rules are read-scoped.
pub(super) fn scoped_for_read(root: &Path, file: &Path) -> ScopedRead {
    let mut found = ScopedRead::default();
    let mut bytes = 0usize;
    let Some(parent) = file.parent() else {
        return found;
    };
    if !parent.starts_with(root) {
        return found;
    }
    let scope = match std::fs::canonicalize(root) {
        Ok(scope) => scope,
        Err(error) => {
            found.instructions.push(ScopedInstruction {
                path: ".".to_owned(),
                content: Err(bounded_reason(&format!(
                    "instruction scope cannot be resolved: {error}"
                ))),
            });
            return found;
        }
    };
    let mut rules = rules::Discovery::new(scope.clone());
    for (checked, directory) in parent.ancestors().enumerate() {
        if !directory.starts_with(root) {
            break;
        }
        if directory == root {
            rules.append(root, directory, file, &mut found, &mut bytes);
            break;
        }
        let relative = |path: &Path| {
            path.strip_prefix(root)
                .ok()
                .and_then(Path::to_str)
                .map(|relative| relative.replace('\\', "/"))
        };
        if checked == MAX_FILES {
            found.unchecked = Some(relative(directory).unwrap_or_else(|| "a parent".to_owned()));
            break;
        }
        rules.append(root, directory, file, &mut found, &mut bytes);
        let (path, content) = read_instruction_file(directory, Some(&scope));
        let Some(relative) = relative(&path) else {
            continue;
        };
        let content = match content {
            Ok(None) => continue,
            Ok(Some(content)) => Ok(content),
            Err(error) => Err(error),
        }
        .and_then(|content| {
            bytes = bytes.saturating_add(content.len());
            if bytes > MAX_SCOPED_BYTES {
                Err("scoped instructions for this read exceed 64 KiB".to_owned())
            } else {
                Ok(content)
            }
        })
        .map_err(|reason| bounded_reason(&reason));
        found.instructions.push(ScopedInstruction {
            path: relative,
            content,
        });
    }
    found
}

/// `path` resolves, through any links, to a location under `scope`.
fn contained(path: &Path, scope: &Path) -> Result<(), String> {
    let target = std::fs::canonicalize(path)
        .map_err(|error| format!("instruction source cannot be resolved: {error}"))?;
    if target.starts_with(scope) {
        Ok(())
    } else {
        Err(format!(
            "instruction source {} resolves outside the directory it governs",
            path.display()
        ))
    }
}

/// Ordered filename candidates, with CLAUDE.md as the final fallback.
/// A present but empty, unreadable or unresolved source still owns its scope.
fn read_instruction_file(
    directory: &Path,
    scope: Option<&Path>,
) -> (PathBuf, Result<Option<String>, String>) {
    for name in ["AGENTS.override.md", "AGENTS.md", "CLAUDE.md"] {
        let path = directory.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let content = read_file(&path, scope).and_then(|content| {
                    let source = content.ok_or_else(|| {
                        "instruction source is present but cannot be resolved".to_owned()
                    })?;
                    imports::expand(source).map(Some)
                });
                return (path, content);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return (
                    path,
                    Err(format!(
                        "instruction source metadata is unreadable: {error}"
                    )),
                );
            }
        }
    }
    (directory.join("CLAUDE.md"), Ok(None))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod sustained_tests;
