//! Bounded lifecycle-hook declarations and additive, narrow-only project bindings.
//!
//! User-owned `hook_commands` declares the execution allowlist. `hooks` maps an
//! event to names in that allowlist. Every user binding is mandatory: project
//! bindings can add constraints but cannot replace, remove or retime user hooks.
//! Repeating a binding never repeats execution. Unknown names/events/fields fail
//! closed; command contents are never included in refusal messages.
//!
//! Trust filtering. Modified for Grapt's JSONC
//! catalog, strict bounds and additive enforcement (rather than state overrides).
//! Copyright 2025 OpenAI. Licensed under the Apache License, Version
//! 2.0 (https://www.apache.org/licenses/LICENSE-2.0); provided AS IS, without
//! warranties or conditions of any kind. See the repository's LICENSE-APACHE.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::matcher::Matcher;
use grapt_config::ConfigValue;

#[path = "inline.rs"]
mod inline;

use crate::dogfood::catalog_config::{
    user_owned, CatalogDocument, CatalogEnvironment, CatalogSources,
};

const MAX_HOOKS_PER_EVENT: usize = 16;
const MAX_COMMANDS: usize = MAX_HOOKS_PER_EVENT * Event::ALL.len();
const MAX_NAME_BYTES: usize = 128;
const MAX_COMMAND_BYTES: usize = 8192;
const MAX_ARGUMENTS: usize = 128;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const MAX_TIMEOUT_MS: u64 = 60_000;

pub(in crate::dogfood) use grapt_session::notice::LifecycleEvent as Event;

/// An explicit argv, including a fixed shell for user-owned inline commands.
/// The executable must be absolute so a project cannot substitute it through
/// the working directory or PATH. The cwd
/// belongs to the declaration, not a project binding, so relative script
/// arguments keep their user-selected base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dogfood) struct HookCommand {
    pub(in crate::dogfood) name: grapt_types::HookId,
    pub(in crate::dogfood) directory: PathBuf,
    pub(in crate::dogfood) command: Vec<String>,
    pub(in crate::dogfood) timeout_ms: u64,
    pub(super) matcher: Option<Matcher>,
    pub(super) permits_allow: bool,
    pub(super) plugin: Option<super::plugin::Context>,
    /// Set only by the user: a failure of this hook stops the turn instead of
    /// being skipped with a warning.
    pub(super) required: bool,
}

impl HookCommand {
    pub(super) fn matches(&self, tool: Option<&str>, alias: Option<&str>) -> bool {
        tool.is_none_or(|name| {
            self.matcher.as_ref().is_none_or(|matcher| {
                matcher.matches(name) || alias.is_some_and(|alias| matcher.matches(alias))
            })
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::dogfood) struct HookSettings {
    events: BTreeMap<Event, Vec<HookCommand>>,
}

impl HookSettings {
    /// Uses the same content-bound origin classification as permissions/plugins;
    /// a profile in a checkout does not become user authority by being selected.
    pub(in crate::dogfood) fn load(workspace: &Path, service_root: &Path) -> Result<Self, String> {
        let sources = CatalogSources::read(workspace, CatalogEnvironment::read(service_root)?)?;
        Self::from_catalog(&sources)
    }

    pub(in crate::dogfood) fn commands(&self, event: Event) -> &[HookCommand] {
        self.events.get(&event).map_or(&[], Vec::as_slice)
    }

    /// Resolve declarations before bindings so trust never depends on whether a
    /// project document precedes an explicit user document in catalog order.
    /// User source order, then project source order, then array order determines
    /// execution order per event. No project binding can reorder user hooks.
    pub(in crate::dogfood) fn from_sources(documents: &[CatalogDocument]) -> Result<Self, String> {
        Self::compose(documents, &[])
    }

    pub(in crate::dogfood) fn from_catalog(sources: &CatalogSources) -> Result<Self, String> {
        let plugins = crate::dogfood::claude_plugins::hooks::load(sources)?;
        if plugins.is_empty() {
            Self::from_sources(&sources.documents)
        } else {
            Self::compose(&sources.documents, &plugins)
        }
    }

    fn compose(
        documents: &[CatalogDocument],
        plugins: &[(CatalogDocument, super::plugin::Context)],
    ) -> Result<Self, String> {
        let mut declared = BTreeMap::new();
        for document in documents {
            if document.value.as_object().is_none() {
                return Err(document.refusal("hook configuration document must be an object"));
            }
            if let Some(value) = document.value.get("hook_commands") {
                if !user_owned(document.origin) {
                    return Err(document.refusal("project config cannot declare hook_commands"));
                }
                declarations(value.value(), &document.directory, &mut declared)
                    .map_err(|reason| document.refusal(&reason))?;
            }
        }

        let mut settings = Self::default();
        // User enforcement always runs first, regardless of source interleaving.
        // Projects may not even reorder it by repeating a user binding earlier.
        let ordered = documents
            .iter()
            .filter(|document| user_owned(document.origin))
            .map(|document| (document, None))
            .chain(
                plugins
                    .iter()
                    .map(|(document, context)| (document, Some(context))),
            )
            .chain(
                documents
                    .iter()
                    .filter(|document| !user_owned(document.origin))
                    .map(|document| (document, None)),
            );
        for (document, context) in ordered {
            if let Some(value) = document.value.get("hooks") {
                settings
                    .bind(value.value(), &declared, document, context)
                    .map_err(|reason| document.refusal(&reason))?;
            }
        }
        Ok(settings)
    }

    fn bind(
        &mut self,
        value: &ConfigValue,
        declared: &BTreeMap<String, HookCommand>,
        document: &CatalogDocument,
        context: Option<&super::plugin::Context>,
    ) -> Result<(), String> {
        let events = value.as_object().ok_or("hooks must be an event object")?;
        for (name, bindings) in events {
            let event = Event::parse(name.value().as_str())?;
            let bindings = bindings
                .value()
                .as_array()
                .ok_or("hooks event must be an array of command names")?;
            if bindings.len() > MAX_HOOKS_PER_EVENT {
                return Err("hooks event exceeds 16 bindings".to_owned());
            }
            let commands = self.events.entry(event).or_default();
            for binding in bindings {
                for command in inline::binding(binding.value(), declared, document, event, context)?
                {
                    // A project repetition cannot rerun or weaken a user binding.
                    if commands.iter().any(|previous| {
                        previous.name == command.name && previous.matcher == command.matcher
                    }) {
                        continue;
                    }
                    if commands.len() >= MAX_HOOKS_PER_EVENT {
                        return Err("effective hooks event exceeds 16 commands".to_owned());
                    }
                    commands.push(command);
                }
            }
        }
        Ok(())
    }
}

fn declarations(
    value: &ConfigValue,
    directory: &Path,
    declared: &mut BTreeMap<String, HookCommand>,
) -> Result<(), String> {
    let entries = value
        .as_object()
        .ok_or("hook_commands must be a named-command object")?;
    if entries.len() > MAX_COMMANDS {
        return Err(format!("hook_commands exceeds {MAX_COMMANDS} declarations"));
    }
    for (name, value) in entries {
        let name = name.value().as_str();
        valid_name(name)?;
        let command = parse_command(name, directory, value.value())?;
        if let Some(previous) = declared.get(name) {
            if previous != &command {
                return Err("hook_commands cannot redefine an existing command".to_owned());
            }
            continue;
        }
        if declared.len() >= MAX_COMMANDS {
            return Err(format!(
                "effective hook_commands exceeds {MAX_COMMANDS} declarations"
            ));
        }
        declared.insert(name.to_owned(), command);
    }
    Ok(())
}

fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(
            "hook command name must be 1..128 ASCII letters, digits, '.', '_' or '-'".into(),
        );
    }
    Ok(())
}

fn parse_command(name: &str, directory: &Path, value: &ConfigValue) -> Result<HookCommand, String> {
    if !directory.is_absolute() {
        return Err("hook command declaration directory must be absolute".to_owned());
    }
    let object = value.as_object().ok_or("hook command must be an object")?;
    let mut seen = BTreeSet::new();
    for (key, _) in object {
        let key = key.value().as_str();
        if !matches!(key, "command" | "timeout_ms" | "required") || !seen.insert(key) {
            return Err("hook command contains an unknown or repeated field".to_owned());
        }
    }
    let argv = value
        .get("command")
        .and_then(|value| value.value().as_array())
        .ok_or("hook command.command must be an argv array")?;
    if argv.is_empty() || argv.len() > MAX_ARGUMENTS {
        return Err("hook command argv must contain 1..128 arguments".to_owned());
    }
    let mut bytes = 0usize;
    let mut command = Vec::with_capacity(argv.len());
    for argument in argv {
        let argument = argument
            .value()
            .as_str()
            .ok_or("hook command argv arguments must be strings")?;
        bytes = bytes
            .checked_add(argument.len())
            .and_then(|bytes| bytes.checked_add(1))
            .ok_or("hook command argv byte count overflowed")?;
        if argument.contains('\0') || bytes > MAX_COMMAND_BYTES {
            return Err("hook command argv must be NUL-free and at most 8192 bytes".to_owned());
        }
        command.push(argument.to_owned());
    }
    if !Path::new(&command[0]).is_absolute() {
        return Err("hook command executable must be an absolute path".to_owned());
    }
    let timeout_ms = match value.get("timeout_ms") {
        Some(value) => value
            .value()
            .as_number()
            .and_then(|number| number.as_u64())
            .filter(|timeout| (1..=MAX_TIMEOUT_MS).contains(timeout))
            .ok_or("hook command timeout_ms must be an integer in 1..60000")?,
        None => DEFAULT_TIMEOUT_MS,
    };
    Ok(HookCommand {
        name: grapt_types::HookId::new(name).ok_or("invalid hook identity")?,
        directory: directory.to_path_buf(),
        command,
        timeout_ms,
        matcher: None,
        permits_allow: false,
        plugin: None,
        required: required(value)?,
    })
}

/// `required` is an optional boolean; anything else is refused, not ignored.
pub(super) fn required(value: &ConfigValue) -> Result<bool, String> {
    value.get("required").map_or(Ok(false), |value| {
        value
            .value()
            .as_bool()
            .ok_or_else(|| "hook required must be true or false".to_owned())
    })
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
