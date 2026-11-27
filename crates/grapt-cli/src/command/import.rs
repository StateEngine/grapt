//! Preview or import setup written for other agent tools.
use super::{check_len, CliCommand, CommandError};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    Claude,
    Codex,
}

impl ImportSource {
    pub fn label(self) -> &'static str {
        match self {
            ImportSource::Claude => "claude",
            ImportSource::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    Mcp,
    Instructions,
    Skills,
    Hooks,
}
impl ImportKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Instructions => "instructions",
            Self::Skills => "skills",
            Self::Hooks => "hooks",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportCommand {
    pub source: ImportSource,
    pub kind: ImportKind,
    pub service_root: Option<PathBuf>,
    pub directory: Option<String>,
    /// Write the definitions; without it the command only reports the plan.
    pub apply: bool,
}

const GRAMMAR: &str = "import <mcp|instructions|skills|hooks> requires --from claude|codex; mcp also requires --service <root>";

pub(super) fn parse(arguments: &[String]) -> Result<CliCommand, CommandError> {
    let fail = || CommandError::Import(GRAMMAR);
    if arguments.len() > 10
        || arguments.iter().any(|value| {
            value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control)
        })
    {
        return Err(fail());
    }
    let kind = match arguments.first().map(String::as_str) {
        Some("mcp") => ImportKind::Mcp,
        Some("instructions") => ImportKind::Instructions,
        Some("skills") => ImportKind::Skills,
        Some("hooks") => ImportKind::Hooks,
        _ => {
            return Err(CommandError::Import(
                "import supports mcp, instructions, skills and hooks",
            ))
        }
    };
    let mut source = None;
    let mut service_root = None;
    let mut directory = None;
    let mut apply = false;
    let mut values = arguments[1..].iter();
    while let Some(flag) = values.next() {
        if flag == "--apply" {
            if apply {
                return Err(fail());
            }
            apply = true;
            continue;
        }
        let value = values
            .next()
            .filter(|value| !value.starts_with('-'))
            .ok_or_else(fail)?;
        match flag.as_str() {
            "--from" => {
                if source.is_some() {
                    return Err(fail());
                }
                check_len("import source", value, 16)?;
                source = Some(match value.as_str() {
                    "claude" => ImportSource::Claude,
                    "codex" => ImportSource::Codex,
                    _ => return Err(fail()),
                });
            }
            "--service" | "--service-root" => {
                if service_root.is_some() {
                    return Err(fail());
                }
                check_len("import service root", value, 8192)?;
                service_root = Some(value.clone());
            }
            "--directory" => {
                if directory.is_some() {
                    return Err(fail());
                }
                check_len("import directory", value, 8192)?;
                directory = Some(value.clone());
            }
            _ => return Err(CommandError::UnknownOption(flag.clone())),
        }
    }
    let source = source.ok_or(CommandError::MissingValue("--from"))?;
    if kind == ImportKind::Hooks && source != ImportSource::Claude {
        return Err(CommandError::Import("hooks supports --from claude only"));
    }
    if kind == ImportKind::Mcp && service_root.is_none() {
        return Err(CommandError::MissingValue("--service"));
    }
    Ok(CliCommand::Import(ImportCommand {
        kind,
        source,
        service_root: service_root.map(PathBuf::from),
        directory,
        apply,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }
    #[test]
    fn import_mcp_parses_source_service_and_apply() -> Result<(), CommandError> {
        let CliCommand::Import(command) = CliCommand::parse(args(&[
            "import",
            "mcp",
            "--from",
            "claude",
            "--service",
            "/profile",
            "--directory",
            "/repo",
            "--apply",
        ]))?
        else {
            return Err(CommandError::Import(GRAMMAR));
        };
        assert_eq!(command.source, ImportSource::Claude);
        assert_eq!(command.directory.as_deref(), Some("/repo"));
        assert!(command.apply);
        let CliCommand::Import(dry) = CliCommand::parse(args(&[
            "import",
            "mcp",
            "--from",
            "codex",
            "--service",
            "/profile",
        ]))?
        else {
            return Err(CommandError::Import(GRAMMAR));
        };
        assert_eq!(dry.source, ImportSource::Codex);
        assert!(!dry.apply);
        assert_eq!(dry.directory, None);
        Ok(())
    }
    #[test]
    fn import_refuses_grammar_violations() {
        for values in [
            args(&["import", "mcp"]),
            args(&["import", "mcp", "--from", "claude"]),
            args(&["import", "mcp", "--service", "/profile"]),
            args(&["import", "mcp", "--from", "gemini", "--service", "/profile"]),
            args(&[
                "import",
                "mcp",
                "--from",
                "claude",
                "--from",
                "codex",
                "--service",
                "/profile",
            ]),
            args(&[
                "import",
                "mcp",
                "--from",
                "claude",
                "--service",
                "/a",
                "--service",
                "/b",
            ]),
            args(&[
                "import",
                "mcp",
                "--from",
                "claude",
                "--service",
                "/a",
                "--apply",
                "--apply",
            ]),
            args(&["import", "openai", "--from", "claude", "--service", "/a"]),
            args(&[
                "import",
                "mcp",
                "--from",
                "claude",
                "--service",
                "/a",
                "--unknown",
            ]),
            args(&["import", "mcp", "--from", "--service", "/a"]),
        ] {
            let rendered = format!("{values:?}");
            assert!(CliCommand::parse(values).is_err(), "{rendered}");
        }
    }
}
