//! Explicit stdio command or remote URL registration.
use super::{check_len, CliCommand, CommandError, McpAction, McpCommand};
use std::collections::BTreeMap;

#[derive(Clone, PartialEq, Eq)]
pub struct McpAddConfig(String);
impl std::fmt::Debug for McpAddConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("McpAddConfig(<redacted>)")
    }
}
impl McpAddConfig {
    pub(crate) fn encoded(&self) -> &str {
        &self.0
    }
}

pub(super) fn parse(arguments: &[String]) -> Result<CliCommand, CommandError> {
    let fail =
        || CommandError::Mcp("mcp add requires a name, --service, and either --url or -- COMMAND");
    if arguments.len() > 264
        || arguments.iter().any(|value| {
            value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control)
        })
    {
        return Err(fail());
    }
    let name = super::server(arguments.get(1))?;
    let mut service_root = None;
    let mut directory = None;
    let mut url = None;
    let mut environment = BTreeMap::new();
    let mut command = Vec::new();
    let mut arguments = arguments[2..].iter();
    while let Some(flag) = arguments.next() {
        if flag == "--" {
            command.extend(arguments.cloned());
            if command.is_empty() || command.len() > 128 {
                return Err(fail());
            }
            break;
        }
        let value = arguments
            .next()
            .filter(|value| !value.starts_with('-'))
            .ok_or_else(fail)?;
        if flag == "--env" {
            let (key, value) = value.split_once('=').ok_or_else(fail)?;
            if key.is_empty()
                || key.len() > 256
                || environment.len() >= 64
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || environment
                    .insert(key.to_owned(), value.to_owned())
                    .is_some()
            {
                return Err(CommandError::Mcp(
                    "MCP environment names must be unique KEY=VALUE pairs",
                ));
            }
            continue;
        }
        let slot = match flag.as_str() {
            "--service" | "--service-root" => &mut service_root,
            "--directory" => &mut directory,
            "--url" => &mut url,
            _ => return Err(CommandError::UnknownOption(flag.clone())),
        };
        if slot.is_some() {
            return Err(CommandError::Mcp("an MCP option appeared more than once"));
        }
        *slot = Some(value.clone());
    }
    let config = match (url, command.is_empty()) {
        (Some(url), true) if environment.is_empty() => {
            let parsed = reqwest::Url::parse(&url).map_err(|_| fail())?;
            let loopback = parsed
                .host_str()
                .and_then(|host| {
                    host.trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .ok()
                })
                .is_some_and(|address| address.is_loopback());
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.scheme() == "http" && !loopback
                || !parsed.has_host()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.fragment().is_some()
            {
                return Err(CommandError::Mcp(
                    "MCP URL requires HTTPS or literal HTTP loopback, without user information or a fragment",
                ));
            }
            serde_json::json!({"type":"remote","url":url,"oauth":parsed.scheme() == "https"})
        }
        (None, false) => {
            serde_json::json!({"type":"local","command":command,"environment":environment})
        }
        _ => return Err(fail()),
    };
    let config = serde_json::to_string(&config).map_err(|_| fail())?;
    check_len("MCP configuration", &config, 64 * 1024)?;
    Ok(CliCommand::Mcp(McpCommand {
        server: Some(name),
        service_root: service_root
            .ok_or(CommandError::MissingValue("--service"))?
            .into(),
        directory,
        action: McpAction::Add(McpAddConfig(config)),
    }))
}
