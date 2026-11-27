//! Operator discovery and sign-in for configured MCP servers.
use super::{check_len, CliCommand, CommandError};
use std::path::PathBuf;
mod add;
pub use add::McpAddConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpAction {
    Add(McpAddConfig),
    Remove,
    List {
        json: bool,
    },
    Login {
        client_id: Option<String>,
        scope: Option<String>,
        no_browser: bool,
    },
    Logout,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCommand {
    pub server: Option<String>,
    pub service_root: PathBuf,
    pub directory: Option<String>,
    pub action: McpAction,
}

pub(super) fn parse(arguments: &[String]) -> Result<CliCommand, CommandError> {
    if arguments.first().is_some_and(|value| value == "add") {
        return add::parse(arguments);
    }
    if arguments.len() > 16
        || arguments.iter().any(|value| {
            value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control)
        })
    {
        return Err(CommandError::Mcp("MCP command arguments are invalid"));
    }
    let listing = arguments.first().is_some_and(|value| value == "list");
    let login = match arguments.first().map(String::as_str) {
        Some("login") => true,
        Some("logout" | "list" | "remove") => false,
        _ => {
            return Err(CommandError::Mcp(
                "mcp requires add, list, remove, login or logout",
            ))
        }
    };
    let server = if listing {
        None
    } else {
        Some(server(arguments.get(1))?)
    };
    let mut service_root = None;
    let mut directory = None;
    let mut client_id = None;
    let mut scope = None;
    let mut no_browser = false;
    let mut json = false;
    let mut values = arguments[if listing { 1 } else { 2 }..].iter();
    while let Some(flag) = values.next() {
        if flag == "--json" && listing && !json {
            json = true;
            continue;
        }
        if flag == "--no-browser" && login && !no_browser {
            no_browser = true;
            continue;
        }
        let slot = match flag.as_str() {
            "--service" | "--service-root" => &mut service_root,
            "--directory" => &mut directory,
            "--client-id" if login => &mut client_id,
            "--scope" if login => &mut scope,
            _ => return Err(CommandError::UnknownOption(flag.clone())),
        };
        if slot.is_some() {
            return Err(CommandError::Mcp("an MCP option appeared more than once"));
        }
        let value = values
            .next()
            .filter(|value| !value.starts_with('-'))
            .ok_or(CommandError::Mcp("an MCP option needs a value"))?;
        check_len(
            "MCP option",
            value,
            if flag == "--scope" { 4096 } else { 8192 },
        )?;
        *slot = Some(value.clone());
    }
    let service_root = service_root.ok_or(CommandError::MissingValue("--service"))?;
    Ok(CliCommand::Mcp(McpCommand {
        server,
        service_root: service_root.into(),
        directory,
        action: if listing {
            McpAction::List { json }
        } else if login {
            McpAction::Login {
                client_id,
                scope,
                no_browser,
            }
        } else if arguments.first().is_some_and(|value| value == "remove") {
            McpAction::Remove
        } else {
            McpAction::Logout
        },
    }))
}

fn server(value: Option<&String>) -> Result<String, CommandError> {
    let name = value
        .filter(|value| !value.starts_with('-'))
        .ok_or(CommandError::Mcp("MCP command requires a server name"))?;
    check_len("MCP server", name, 256)?;
    if matches!(name.as_str(), "." | "..") {
        return Err(CommandError::Mcp("MCP server name is not addressable"));
    }
    Ok(name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }
    #[test]
    fn mcp_login_and_logout_use_the_public_command_grammar(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let CliCommand::Mcp(command) = CliCommand::parse(args(&[
            "mcp",
            "login",
            "docs",
            "--service",
            "/profile",
            "--directory",
            "/repo",
            "--client-id",
            "public-client",
            "--scope",
            "read write",
            "--no-browser",
        ]))?
        else {
            return Err("not an MCP command".into());
        };
        assert_eq!(command.server.as_deref(), Some("docs"));
        assert_eq!(command.directory.as_deref(), Some("/repo"));
        assert!(matches!(
            command.action,
            McpAction::Login {
                no_browser: true,
                ..
            }
        ));
        assert!(matches!(
            CliCommand::parse(args(&[
                "mcp",
                "logout",
                "docs",
                "--service-root",
                "/profile"
            ]))?,
            CliCommand::Mcp(McpCommand {
                action: McpAction::Logout,
                ..
            })
        ));
        assert_eq!(
            CliCommand::parse(args(&["mcp", "--help"]))?,
            CliCommand::Help
        );
        Ok(())
    }
    #[test]
    fn mcp_list_selects_a_location_and_json_without_an_authentication_target(
    ) -> Result<(), CommandError> {
        let CliCommand::Mcp(command) = CliCommand::parse(args(&[
            "mcp",
            "list",
            "--service",
            "/profile",
            "--directory",
            "/repo",
            "--json",
        ]))?
        else {
            return Err(CommandError::Mcp("not an MCP command"));
        };
        assert_eq!(command.server, None);
        assert_eq!(command.directory.as_deref(), Some("/repo"));
        assert_eq!(command.action, McpAction::List { json: true });
        for values in [
            args(&["mcp", "list", "docs", "--service", "/a"]),
            args(&["mcp", "list", "--service", "/a", "--no-browser"]),
            args(&["mcp", "list", "--service", "/a", "--json", "--json"]),
        ] {
            assert!(CliCommand::parse(values).is_err());
        }
        Ok(())
    }

    #[test]
    fn mcp_auth_requires_a_profile_and_refuses_ambiguous_or_over_bound_arguments() {
        for values in [
            args(&["mcp", "login", "docs"]),
            args(&[
                "mcp",
                "login",
                "docs",
                "--service",
                "/a",
                "--service-root",
                "/b",
            ]),
            args(&[
                "mcp",
                "logout",
                "docs",
                "--service",
                "/a",
                "--scope",
                "read",
            ]),
            args(&["mcp", "login", ".", "--service", "/a"]),
            args(&["mcp", "login", "bad\nname", "--service", "/a"]),
            vec![
                "mcp".into(),
                "login".into(),
                "x".repeat(257),
                "--service".into(),
                "/a".into(),
            ],
            vec![
                "mcp".into(),
                "login".into(),
                "docs".into(),
                "--service".into(),
                "/a".into(),
                "--scope".into(),
                "x".repeat(4097),
            ],
        ] {
            assert!(CliCommand::parse(values).is_err());
        }
    }
}
