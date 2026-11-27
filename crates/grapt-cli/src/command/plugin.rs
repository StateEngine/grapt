//! Local plugin and marketplace management grammar.
use super::{CliCommand, CommandError};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginAction {
    MarketplaceAdd(String),
    Install(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommand {
    pub action: PluginAction,
    pub service: Option<PathBuf>,
    pub directory: Option<String>,
}
pub(super) fn parse(args: &[String]) -> Result<CliCommand, CommandError> {
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h" | "help") {
        return Ok(CliCommand::Help);
    }
    let fail = || {
        CommandError::Plugin("plugin marketplace add <source> | plugin install <name@marketplace> [--scope user] [--service <root>] [--directory <path>]")
    };
    if args.len() > 10
        || args
            .iter()
            .any(|v| v.is_empty() || v.len() > 4096 || v.chars().any(char::is_control))
    {
        return Err(fail());
    }
    let (action, offset) = match args.first().map(String::as_str) {
        Some("marketplace") if args.get(1).is_some_and(|s| s == "add") => (
            PluginAction::MarketplaceAdd(args.get(2).ok_or_else(fail)?.clone()),
            3,
        ),
        Some("install") => (
            PluginAction::Install(args.get(1).ok_or_else(fail)?.clone()),
            2,
        ),
        _ => return Err(fail()),
    };
    let mut service = None;
    let mut directory = None;
    let mut scope = false;
    let mut rest = args[offset..].iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .filter(|v| !v.starts_with('-'))
            .ok_or_else(fail)?;
        match flag.as_str() {
            "--service" if service.is_none() => service = Some(PathBuf::from(value)),
            "--directory" if directory.is_none() => directory = Some(value.clone()),
            "--scope" if !scope && value == "user" => scope = true,
            _ => return Err(fail()),
        }
    }
    Ok(CliCommand::Plugin(PluginCommand {
        action,
        service,
        directory,
    }))
}

#[cfg(test)]
#[test]
fn plugin_grammar_keeps_user_scope_and_refuses_cleanup() {
    let args = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    assert!(parse(&args(&["marketplace", "add", "./market with spaces"])).is_ok());
    assert!(parse(&args(&["install", "review@local", "--scope", "user"])).is_ok());
    for command in [
        vec!["prune"],
        vec!["uninstall", "review@local"],
        vec!["install", "review@local", "--scope", "project"],
    ] {
        assert!(parse(&args(&command)).is_err());
    }
}
