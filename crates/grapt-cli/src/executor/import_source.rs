//! The import reader: bounded, refusal-honest reads of the source tool's
//! configuration.
//!
//! A source file is operator configuration, not a protocol channel: every
//! read is capped, every definition the import cannot represent is refused
//! naming the problem, and a present field with the wrong type is a broken
//! source, never an absent one. Entries carry the writer payload inside the
//! read result and nowhere else: no reader output carries an environment
//! value, a header value or a full URL.
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::command::import::ImportSource;

/// The sources are operator configuration, not a protocol channel: a hard
/// bound keeps a runaway or hostile file from dominating this command.
const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
/// The product refuses an MCP catalog above 256 servers; cap the source at
/// the same width so one file cannot crowd out every later entry.
const MAX_SOURCE_SERVERS: usize = 256;
const MAX_ENV_ENTRIES: usize = 256;
const MAX_ENV_KEY_BYTES: usize = 1024;
const MAX_ENV_VALUE_BYTES: usize = 32_768;
const MAX_COMMAND_ARGS: usize = 128;

#[derive(Clone)]
pub(super) struct Imported {
    pub(super) name: String,
    pub(super) transport: &'static str,
    pub(super) source_scope: &'static str,
    /// The Grapt configuration for the writer. Carries secrets; the type
    /// has no Debug, so it can never be rendered.
    pub(super) config: serde_json::Value,
}

#[derive(Default)]
pub(super) struct ReadResult {
    pub(super) entries: Vec<Imported>,
    /// (name, static reason) for definitions the import cannot represent.
    /// Reasons are fixed strings; the name is escaped at the render site.
    pub(super) refused: Vec<(String, String)>,
}

fn home_directory() -> Option<PathBuf> {
    let value = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

pub(super) fn read_source(
    source: ImportSource,
    project: Option<&str>,
) -> Result<ReadResult, String> {
    match source {
        ImportSource::Claude => read_claude(project),
        ImportSource::Codex => read_codex(),
    }
}

fn read_claude(project: Option<&str>) -> Result<ReadResult, String> {
    let mut user = ReadResult::default();
    if let Some(home) = home_directory() {
        if let Some(value) = read_json_object(home.join(".claude.json"))? {
            claude_servers(value.get("mcpServers"), "user", &mut user)?;
        }
    }
    let mut project_read = ReadResult::default();
    if let Some(project) = project {
        let path = Path::new(project).join(".mcp.json");
        if let Some(value) = read_json_object(path)? {
            claude_servers(value.get("mcpServers"), "project", &mut project_read)?;
        }
    }
    merge_scopes(user, project_read)
}

fn claude_servers(
    servers: Option<&serde_json::Value>,
    scope: &'static str,
    read: &mut ReadResult,
) -> Result<(), String> {
    let Some(servers) = servers else {
        return Ok(());
    };
    // A present container with the wrong type is a broken source, not an
    // empty one: "No MCP servers found" would call a broken file clean.
    let servers = servers
        .as_object()
        .ok_or_else(|| "mcpServers must be an object".to_owned())?;
    for (name, value) in servers {
        if read.entries.len() + read.refused.len() >= MAX_SOURCE_SERVERS {
            return Err("the source declares more than 256 MCP servers".to_owned());
        }
        let entry = match value.as_object() {
            None => Err("entry must be an object".to_owned()),
            Some(object) => match object.get("type") {
                // Inference is for an absent `type` alone: a present value
                // that is not a usable string is a broken definition,
                // refused naming the field instead of imported as guessed.
                None => inferred_entry(name, object, scope),
                Some(value) => match value.as_str() {
                    Some("stdio") => stdio_entry(name, object, scope),
                    Some("http") | Some("sse") => remote_entry(name, object, scope),
                    Some(_) => Err("transport is not supported".to_owned()),
                    None => Err("type must be a string".to_owned()),
                },
            },
        };
        match entry {
            Ok(entry) => read.entries.push(entry),
            Err(reason) => read.refused.push((name.to_owned(), reason)),
        }
    }
    Ok(())
}

/// The source configuration gives stdio entries as `{"command": ...}` and remote
/// entries as `{"url": ...}` with no `type` at all. Honor both shapes; an
/// entry that names both, or names a field that is not a usable string, is
/// refused naming the problem instead of being imported or dropped.
fn inferred_entry(
    name: &str,
    object: &serde_json::Map<String, serde_json::Value>,
    scope: &'static str,
) -> Result<Imported, String> {
    let command = object.get("command").filter(|value| !value.is_null());
    let url = object.get("url").filter(|value| !value.is_null());
    match (command, url) {
        (Some(_), Some(_)) => Err("entry has both command and url".to_owned()),
        (Some(command), None) => {
            if command.as_str().is_none() {
                return Err("stdio entry requires a command".to_owned());
            }
            stdio_entry(name, object, scope)
        }
        (None, Some(url)) => {
            if url.as_str().is_none() {
                return Err("remote entry requires a url".to_owned());
            }
            remote_entry(name, object, scope)
        }
        (None, None) => Err("entry requires a type".to_owned()),
    }
}

/// The project scope overrides the user scope NAME BY NAME: a name declared
/// in the project file imports from the project file alone. A valid project
/// entry replaces the user's definition; a refused project entry takes the
/// name off the table instead of leaving the user's copy importable.
fn merge_scopes(user: ReadResult, project: ReadResult) -> Result<ReadResult, String> {
    let mut merged = ReadResult {
        entries: project.entries,
        refused: project.refused,
    };
    let declared = merged
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .chain(merged.refused.iter().map(|(name, _)| name.clone()))
        .collect::<BTreeSet<_>>();
    for entry in user
        .entries
        .into_iter()
        .filter(|entry| !declared.contains(&entry.name))
    {
        merged.entries.push(entry);
    }
    for (name, reason) in user
        .refused
        .into_iter()
        .filter(|(name, _)| !declared.contains(name))
    {
        merged.refused.push((name, reason));
    }
    if merged.entries.len() + merged.refused.len() > MAX_SOURCE_SERVERS {
        return Err("the sources declare more than 256 MCP servers".to_owned());
    }
    Ok(merged)
}

fn stdio_entry(
    name: &str,
    object: &serde_json::Map<String, serde_json::Value>,
    scope: &'static str,
) -> Result<Imported, String> {
    if !valid_name(name) {
        return Err("server name is invalid".to_owned());
    }
    let command = object
        .get("command")
        .and_then(serde_json::Value::as_str)
        .filter(|command| !command.is_empty())
        .ok_or_else(|| "stdio entry requires a command".to_owned())?;
    let args = object
        .get("args")
        .map(|value| string_list(value, "args"))
        .transpose()?
        .unwrap_or_default();
    if 1 + args.len() > MAX_COMMAND_ARGS {
        return Err("command exceeds its argument bound".to_owned());
    }
    let environment = object
        .get("env")
        .map(|value| string_map(value, "env"))
        .transpose()?
        .unwrap_or_default();
    let disabled = disabled_flag(object)?;
    let mut config = serde_json::json!({
        "type": "local",
        "command": std::iter::once(command.to_owned()).chain(args).collect::<Vec<_>>(),
        "environment": environment,
    });
    if disabled {
        config["disabled"] = serde_json::Value::Bool(true);
    }
    Ok(Imported {
        name: name.to_owned(),
        transport: "local",
        source_scope: scope,
        config,
    })
}

pub(super) fn remote_entry(
    name: &str,
    object: &serde_json::Map<String, serde_json::Value>,
    scope: &'static str,
) -> Result<Imported, String> {
    if !valid_name(name) {
        return Err("server name is invalid".to_owned());
    }
    let raw = object
        .get("url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "remote entry requires a url".to_owned())?;
    let url = reqwest::Url::parse(raw).map_err(|_| "remote url is invalid".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("remote url must use HTTP(S)".to_owned());
    }
    let headers = object
        .get("headers")
        .map(|value| string_map(value, "headers"))
        .transpose()?
        .unwrap_or_default();
    let disabled = disabled_flag(object)?;
    let mut config = serde_json::json!({
        "type": "remote",
        "url": raw,
        "headers": headers,
    });
    if disabled {
        config["disabled"] = serde_json::Value::Bool(true);
    }
    Ok(Imported {
        name: name.to_owned(),
        transport: "remote",
        source_scope: scope,
        config,
    })
}

fn read_codex() -> Result<ReadResult, String> {
    let mut read = ReadResult::default();
    let Some(home) = home_directory() else {
        return Ok(read);
    };
    let Some(bytes) = read_bounded(home.join(".codex/config.toml"))? else {
        return Ok(read);
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "~/.codex/config.toml is not UTF-8 text".to_owned())?;
    let value: toml::Value =
        toml::from_str(text).map_err(|_| "~/.codex/config.toml is not valid TOML".to_owned())?;
    let Some(servers) = value.get("mcp_servers") else {
        return Ok(read);
    };
    // A present container with the wrong type is a broken source, not an
    // empty one: "No MCP servers found" would call a broken file clean.
    let servers = servers
        .as_table()
        .ok_or_else(|| "mcp_servers must be a table".to_owned())?;
    for (name, value) in servers {
        if read.entries.len() + read.refused.len() >= MAX_SOURCE_SERVERS {
            return Err("the source declares more than 256 MCP servers".to_owned());
        }
        let entry = match value.as_table() {
            None => Err("entry must be a table".to_owned()),
            Some(object) => codex_entry(name, object),
        };
        match entry {
            Ok(entry) => read.entries.push(entry),
            Err(reason) => read.refused.push((name.to_owned(), reason)),
        }
    }
    Ok(read)
}

/// The settings this import translates from a Codex entry. Anything else --
/// a bearer-token source, a tool allowlist, a timeout -- changes how the
/// server starts or what it may do, so the entry is refused naming the
/// field instead of being imported without it.
const CODEX_TRANSLATED: &[&str] = &[
    "command",
    "args",
    "env",
    "enabled",
    "url",
    "http_headers",
    "cwd",
];

fn codex_entry(
    name: &str,
    object: &toml::map::Map<String, toml::Value>,
) -> Result<Imported, String> {
    for key in object.keys() {
        if !CODEX_TRANSLATED.contains(&key.as_str()) {
            return Err(format!(
                "codex entry declares {}, which the import does not support",
                key.escape_debug()
            ));
        }
    }
    match (object.get("command"), object.get("url")) {
        (Some(_), Some(_)) => Err("entry has both command and url".to_owned()),
        (Some(command), None) => {
            let command = command
                .as_str()
                .ok_or_else(|| "stdio entry requires a command".to_owned())?;
            codex_stdio(name, command, object)
        }
        (None, Some(url)) => {
            let url = url
                .as_str()
                .ok_or_else(|| "remote entry requires a url".to_owned())?;
            codex_remote(name, url, object)
        }
        (None, None) => Err("entry has neither command nor url".to_owned()),
    }
}

/// Codex declares `enabled` (absent means enabled). A value with the wrong
/// type is a malformed known field, so it is refused rather than defaulted.
fn codex_enabled(object: &toml::map::Map<String, toml::Value>) -> Result<bool, String> {
    match object.get("enabled") {
        Some(value) => Ok(!value
            .as_bool()
            .ok_or_else(|| "enabled must be a boolean".to_owned())?),
        None => Ok(false),
    }
}

/// The child's working directory. The writer resolves it against the
/// location exactly like any other local declaration, so the value is
/// carried through unchanged once it is a usable string.
fn codex_cwd(object: &toml::map::Map<String, toml::Value>) -> Result<Option<String>, String> {
    match object.get("cwd") {
        None => Ok(None),
        Some(value) => {
            let cwd = value
                .as_str()
                .ok_or_else(|| "cwd must be a string".to_owned())?;
            if cwd.is_empty() {
                return Err("cwd must not be empty".to_owned());
            }
            Ok(Some(cwd.to_owned()))
        }
    }
}

fn codex_stdio(
    name: &str,
    command: &str,
    object: &toml::map::Map<String, toml::Value>,
) -> Result<Imported, String> {
    if !valid_name(name) {
        return Err("server name is invalid".to_owned());
    }
    if command.is_empty() {
        return Err("stdio entry requires a command".to_owned());
    }
    let args = object
        .get("args")
        .map(toml_string_list)
        .transpose()?
        .unwrap_or_default();
    if 1 + args.len() > MAX_COMMAND_ARGS {
        return Err("command exceeds its argument bound".to_owned());
    }
    let environment = object
        .get("env")
        .map(toml_string_map)
        .transpose()?
        .unwrap_or_default();
    let disabled = codex_enabled(object)?;
    let cwd = codex_cwd(object)?;
    let mut config = serde_json::json!({
        "type": "local",
        "command": std::iter::once(command.to_owned()).chain(args).collect::<Vec<_>>(),
        "environment": environment,
    });
    if let Some(cwd) = cwd {
        config["cwd"] = serde_json::Value::String(cwd);
    }
    if disabled {
        config["disabled"] = serde_json::Value::Bool(true);
    }
    Ok(Imported {
        name: name.to_owned(),
        transport: "local",
        source_scope: "user",
        config,
    })
}

fn codex_remote(
    name: &str,
    raw: &str,
    object: &toml::map::Map<String, toml::Value>,
) -> Result<Imported, String> {
    if !valid_name(name) {
        return Err("server name is invalid".to_owned());
    }
    if object.contains_key("cwd") {
        return Err("cwd is not supported on a remote entry".to_owned());
    }
    let url = reqwest::Url::parse(raw).map_err(|_| "remote url is invalid".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("remote url must use HTTP(S)".to_owned());
    }
    let headers = object
        .get("http_headers")
        .map(toml_string_map)
        .transpose()?
        .unwrap_or_default();
    let disabled = codex_enabled(object)?;
    let mut config = serde_json::json!({
        "type": "remote",
        "url": raw,
        "headers": headers,
    });
    if disabled {
        config["disabled"] = serde_json::Value::Bool(true);
    }
    Ok(Imported {
        name: name.to_owned(),
        transport: "remote",
        source_scope: "user",
        config,
    })
}

fn read_json_object(path: PathBuf) -> Result<Option<serde_json::Value>, String> {
    let Some(bytes) = read_bounded(path)? else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "the source is not valid JSON".to_owned())?;
    if !value.is_object() {
        return Err("the source is not a JSON object".to_owned());
    }
    Ok(Some(value))
}

fn read_bounded(path: PathBuf) -> Result<Option<Vec<u8>>, String> {
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {}", path.display(), error.kind())),
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err(bound_error(&path, MAX_SOURCE_BYTES));
    }
    // The path can grow or be replaced between that check and the open, so
    // the bound is re-checked on the opened handle and the read itself is
    // capped one byte past it. The open is nonblocking where the platform
    // names the flag, so a swap to a FIFO refuses instead of hanging.
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    let file = options
        .open(&path)
        .map_err(|error| format!("{}: {}", path.display(), error.kind()))?;
    read_handle(file, &path, MAX_SOURCE_BYTES).map(Some)
}

/// Read an already-opened source under the byte bound. The handle's own
/// metadata is authoritative for what was opened, and the read stops one
/// byte past the limit, so growth between the check and the read overflows
/// into a refusal instead of a silent over-read.
fn read_handle(mut file: std::fs::File, path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("{}: {}", path.display(), error.kind()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > limit {
        return Err(bound_error(path, limit));
    }
    let mut bytes = Vec::new();
    std::io::Read::take(&mut file, limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {}", path.display(), error.kind()))?;
    if (bytes.len() as u64) > limit {
        return Err(bound_error(path, limit));
    }
    Ok(bytes)
}

fn bound_error(path: &Path, limit: u64) -> String {
    format!("{} exceeds the {limit}-byte import bound", path.display())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 1024
        && !name.chars().any(|character| character.is_control())
        && !matches!(name, "." | "..")
}

fn disabled_flag(object: &serde_json::Map<String, serde_json::Value>) -> Result<bool, String> {
    let flag = |key: &str| -> Result<Option<bool>, String> {
        object
            .get(key)
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| format!("{key} must be a boolean"))
            })
            .transpose()
    };
    Ok(flag("disabled")?.unwrap_or(false) || flag("enabled")? == Some(false))
}

fn string_list(value: &serde_json::Value, field: &str) -> Result<Vec<String>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{field} must be an array of strings"))?;
    if items.len() > MAX_COMMAND_ARGS {
        return Err(format!("{field} exceeds its argument bound"));
    }
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{field} must be an array of strings"))
        })
        .collect()
}

fn toml_string_list(value: &toml::Value) -> Result<Vec<String>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| "args must be an array of strings".to_owned())?;
    if items.len() > MAX_COMMAND_ARGS {
        return Err("args exceeds its argument bound".to_owned());
    }
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "args must be an array of strings".to_owned())
        })
        .collect()
}

fn string_map(
    value: &serde_json::Value,
    field: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{field} must be an object of strings"))?;
    if object.len() > MAX_ENV_ENTRIES {
        return Err(format!(
            "{field} declares more than {MAX_ENV_ENTRIES} entries"
        ));
    }
    let mut entries = serde_json::Map::new();
    for (key, value) in object {
        if key.is_empty()
            || key.len() > MAX_ENV_KEY_BYTES
            || key
                .chars()
                .any(|character| matches!(character, '\0' | '\r' | '\n' | '='))
        {
            return Err(format!("{field} has an invalid name"));
        }
        let text = value
            .as_str()
            .ok_or_else(|| format!("{field} values must be strings"))?;
        if text.len() > MAX_ENV_VALUE_BYTES
            || text
                .chars()
                .any(|character| matches!(character, '\0' | '\r' | '\n'))
        {
            return Err(format!("{field} value is invalid"));
        }
        entries.insert(key.clone(), serde_json::Value::String(text.to_owned()));
    }
    Ok(entries)
}

fn toml_string_map(
    value: &toml::Value,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let object = value
        .as_table()
        .ok_or_else(|| "environment values must be a table of strings".to_owned())?;
    if object.len() > MAX_ENV_ENTRIES {
        return Err(format!(
            "environment declares more than {MAX_ENV_ENTRIES} entries"
        ));
    }
    let mut entries = serde_json::Map::new();
    for (key, value) in object {
        if key.is_empty()
            || key.len() > MAX_ENV_KEY_BYTES
            || key
                .chars()
                .any(|character| matches!(character, '\0' | '\r' | '\n' | '='))
        {
            return Err("environment has an invalid name".to_owned());
        }
        let text = value
            .as_str()
            .ok_or_else(|| "environment values must be strings".to_owned())?;
        if text.len() > MAX_ENV_VALUE_BYTES
            || text
                .chars()
                .any(|character| matches!(character, '\0' | '\r' | '\n'))
        {
            return Err("environment value is invalid".to_owned());
        }
        entries.insert(key.clone(), serde_json::Value::String(text.to_owned()));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_entry(name: &str, scope: &'static str) -> Imported {
        Imported {
            name: name.to_owned(),
            transport: "local",
            source_scope: scope,
            config: serde_json::json!({}),
        }
    }

    fn fixture_directory(
        label: &str,
    ) -> Result<crate::testfs::ScratchDir, Box<dyn std::error::Error>> {
        Ok(crate::testfs::ScratchDir::new(&format!("import-{label}"))?)
    }

    #[test]
    fn project_scope_overrides_the_user_scope_name_by_name() -> Result<(), String> {
        let user = ReadResult {
            entries: vec![
                fixture_entry("shared", "user"),
                fixture_entry("solo", "user"),
                fixture_entry("blocked", "user"),
            ],
            refused: Vec::new(),
        };
        let project = ReadResult {
            // A valid project entry replaces the user's `shared` definition.
            entries: vec![fixture_entry("shared", "project")],
            // A refused project entry takes `blocked` off the table even
            // though the user scope carries an importable copy of it.
            refused: vec![("blocked".to_owned(), "entry requires a type".to_owned())],
        };
        let merged = merge_scopes(user, project)?;
        let mut entries = merged
            .entries
            .iter()
            .map(|entry| (entry.name.clone(), entry.source_scope.to_owned()))
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(
            entries,
            vec![
                ("shared".to_owned(), "project".to_owned()),
                ("solo".to_owned(), "user".to_owned()),
            ]
        );
        let mut refused = merged
            .refused
            .iter()
            .map(|(name, reason)| (name.clone(), reason.clone()))
            .collect::<Vec<_>>();
        refused.sort();
        assert_eq!(
            refused,
            vec![("blocked".to_owned(), "entry requires a type".to_owned())]
        );
        Ok(())
    }

    #[test]
    fn merge_scopes_refuse_a_source_wider_than_the_catalog() {
        let user = ReadResult {
            entries: (0..129)
                .map(|index| fixture_entry(&format!("user-{index}"), "user"))
                .collect(),
            refused: Vec::new(),
        };
        let project = ReadResult {
            entries: (0..129)
                .map(|index| fixture_entry(&format!("project-{index}"), "project"))
                .collect(),
            refused: Vec::new(),
        };
        assert_eq!(
            merge_scopes(user, project).err(),
            Some("the sources declare more than 256 MCP servers".to_owned())
        );
    }

    #[test]
    fn claude_entries_without_a_type_infer_their_transport() -> Result<(), String> {
        let mut read = ReadResult::default();
        claude_servers(
            Some(&serde_json::json!({
                "shell": {"command": "shell-mcp", "args": ["--stdio"]},
                "api": {"url": "https://api.example/mcp"},
                "ambiguous": {"command": "both", "url": "https://both.example"},
                "typeless": {},
                "number": {"command": 42},
            })),
            "user",
            &mut read,
        )?;
        let mut entries = read
            .entries
            .iter()
            .map(|entry| (entry.name.clone(), entry.transport.to_owned()))
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(
            entries,
            vec![
                ("api".to_owned(), "remote".to_owned()),
                ("shell".to_owned(), "local".to_owned()),
            ]
        );
        let mut refused = read
            .refused
            .iter()
            .map(|(name, reason)| (name.clone(), reason.clone()))
            .collect::<Vec<_>>();
        refused.sort();
        assert_eq!(
            refused,
            vec![
                (
                    "ambiguous".to_owned(),
                    "entry has both command and url".to_owned()
                ),
                (
                    "number".to_owned(),
                    "stdio entry requires a command".to_owned()
                ),
                ("typeless".to_owned(), "entry requires a type".to_owned()),
            ]
        );
        // A present container with the wrong type is an error, not a refusal.
        let malformed = claude_servers(
            Some(&serde_json::json!(["docs"])),
            "user",
            &mut ReadResult::default(),
        );
        assert_eq!(malformed, Err("mcpServers must be an object".to_owned()));
        // An object container with a broken entry refuses that entry alone.
        let mut broken_entry = ReadResult::default();
        claude_servers(
            Some(&serde_json::json!({"docs": "not-an-object"})),
            "user",
            &mut broken_entry,
        )?;
        assert!(broken_entry.entries.is_empty());
        assert_eq!(
            broken_entry.refused,
            vec![("docs".to_owned(), "entry must be an object".to_owned())]
        );
        assert!(claude_servers(None, "user", &mut ReadResult::default()).is_ok());
        Ok(())
    }

    #[test]
    fn claude_entries_refuse_a_present_non_string_type_instead_of_inferring() -> Result<(), String>
    {
        // A `type` key that is present but not a string is a broken
        // definition, refused naming the field; only an absent key opens
        // inference.
        let mut read = ReadResult::default();
        claude_servers(
            Some(&serde_json::json!({
                "number": {"type": 42, "command": "echo"},
                "null": {"type": null, "command": "echo"},
                "array": {"type": ["stdio"], "url": "https://array.example/mcp"},
            })),
            "user",
            &mut read,
        )?;
        assert!(read.entries.is_empty());
        let mut refused = read
            .refused
            .iter()
            .map(|(name, reason)| (name.clone(), reason.clone()))
            .collect::<Vec<_>>();
        refused.sort();
        assert_eq!(
            refused,
            vec![
                ("array".to_owned(), "type must be a string".to_owned()),
                ("null".to_owned(), "type must be a string".to_owned()),
                ("number".to_owned(), "type must be a string".to_owned()),
            ]
        );
        Ok(())
    }

    #[test]
    fn codex_entries_refuse_settings_the_import_cannot_translate(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let document: toml::Value = toml::from_str(
            r#"
[mcp_servers.audit]
command = "audit"
bearer_token_env_var = "AUDIT_TOKEN"

[mcp_servers.tools]
command = "tools"
enabled_tools = ["read_file"]

[mcp_servers.flagged]
command = "flagged"
enabled = "yes"

[mcp_servers.wandering]
command = "wandering"
cwd = "tools"

[mcp_servers.remote_home]
url = "https://remote.example/mcp"
cwd = "tools"
"#,
        )?;
        let servers = document
            .get("mcp_servers")
            .and_then(toml::Value::as_table)
            .ok_or("the fixture must carry mcp_servers")?;
        let entry = |name: &str| -> Result<Imported, String> {
            let object = servers
                .get(name)
                .and_then(toml::Value::as_table)
                .ok_or_else(|| format!("the fixture must carry {name}"))?;
            codex_entry(name, object)
        };
        assert_eq!(
            entry("audit").err(),
            Some(
                "codex entry declares bearer_token_env_var, which the import does not support"
                    .to_owned()
            )
        );
        assert_eq!(
            entry("tools").err(),
            Some(
                "codex entry declares enabled_tools, which the import does not support".to_owned()
            )
        );
        assert_eq!(
            entry("flagged").err(),
            Some("enabled must be a boolean".to_owned())
        );
        let wandering = entry("wandering")?;
        assert_eq!(wandering.config["cwd"], serde_json::json!("tools"));
        assert_eq!(
            entry("remote_home").err(),
            Some("cwd is not supported on a remote entry".to_owned())
        );
        Ok(())
    }

    #[test]
    fn read_handle_keeps_the_bound_on_the_opened_file() -> Result<(), Box<dyn std::error::Error>> {
        let directory = fixture_directory("bound")?;
        let path = directory.join("source.json");
        std::fs::write(&path, b"0123456789")?;
        // At the bound exactly, the read succeeds.
        let file = std::fs::File::open(&path)?;
        assert_eq!(read_handle(file, &path, 10)?, b"0123456789");
        // Past it, the refusal names the bound.
        let file = std::fs::File::open(&path)?;
        let refused = read_handle(file, &path, 9);
        let Err(reason) = refused else {
            panic!("growth past the bound must be refused");
        };
        assert!(
            reason.contains("exceeds the 9-byte import bound"),
            "{reason}"
        );
        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }

    #[test]
    fn read_bounded_treats_missing_and_nonfile_paths_as_absent(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = fixture_directory("absent")?;
        assert_eq!(read_bounded(directory.join("missing.json"))?, None);
        assert_eq!(read_bounded(directory.to_path_buf())?, None);
        let oversized = directory.join("source.json");
        let file = std::fs::File::create(&oversized)?;
        file.set_len(MAX_SOURCE_BYTES + 1)?;
        let refused = read_bounded(oversized);
        let Err(reason) = refused else {
            panic!("an oversize source must be refused");
        };
        assert!(
            reason.contains("exceeds the 16777216-byte import bound"),
            "{reason}"
        );
        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn read_handle_refuses_a_fifo_handle_without_blocking() -> Result<(), Box<dyn std::error::Error>>
    {
        // The regression the nonblocking open guards: a FIFO swapped under
        // the source path opens, and its handle is refused on the fstat
        // instead of blocking the reader.
        let directory = fixture_directory("fifo")?;
        let path = directory.join("source.json");
        let status = std::process::Command::new("mkfifo").arg(&path).status()?;
        assert!(status.success(), "mkfifo must exist on this platform");
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
        let file = options.open(&path)?;
        let refused = read_handle(file, &path, MAX_SOURCE_BYTES);
        let Err(reason) = refused else {
            panic!("a FIFO handle must be refused");
        };
        assert!(reason.contains("is not a regular file"), "{reason}");
        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }
}
