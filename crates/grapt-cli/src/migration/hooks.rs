//! Source settings are read locally; only the existing product owns config writes.
use super::{files, home, ImportCommand, Path};
use crate::{ApiClient, CliLimits, ExitClass, ServiceDiscovery};
use grapt_api::u3a::HookImportRequestDto;
use serde_json::{Map, Value};

pub(super) async fn run(
    command: ImportCommand,
    limits: CliLimits,
) -> Result<Vec<String>, (ExitClass, String)> {
    let source = command.clone();
    let (hooks, directory) = tokio::task::spawn_blocking(move || read(&source))
        .await
        .map_err(|_| (ExitClass::Output, "hook reader could not finish".into()))?
        .map_err(|reason| (ExitClass::Usage, reason))?;
    if hooks.is_empty() {
        return Ok(vec!["No enabled Claude command hooks found.".into()]);
    }
    let root = command
        .service_root
        .or_else(|| crate::command::terminal::user_profile(|name| std::env::var_os(name)))
        .ok_or_else(|| (ExitClass::Usage, "hooks requires --service".into()))?;
    let discovered = ServiceDiscovery::try_new(root, env!("CARGO_PKG_VERSION"))
        .map_err(|error| (ExitClass::Discovery, error.to_string()))?
        .discover()
        .await
        .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
    let client = ApiClient::from_discovered(&discovered, limits)
        .map_err(|error| (ExitClass::Protocol, error.to_string()))?;
    let encoded = serde_json::to_string(&hooks)
        .map_err(|_| (ExitClass::Usage, "hook source cannot be encoded".into()))?;
    let request = HookImportRequestDto {
        hooks: grapt_api::u3a::BoundedString::try_new(encoded)
            .map_err(|_| (ExitClass::Usage, "hooks exceed 64 KiB".into()))?,
        apply: command.apply,
    };
    let result = client
        .import_hooks(&directory, &request)
        .await
        .map_err(|error| (error.exit_class(), format!("hook import refused: {error}")))?;
    let mut rows = vec![format!(
        "import hooks from claude ({})",
        if command.apply {
            "apply"
        } else {
            "dry run; no hooks enabled"
        }
    )];
    rows.push(format!(
        "Hooks run future events in {}; command contents are not displayed.",
        directory.escape_debug()
    ));
    for event in result.imported.as_slice() {
        rows.push(format!(
            "{} {}",
            if command.apply {
                "imported"
            } else {
                "would import"
            },
            event.as_str()
        ));
    }
    for event in result.skipped.as_slice() {
        rows.push(format!("kept existing {}", event.as_str()));
    }
    Ok(rows)
}

fn read(command: &ImportCommand) -> Result<(Map<String, Value>, String), String> {
    let directory = command
        .directory
        .as_ref()
        .map(std::path::PathBuf::from)
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .map_err(|_| "import working directory unavailable")?;
    let directory = std::fs::canonicalize(directory)
        .map_err(|_| "hook working directory cannot be resolved")?;
    let mut merged = Map::new();
    let mut disabled = false;
    for path in [
        home()?.join(".claude/settings.json"),
        directory.join(".claude/settings.json"),
        directory.join(".claude/settings.local.json"),
    ] {
        let Some(bytes) = files::read(&path)? else {
            continue;
        };
        let settings: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Claude settings contain invalid JSON")?;
        let object = settings
            .as_object()
            .ok_or("Claude settings must be a JSON object")?;
        if let Some(value) = object.get("disableAllHooks") {
            disabled = value.as_bool().ok_or("disableAllHooks must be boolean")?;
        }
        let Some(hooks) = object.get("hooks") else {
            continue;
        };
        for (event, groups) in hooks.as_object().ok_or("hooks must be an event object")? {
            let groups = groups.as_array().ok_or("hook event must contain groups")?;
            let previous = merged
                .entry(event.clone())
                .or_insert_with(|| Value::Array(Vec::new()));
            let previous = previous
                .as_array_mut()
                .ok_or("hook event group is malformed")?;
            previous.extend(groups.iter().cloned());
            if merged.len() > 32 {
                return Err("source exceeds 32 hook events".into());
            }
        }
    }
    if disabled {
        merged.clear();
    }
    for groups in merged.values_mut() {
        for group in groups
            .as_array_mut()
            .ok_or("hook groups must be an array")?
        {
            if let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                for hook in hooks {
                    if let Some(text) = hook.get_mut("command") {
                        if let Some(command) = text.as_str() {
                            *text = Value::String(in_workspace(command, &directory));
                        }
                    }
                }
            }
        }
    }
    Ok((merged, directory.to_string_lossy().into_owned()))
}

fn in_workspace(command: &str, directory: &Path) -> String {
    let path = directory.to_string_lossy();
    #[cfg(windows)]
    {
        let path = path.replace('\'', "''");
        format!("$env:CLAUDE_PROJECT_DIR = '{path}'\nSet-Location -LiteralPath '{path}' -ErrorAction Stop\n{command}\n")
    }
    #[cfg(not(windows))]
    {
        let path = path.replace('\'', "'\\''");
        format!("export CLAUDE_PROJECT_DIR='{path}'\ncd '{path}' || exit\n{command}\n")
    }
}
