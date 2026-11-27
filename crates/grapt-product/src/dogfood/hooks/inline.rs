//! Configured command groups share the bounded, user-owned executor.
use super::*;

pub(super) fn binding(
    value: &ConfigValue,
    declared: &BTreeMap<String, HookCommand>,
    document: &CatalogDocument,
    event: Event,
    context: Option<&super::super::plugin::Context>,
) -> Result<Vec<HookCommand>, String> {
    if let Some(name) = value.as_str() {
        if context.is_some() {
            return Err("plugin hooks require command objects".into());
        }
        return Ok(vec![named(name, declared, document, None)?]);
    }
    fields(value, &["matcher", "hooks"])?;
    let pattern = match value.get("matcher") {
        Some(value) => value
            .value()
            .as_str()
            .ok_or("hook matcher must be a string")?,
        None => "",
    };
    let matcher = Some(Matcher::new(pattern)?);
    let hooks = value
        .get("hooks")
        .and_then(|value| value.value().as_array())
        .ok_or("hook group requires hooks[]")?;
    if hooks.len() > MAX_HOOKS_PER_EVENT {
        return Err("hook group exceeds 16 commands".into());
    }
    hooks
        .iter()
        .map(|hook| {
            if let Some(name) = hook.value().as_str() {
                if context.is_some() {
                    return Err("plugin hooks require command objects".into());
                }
                named(name, declared, document, matcher.clone())
            } else {
                command(hook.value(), document, event, matcher.clone(), context)
            }
        })
        .collect()
}

fn named(
    name: &str,
    declared: &BTreeMap<String, HookCommand>,
    document: &CatalogDocument,
    matcher: Option<Matcher>,
) -> Result<HookCommand, String> {
    valid_name(name)?;
    let mut command = declared
        .get(name)
        .cloned()
        .ok_or("hooks binding is not declared in user hook_commands")?;
    command.permits_allow = user_owned(document.origin) && matcher.is_some();
    command.matcher = matcher;
    Ok(command)
}

fn command(
    value: &ConfigValue,
    document: &CatalogDocument,
    event: Event,
    matcher: Option<Matcher>,
    context: Option<&super::super::plugin::Context>,
) -> Result<HookCommand, String> {
    if !user_owned(document.origin) {
        return Err(
            "project config cannot declare inline hook commands; use a user hook_commands binding"
                .into(),
        );
    }
    if !document.directory.is_absolute() {
        return Err("hook command declaration directory must be absolute".into());
    }
    fields(value, &["type", "command", "timeout"])?;
    if value.get("type").and_then(|value| value.value().as_str()) != Some("command") {
        return Err("hook type must be command".into());
    }
    let text = value
        .get("command")
        .and_then(|value| value.value().as_str())
        .filter(|text| {
            !text.trim().is_empty() && !text.contains('\0') && text.len() <= MAX_COMMAND_BYTES
        })
        .ok_or("inline hook command must be NUL-free and contain 1..8192 bytes")?;
    let timeout_ms = match value.get("timeout") {
        Some(value) => {
            value
                .value()
                .as_number()
                .and_then(|n| n.as_u64())
                .filter(|n| (1..=60).contains(n))
                .ok_or("hook timeout must be integer seconds in 1..60")?
                * 1000
        }
        None => DEFAULT_TIMEOUT_MS,
    };
    let identity = serde_json::to_vec(&serde_json::json!([
        document.source,
        event.as_str(),
        text,
        timeout_ms,
    ]))
    .map_err(|_| "inline hook identity encoding failed")?;
    let digest: String = ring::digest::digest(&ring::digest::SHA256, &identity)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(HookCommand {
        name: grapt_types::HookId::new(format!("inline-{digest}"))
            .ok_or("invalid inline hook identity")?,
        directory: document.directory.clone(),
        command: match context {
            Some(context) => super::super::plugin::shell(text, context)?,
            None => shell(text)?,
        },
        timeout_ms,
        matcher,
        permits_allow: true,
        plugin: context.cloned(),
    })
}

fn fields(value: &ConfigValue, allowed: &[&str]) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("hook binding must be a command name or matcher/hooks object")?;
    let mut seen = BTreeSet::new();
    for (name, _) in object {
        if !allowed.contains(&name.value().as_str()) || !seen.insert(name.value().as_str()) {
            return Err("hook binding contains an unknown or repeated field".into());
        }
    }
    Ok(())
}

fn shell(text: &str) -> Result<Vec<String>, String> {
    #[cfg(windows)]
    {
        let system =
            std::env::var_os("SystemRoot").ok_or("SystemRoot is unavailable for command hook")?;
        let program = PathBuf::from(system).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        if !program.is_absolute() {
            return Err("hook shell must be absolute".into());
        }
        Ok(vec![
            program.to_string_lossy().into_owned(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            text.into(),
        ])
    }
    #[cfg(not(windows))]
    {
        Ok(vec!["/bin/sh".into(), "-c".into(), text.into()])
    }
}
