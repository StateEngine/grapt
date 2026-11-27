//! Named specialist definitions.
//! Apache-2.0, Copyright 2025 OpenAI.
//! Modified: bounded Grapt config roots, existing model routing and narrow-only tools.
mod claude;
pub(in crate::dogfood) mod claude_plugin;
mod configured;
mod files;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use super::catalog_config::{CatalogEnvironment, CatalogSources, CommandSource};

const MAX_AGENTS: usize = 64;
pub(super) const PLAN_AGENT: &str = "plan";

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(super) enum Mode {
    Primary,
    Subagent,
    #[default]
    All,
}

impl Mode {
    pub(super) fn permits(self, child: bool) -> bool {
        self == Self::All
            || if child {
                self == Self::Subagent
            } else {
                self == Self::Primary
            }
    }

    fn narrow(self, requested: Self, name: &str) -> Result<Self, String> {
        match (self, requested) {
            (Self::All, mode) | (mode, Self::All) => Ok(mode),
            (left, right) if left == right => Ok(left),
            (previous, requested) => Err(format!(
                "agent {name} mode {} conflicts with inherited {}; set its mode to {} or all in this source",
                requested.spell(), previous.spell(), previous.spell()
            )),
        }
    }

    fn spell(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Subagent => "subagent",
            Self::All => "all",
        }
    }
}

/// Which admitted definition won a same-name conflict, for the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Builtin,
    User,
    Project,
    ClaudeUser,
    ClaudeProject,
    ClaudePlugin,
}

impl Source {
    pub(super) fn spell(self) -> &'static str {
        match self {
            Source::Builtin => "builtin",
            Source::User => "user",
            Source::Project => "project",
            Source::ClaudeUser => "claude-user",
            Source::ClaudeProject => "claude-project",
            Source::ClaudePlugin => "claude-plugin",
        }
    }
}

/// The provider/model shape every admitted model reference shares.
pub(super) fn provider_model(model: &str) -> Option<(String, String)> {
    let (provider, id) = model.split_once('/')?;
    if provider.is_empty()
        || id.is_empty()
        || model.len() > 512
        || model.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some((provider.to_owned(), id.to_owned()))
}

#[derive(Clone, Debug)]
pub(super) struct Definition {
    pub name: String,
    pub description: String,
    pub developer_instructions: String,
    model: Option<String>,
    pub tools: Option<BTreeSet<String>>,
    color: Option<String>,
    pub mode: Mode,
    pub steps: Option<std::num::NonZeroU32>,
    pub disabled: bool,
    hidden: bool,
}

impl Definition {
    fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.name)
            || super::selection::composed_agent_ids().any(|id| id == self.name)
        {
            return Err("agent name must use a bounded role name or plugin:role and must not replace a composed identity".into());
        }
        if self.description.trim().is_empty() || self.description.len() > 1024 {
            return Err("agent description must contain text and fit within 1024 bytes".into());
        }
        if self.developer_instructions.trim().is_empty()
            || self.developer_instructions.len() > 48 * 1024
        {
            return Err(
                "agent developer_instructions must contain text and fit within 48 KiB".into(),
            );
        }
        let _ = self.model()?;
        if self.color.as_deref().is_some_and(|color| {
            ![
                "red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan",
            ]
            .contains(&color)
        }) {
            return Err("agent color must name a supported display color".into());
        }
        if let Some(tools) = &self.tools {
            if tools.len() > 256
                || tools.iter().any(|tool| {
                    tool.is_empty()
                        || tool.len() > 256
                        || !tool
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_-.".contains(&c))
                })
            {
                return Err("agent tools must be at most 256 exact tool names".into());
            }
        }
        Ok(())
    }

    pub fn model(&self) -> Result<Option<Value>, String> {
        self.model
            .as_deref()
            .map(|model| {
                let (provider, id) =
                    provider_model(model).ok_or("agent model must be provider/model")?;
                Ok(json!({"providerID":provider,"id":id}))
            })
            .transpose()
    }
}

pub(super) fn valid_name(name: &str) -> bool {
    if let Some((plugin, role)) = name.split_once(':') {
        name.len() <= 512
            && grapt_plugin::claude::Name::new(plugin).is_ok()
            && role
                .split(':')
                .all(|segment| grapt_plugin::claude::Name::new(segment).is_ok())
    } else {
        plain_name(name)
    }
}
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
}

/// The row's visible text: the description plus its diagnostic notes, at
/// most 1024 bytes. Diagnostic space is reserved up front: a long
/// description yields with a visible marker, and no note disappears
/// silently.
fn bounded_row_text(description: &str, notes: Option<&[String]>) -> String {
    const BOUND: usize = 1024;
    const MARKER: &str = "…";
    let notes = notes.unwrap_or_default();
    if notes.is_empty() {
        return truncate_chars(description, BOUND).to_owned();
    }
    let mut full = description.to_owned();
    for note in notes {
        full.push_str("; ");
        full.push_str(note);
    }
    if full.len() <= BOUND {
        return full;
    }
    // The diagnostics keep their text; the description yields, and the
    // marker says where it stopped.
    let tail_len: usize = notes.iter().map(|note| note.len() + 2).sum();
    if tail_len + MARKER.len() >= BOUND {
        // The diagnostics alone fill the row: keep as much of them as fits.
        let budget = BOUND - MARKER.len();
        let mut kept = String::new();
        for note in notes {
            if kept.is_empty() {
                kept.push_str(truncate_chars(note, budget));
            } else if kept.len() + 2 + note.len() <= budget {
                kept.push_str("; ");
                kept.push_str(note);
            } else {
                break;
            }
        }
        kept.push_str(MARKER);
        return kept;
    }
    let description_budget = BOUND - tail_len - MARKER.len();
    let mut kept = truncate_chars(description, description_budget).to_owned();
    kept.push_str(MARKER);
    for note in notes {
        kept.push_str("; ");
        kept.push_str(note);
    }
    kept
}

/// A prefix that never splits a character.
fn truncate_chars(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[derive(Debug)]
pub(super) struct Catalog {
    pub definitions: BTreeMap<String, Definition>,
    /// Directories are protected too: adding a higher-priority file can change policy.
    pub roots: Vec<PathBuf>,
    /// The winning layer per name, so the TUI can say which one loaded.
    sources: BTreeMap<String, Source>,
    /// One report per name: unknown tools and other dropped frontmatter.
    notes: BTreeMap<String, Vec<String>>,
}

impl Catalog {
    pub fn read(service: &Path, directory: &Path) -> Result<Self, String> {
        // Roster and selection must resolve the same built-ins without a home.
        // Prompt execution still requires its separately read catalog sources.
        let Some(environment) = CatalogEnvironment::configured(service)? else {
            return Self::base(
                Vec::new(),
                &BTreeMap::new(),
                Vec::new(),
                &mut files::Capture::default(),
            );
        };
        let sources = CatalogSources::read(directory, environment)?;
        Self::from_sources(&sources)
    }

    pub fn from_sources(sources: &CatalogSources) -> Result<Self, String> {
        super::permission_preset::validate_agent_scopes(&sources.documents)?;
        let aliases = claude::model_aliases(sources)?;
        let plugins = super::claude_plugins::agents::load(sources, &aliases)?;
        let mut capture = files::Capture::default();
        let mut catalog = Self::base(claude::roots(sources)?, &aliases, plugins, &mut capture)?;
        configured::load(&mut catalog, sources, &mut capture)?;
        catalog.roots.extend(capture.aliases);
        Ok(catalog)
    }

    fn base(
        claude_roots: Vec<PathBuf>,
        aliases: &BTreeMap<String, String>,
        plugins: Vec<super::claude_plugins::agents::Layer>,
        capture: &mut files::Capture,
    ) -> Result<Self, String> {
        let mut definitions: BTreeMap<String, Definition> = BTreeMap::new();
        let mut sources: BTreeMap<String, Source> = BTreeMap::new();
        let mut notes: BTreeMap<String, Vec<String>> = BTreeMap::new();
        // The Claude markdown layer sits below everything: our built-ins and
        // the user's and project's own definitions always win a same-name
        // conflict, and the TUI can say which one loaded.
        for (index, root) in claude_roots.iter().enumerate() {
            let mut layer = BTreeMap::new();
            for path in files::collect(
                root,
                &mut capture.entries,
                Some("md"),
                index == 0,
                &mut capture.aliases,
            )? {
                let (definition, agent_notes) = claude::read(&path, &mut capture.bytes, aliases)?;
                if layer
                    .insert(definition.name.clone(), (definition, agent_notes))
                    .is_some()
                {
                    return Err(format!("duplicate agent name in {}", root.display()));
                }
            }
            Self::install(
                &mut definitions,
                &mut sources,
                &mut notes,
                layer,
                if index == 0 {
                    Source::ClaudeUser
                } else {
                    Source::ClaudeProject
                },
                index > 0,
            )
            .map_err(|reason| format!("{}: {reason}", root.display()))?;
            if definitions.len() > MAX_AGENTS {
                return Err("agent catalog exceeds 64 definitions".into());
            }
        }
        let mut plugin_roots = Vec::new();
        for plugin in plugins {
            Self::install(
                &mut definitions,
                &mut sources,
                &mut notes,
                plugin.definitions,
                Source::ClaudePlugin,
                false,
            )?;
            plugin_roots.push(plugin.root);
            if definitions.len() > MAX_AGENTS {
                return Err("agent catalog exceeds 64 definitions".into());
            }
        }
        for (name, definition) in builtins() {
            sources.insert(name.clone(), Source::Builtin);
            notes.remove(&name);
            definitions.insert(name, definition);
        }
        // The Claude roots are definition directories too: a file added to
        // one changes what loads, so the permission owner protects them.
        let mut all_roots = claude_roots;
        all_roots.extend(plugin_roots);
        Ok(Self {
            definitions,
            roots: all_roots,
            sources,
            notes,
        })
    }

    /// One layer's winners, in layer order: the ceiling narrows, the source
    /// follows the winner and its notes replace any lower layer's.
    fn install(
        definitions: &mut BTreeMap<String, Definition>,
        sources: &mut BTreeMap<String, Source>,
        notes: &mut BTreeMap<String, Vec<String>>,
        layer: BTreeMap<String, (Definition, Vec<String>)>,
        source: Source,
        narrow: bool,
    ) -> Result<(), String> {
        for (name, (mut definition, agent_notes)) in layer {
            if narrow {
                if let Some(previous) = definitions.get(&name) {
                    definition.disabled |= previous.disabled;
                    definition.mode = previous.mode.narrow(definition.mode, &name)?;
                    definition.steps = match (previous.steps, definition.steps) {
                        (Some(left), Some(right)) => Some(left.min(right)),
                        (Some(limit), None) => Some(limit),
                        (None, limit) => limit,
                    };
                }
                if let Some(ceiling) = definitions
                    .get(&name)
                    .and_then(|agent| agent.tools.as_ref())
                {
                    definition.tools = Some(match definition.tools {
                        Some(tools) => tools.intersection(ceiling).cloned().collect(),
                        None => ceiling.clone(),
                    });
                }
            }
            if agent_notes.is_empty() {
                notes.remove(&name);
            } else {
                notes.insert(name.clone(), agent_notes);
            }
            sources.insert(name.clone(), source);
            definitions.insert(name, definition);
        }
        Ok(())
    }

    pub fn selected(&self, name: &str) -> Result<Option<&Definition>, String> {
        if super::selection::composed_agent_ids().any(|id| id == name) {
            return Ok(None);
        }
        match self.definitions.get(name) {
            Some(agent) if agent.disabled => Err(format!(
                "Agent {name} is disabled; choose an enabled agent from the catalog"
            )),
            Some(agent) => Ok(Some(agent)),
            None => Err(format!(
                "Unknown agent: {name}; choose a named agent from the agent catalog"
            )),
        }
    }

    pub fn selected_for(&self, name: &str, child: bool) -> Result<Option<&Definition>, String> {
        let definition = self.selected(name)?;
        if definition.is_some_and(|definition| !definition.mode.permits(child)) {
            return Err(format!(
                "Agent {name} is not available for {}; choose an agent with a matching mode",
                if child {
                    "subagents"
                } else {
                    "primary sessions"
                }
            ));
        }
        Ok(definition)
    }

    pub fn guidance(&self, selected: &str) -> Result<String, String> {
        let chosen = self.selected(selected)?;
        let mut text = if chosen.is_some_and(|agent| agent.mode == Mode::Primary) {
            "Named agents for spawn_agent.agent_type (this agent is primary-only; select a listed subagent explicitly):\n".to_owned()
        } else {
            "Named agents for spawn_agent.agent_type (omitting it inherits this agent):\n"
                .to_owned()
        };
        for agent in self
            .definitions
            .values()
            .filter(|agent| !agent.disabled && agent.mode != Mode::Primary && !agent.hidden)
        {
            text.push_str(&format!("- {}: {}\n", agent.name, agent.description.trim()));
        }
        if let Some(agent) = chosen {
            text.push_str(&format!(
                "\nSelected agent: {}\n{}",
                agent.name, agent.developer_instructions
            ));
        }
        Ok(text)
    }

    pub fn inventory(&self) -> Result<Vec<Value>, String> {
        self.definitions
            .iter()
            .filter(|(_, agent)| !agent.disabled)
            .map(|(name, agent)| {
                let source = self
                    .sources
                    .get(name)
                    .map(|source| source.spell())
                    .unwrap_or("builtin");
                // The report rides the visible roster row: once per load, in
                // the pane the user actually reads, without polluting the
                // prompt the selected agent receives.
                let description = bounded_row_text(
                    &agent.description,
                    self.notes.get(name).map(|list| list.as_slice()),
                );
                let mut value = json!({
                    "id":agent.name,"name":agent.name,"description":description,
                    "system":agent.developer_instructions,
                    "request":{"settings":{},"headers":{},"body":{}},
                    "mode":agent.mode.spell(),
                    "hidden":agent.hidden,"permissions":[],
                    "source":source,
                });
                if let Some(model) = agent.model()? {
                    value["model"] = model;
                }
                if let Some(color) = &agent.color {
                    value["color"] = json!(color);
                }
                if let Some(steps) = agent.steps {
                    value["steps"] = json!(steps.get());
                }
                Ok(value)
            })
            .collect()
    }
}

fn builtins() -> BTreeMap<String, Definition> {
    let read_tools = read_tools();
    [
        ("default", "General purpose assistant.", "Carry out the assigned task.", None),
        ("build", "Implement and verify changes.", "Implement the requested changes and run appropriate checks.", None),
        (PLAN_AGENT, "Inspect the repository and prepare a plan.", "Inspect the relevant code, clarify material ambiguities and prepare an actionable plan. Do not change project files; write plan documents only under plans/. Use apply_patch to create the first plan and its parent directories.", Some(planning_tools())),
        ("worker", "Implement a focused task.", "Complete the assigned work; preserve unrelated edits by other workers.", None),
        ("explorer", "Read and explain the codebase.", "Explore the relevant code and report concise findings with file references. Do not modify files.", Some(read_tools.clone())),
        ("reviewer", "Review correctness and maintainability.", "Review the assigned changes. Report concrete defects with file references and explain their consequences. Do not modify files.", Some(read_tools)),
    ].into_iter().map(|(name, description, prompt, tools)| (name.into(), Definition {
        name:name.into(), description:description.into(), developer_instructions:prompt.into(), model:None, tools, color:None,
        mode: match name { "default" => Mode::All, "build" | PLAN_AGENT => Mode::Primary, _ => Mode::Subagent }, steps:None, disabled:false, hidden:false,
    })).collect()
}

fn read_tools() -> BTreeSet<String> {
    BTreeSet::from_iter(
        [
            "read",
            "read_lines",
            "glob",
            "grep",
            "list",
            "web_fetch",
            "websearch",
            "read_history",
            "view_image",
            "skill",
            "update_plan",
        ]
        .map(str::to_owned),
    )
}

pub(super) fn planning_tools() -> BTreeSet<String> {
    let mut tools = read_tools();
    tools.extend(
        [
            "write",
            "edit",
            "apply_patch",
            "patch",
            "request_user_input",
        ]
        .map(str::to_owned),
    );
    tools
}
