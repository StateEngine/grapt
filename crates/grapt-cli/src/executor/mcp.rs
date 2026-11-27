//! MCP sign-in stays in the public client; token custody stays in the product.
use crate::command::mcp::{McpAction, McpCommand};
use crate::{ApiClient, CliLimits, ExitClass, ServiceDiscovery};
use grapt_api::u3a::{BoundedString, McpAuthRequestDto, McpAuthState};
use grapt_types::Secret;
use std::time::Duration;

type Failure = (ExitClass, String);

pub(super) async fn execute(command: McpCommand, limits: CliLimits) -> u8 {
    let result = tokio::select! {
        result = run(command, limits) => result,
        _signal = super::termination_signal() => return ExitClass::Cancelled.code(),
    };
    match result {
        Ok(()) => ExitClass::Success.code(),
        Err((class, message)) => {
            let emitted = tokio::select! {
                result = super::emit(format!("grapt: {message}"), true, limits) => result,
                _signal = super::termination_signal() => return ExitClass::Cancelled.code(),
            };
            if emitted.is_ok() {
                class.code()
            } else {
                ExitClass::Output.code()
            }
        }
    }
}

pub(super) fn authorization_url(value: &str) -> Result<(), Failure> {
    let refused = || (ExitClass::Protocol, "MCP sign-in URL is invalid".to_owned());
    if value.len() > 32768 || value.trim() != value || value.chars().any(char::is_control) {
        return Err(refused());
    }
    let url = reqwest::Url::parse(value).map_err(|_| refused())?;
    if url.scheme() != "https"
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(refused());
    }
    Ok(())
}

pub(super) async fn browser(url: &Secret<String>) {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = tokio::process::Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = tokio::process::Command::new("open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = tokio::process::Command::new("xdg-open");
    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        command
            .arg(url.expose())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await;
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn run(command: McpCommand, limits: CliLimits) -> Result<(), Failure> {
    let McpCommand {
        server,
        service_root,
        directory,
        action,
    } = command;
    let discovery = ServiceDiscovery::try_new(service_root, env!("CARGO_PKG_VERSION"))
        .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
    let service = discovery
        .discover()
        .await
        .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
    let expected = service
        .product_health()
        .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
    let endpoint = service.endpoint().to_owned();
    let client = ApiClient::from_discovered(&service, limits)
        .map_err(|error| (ExitClass::Protocol, error.to_string()))?;
    if let McpAction::List { json } = action {
        let response = client
            .mcp_list(directory.as_deref())
            .await
            .map_err(|error| {
                (
                    error.exit_class(),
                    error.to_string().escape_debug().to_string(),
                )
            })?;
        let message = if json {
            serde_json::to_string_pretty(&response)
                .map_err(|error| (ExitClass::Protocol, error.to_string()))?
        } else {
            list_text(&response)
        };
        return super::emit(message, false, limits)
            .await
            .map_err(|error| (ExitClass::Output, error));
    }
    let server = server.ok_or((
        ExitClass::Usage,
        "MCP authentication requires a server name".to_owned(),
    ))?;
    let configuration = match &action {
        McpAction::Add(config) => Some(grapt_api::u3a::McpConfigurationRequestDto::Add {
            config: serde_json::from_str(config.encoded())
                .map_err(|_| (ExitClass::Usage, "MCP configuration is invalid".to_owned()))?,
        }),
        McpAction::Remove => Some(grapt_api::u3a::McpConfigurationRequestDto::Remove {}),
        _ => None,
    };
    if let Some(configuration) = configuration {
        client
            .mcp_configuration(&server, directory.as_deref(), &configuration)
            .await
            .map_err(|error| {
                (
                    error.exit_class(),
                    error.to_string().escape_debug().to_string(),
                )
            })?;
        return super::emit(
            format!("Updated MCP server {server} in user configuration."),
            false,
            limits,
        )
        .await
        .map_err(|error| (ExitClass::Output, error));
    }
    let McpAction::Login {
        client_id,
        scope,
        no_browser,
    } = action
    else {
        client
            .mcp_auth(&server, directory.as_deref(), &McpAuthRequestDto::Logout {})
            .await
            .map_err(super::remote)?;
        return super::emit(format!("Signed out of {server}."), false, limits)
            .await
            .map_err(|error| (ExitClass::Output, error));
    };
    let request = McpAuthRequestDto::Login {
        client_id: client_id
            .map(BoundedString::<8192>::try_new)
            .transpose()
            .map_err(|error| (ExitClass::Usage, error.to_owned()))?,
        scope: scope
            .map(BoundedString::<4096>::try_new)
            .transpose()
            .map_err(|error| (ExitClass::Usage, error.to_owned()))?,
    };
    let response = client
        .mcp_auth(&server, directory.as_deref(), &request)
        .await
        .map_err(super::remote)?;
    let login_id = response.login_id.ok_or((
        ExitClass::Protocol,
        "MCP sign-in response has no attempt identity".to_owned(),
    ))?;
    let url = response.authorization_url.ok_or((
        ExitClass::Protocol,
        "MCP sign-in response has no authorization URL".to_owned(),
    ))?;
    authorization_url(url.expose())?;
    super::emit(
        format!("Open this URL to sign in to {server}:\n{}", url.expose()),
        false,
        limits,
    )
    .await
    .map_err(|error| (ExitClass::Output, error))?;
    if !no_browser {
        browser(&url).await;
    }
    let polling = async {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let current = discovery
                .discover()
                .await
                .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
            if current.endpoint() != endpoint
                || current
                    .product_health()
                    .map_err(|error| (ExitClass::Discovery, error.to_string()))?
                    != expected
            {
                return Err((
                    ExitClass::Discovery,
                    "product identity changed; retry the command".to_owned(),
                ));
            }
            let response = client
                .mcp_auth(
                    &server,
                    directory.as_deref(),
                    &McpAuthRequestDto::Status {
                        login_id: login_id.clone(),
                    },
                )
                .await
                .map_err(super::remote)?;
            match response.state {
                McpAuthState::AwaitingBrowser => {}
                McpAuthState::SignedIn => return Ok(()),
                _ => {
                    return Err((
                        ExitClass::Remote,
                        "MCP sign-in failed or was canceled; retry the command".to_owned(),
                    ))
                }
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(180), polling)
        .await
        .map_err(|_| (ExitClass::Remote, "MCP sign-in timed out".to_owned()))??;
    super::emit(format!("Signed in to {server}."), false, limits)
        .await
        .map_err(|error| (ExitClass::Output, error))
}

fn list_text(response: &grapt_api::u3a::McpListResponseDto) -> String {
    use grapt_api::u3a::McpStatusDto;
    let servers = response.data().get().as_slice();
    if servers.is_empty() {
        return "No MCP servers configured.".to_owned();
    }
    servers
        .iter()
        .map(|server| {
            let (status, error) = match server.status().get() {
                McpStatusDto::Connected(_) => ("connected", None),
                McpStatusDto::Pending(_) => ("pending", None),
                McpStatusDto::Disabled(_) => ("disabled", None),
                McpStatusDto::NeedsAuth(_) => ("needs_auth", None),
                McpStatusDto::Failed(value) => ("failed", Some(value.error().get().as_str())),
                McpStatusDto::NeedsRegistration(value) => (
                    "needs_client_registration",
                    Some(value.error().get().as_str()),
                ),
            };
            let name = server.name().get().as_str().escape_debug();
            match error {
                Some(error) => format!("{name}\t{status}: {}", error.escape_debug()),
                None => format!("{name}\t{status}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mcp_browser_destinations_are_https_and_have_no_terminal_controls_or_embedded_credentials() {
        assert!(authorization_url("https://auth.example/authorize?state=private").is_ok());
        for invalid in [
            "file:///tmp/key",
            "javascript:run()",
            "http://auth.example/",
            "https://user:password@auth.example/",
            "https://auth.example/#code",
            "https://auth.example/\u{001b}payload",
        ] {
            assert!(authorization_url(invalid).is_err());
        }
    }
}
