//! Per-child plugin paths and shell construction; no ambient environment writes.
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dogfood) struct Context {
    pub root: PathBuf,
    pub data: PathBuf,
    pub project: PathBuf,
}
impl Context {
    pub(super) fn environment(&self) -> [(&'static str, String); 3] {
        [
            ("CLAUDE_PLUGIN_ROOT", presentation(&self.root)),
            ("CLAUDE_PLUGIN_DATA", presentation(&self.data)),
            ("CLAUDE_PROJECT_DIR", presentation(&self.project)),
        ]
    }
}

fn presentation(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if let Some(unc) = text.strip_prefix("//?/UNC/") {
        format!("//{unc}")
    } else {
        text.strip_prefix("//?/").unwrap_or(&text).to_owned()
    }
}

pub(super) fn shell(text: &str, context: &Context) -> Result<Vec<String>, String> {
    let (program, powershell) = program()?;
    validate(text, powershell)?;
    let text = if powershell {
        let bindings = context
            .environment()
            .iter()
            .map(|(name, _)| format!("${name}=$env:{name}; "))
            .collect::<String>();
        format!("{bindings}{text}")
    } else {
        text.to_owned()
    };
    Ok(if powershell {
        vec![
            program,
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            text,
        ]
    } else {
        vec![program, "-c".into(), text]
    })
}

fn program() -> Result<(String, bool), String> {
    #[cfg(windows)]
    {
        for name in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(name) {
                let path = PathBuf::from(root).join("Git/bin/bash.exe");
                if path.is_absolute() && path.is_file() {
                    return Ok((path.to_string_lossy().into_owned(), false));
                }
            }
        }
        let system =
            std::env::var_os("SystemRoot").ok_or("SystemRoot is unavailable for plugin hooks")?;
        let path = PathBuf::from(system).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        if !path.is_absolute() || !path.is_file() {
            return Err("plugin hook shell is unavailable".into());
        }
        Ok((path.to_string_lossy().into_owned(), true))
    }
    #[cfg(not(windows))]
    {
        Ok(("/bin/sh".into(), false))
    }
}

// Leave paths in child environment variables so shell expansion cannot turn a
// directory containing dollars, quotes or backticks into executable source.
fn validate(text: &str, powershell: bool) -> Result<(), String> {
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let end = after.find('}');
        let key = end.map_or(after, |end| &after[..end]);
        if (key.starts_with("CLAUDE_") || key.starts_with("user_config."))
            && (end.is_none()
                || ![
                    "CLAUDE_PLUGIN_ROOT",
                    "CLAUDE_PLUGIN_DATA",
                    "CLAUDE_PROJECT_DIR",
                ]
                .contains(&key))
        {
            return Err("plugin hook uses an unsupported runtime variable".into());
        }
        let Some(end) = end else {
            break;
        };
        rest = &after[end + 1..];
    }
    // Only an actual single-quoted literal prevents variable expansion. An
    // apostrophe inside a double-quoted command is ordinary text.
    let mut quote = None;
    let mut escaped = false;
    for (offset, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote != Some('\'') && ((!powershell && ch == '\\') || (powershell && ch == '`')) {
            escaped = true;
        } else if (ch == '\'' && quote != Some('"')) || (ch == '"' && quote != Some('\'')) {
            quote = if quote.is_none() { Some(ch) } else { None };
        } else if ch == '$'
            && quote == Some('\'')
            && [
                "${CLAUDE_PLUGIN_ROOT}",
                "${CLAUDE_PLUGIN_DATA}",
                "${CLAUDE_PROJECT_DIR}",
            ]
            .iter()
            .any(|key| text[offset..].starts_with(key))
        {
            return Err("plugin hook path placeholders in single-quoted literals are not supported yet; use double quotes".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn plugin_paths_reach_only_the_child_without_shell_reinterpretation() -> Result<(), String>
    {
        let directory = std::env::temp_dir();
        let context = Context {
            root: directory.join("root $dollar 'quoted' `literal`"),
            data: directory.join("data $dollar"),
            project: directory.join("project $dollar"),
        };
        // printf is supplied by sh/Git Bash; a missing compatible shell is a
        // real failure of the adapter, rather than a skipped native check.
        let command = super::super::config::HookCommand {
            name: grapt_types::HookId::new("plugin-paths").ok_or("invalid fixture hook identity")?,
            directory,
            command: shell("printf '%s\\n' \"Plugin's root: ${CLAUDE_PLUGIN_ROOT}\" \"${CLAUDE_PLUGIN_DATA}\" \"${CLAUDE_PROJECT_DIR}\"", &context)?,
            timeout_ms: 10000,
            matcher: None,
            permits_allow: true,
            plugin: Some(context.clone()),
        };
        assert!(shell("echo '${CLAUDE_PLUGIN_ROOT}'", &context).is_err());
        let environment = crate::dogfood::tools::shell_environment::Environment::parent()?;
        let before: Vec<_> = environment
            .iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        let result = super::super::command::run(&command, &environment, b"{}\n").await?;
        let expected = context
            .environment()
            .into_iter()
            .enumerate()
            .map(|(index, (_, value))| {
                if index == 0 {
                    format!("Plugin's root: {value}")
                } else {
                    value
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(result.notes, vec![expected]);
        assert_eq!(
            before,
            environment
                .iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect::<Vec<_>>()
        );
        Ok(())
    }
}
