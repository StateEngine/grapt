//! The operator's authenticated MCP sign-in endpoint. It resolves the same
//! user-owned declarations as tool publication; request bodies cannot supply
//! an arbitrary endpoint, token, or credential-store location.
use super::{auth::LoginState, Declaration};
use crate::dogfood::{
    connections::McpSnapshot, session_config, support::invalid, DogfoodApplication,
};
use grapt_api::u3a::{
    ApplicationFuture, InvalidRequestErrorDto, McpAuthRequestDto, McpAuthResponseDto, McpAuthState,
    ProjectCurrentLocationQueryDto, WireId,
};
use grapt_types::{IdAuthority, McpLoginId};
use std::sync::Arc;

pub(crate) fn respond(
    application: DogfoodApplication,
    server: String,
    query: ProjectCurrentLocationQueryDto,
    request: McpAuthRequestDto,
) -> ApplicationFuture<McpAuthResponseDto, InvalidRequestErrorDto> {
    Box::pin(async move {
        let selection = session_config::catalog::select(&application, &query)
            .await
            .map_err(invalid)?;
        let snapshot = McpSnapshot::read(&selection.sources, &selection.location.directory)
            .map_err(invalid)?;
        let Some(Declaration::Remote {
            url,
            headers,
            oauth: true,
            ..
        }) = snapshot.declared.get(&server)
        else {
            return Err(invalid(
                "MCP sign-in requires a user-owned remote server with oauth: true",
            ));
        };
        if headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("authorization"))
        {
            return Err(invalid(
                "remove the static Authorization header before enabling MCP OAuth",
            ));
        }
        let resource = url::Url::parse(url)
            .map_err(|_| invalid("invalid MCP endpoint"))?
            .to_string();
        let auth = application
            .mcp
            .auth
            .as_ref()
            .ok_or_else(|| invalid("MCP authentication is unavailable"))?;
        let response = match request {
            McpAuthRequestDto::Login { client_id, scope } => {
                let id: McpLoginId = format!("mcl_{}", application.ids.mint().as_str())
                    .parse()
                    .map_err(|_| invalid("could not create sign-in identity"))?;
                let url = auth
                    .login(
                        server,
                        resource,
                        id.clone(),
                        client_id.map(|v| v.into_string()),
                        scope.map(|v| v.into_string()),
                        Arc::downgrade(&application.mcp),
                    )
                    .await
                    .map_err(invalid)?;
                McpAuthResponseDto {
                    login_id: WireId::parse(id.as_str().to_owned()),
                    state: McpAuthState::AwaitingBrowser,
                    authorization_url: Some(url),
                }
            }
            McpAuthRequestDto::Status { login_id } => {
                let state = match auth
                    .status(&server, &resource, login_id.as_inner())
                    .map_err(invalid)?
                {
                    LoginState::Pending => McpAuthState::AwaitingBrowser,
                    LoginState::Complete => McpAuthState::SignedIn,
                    LoginState::Failed => McpAuthState::Failed,
                };
                McpAuthResponseDto {
                    login_id: Some(login_id),
                    state,
                    authorization_url: None,
                }
            }
            McpAuthRequestDto::Logout {} => {
                auth.logout(&server, &resource).await.map_err(invalid)?;
                application.mcp.retire_server(&server, &resource).await;
                McpAuthResponseDto {
                    login_id: None,
                    state: McpAuthState::SignedOut,
                    authorization_url: None,
                }
            }
        };
        selection.validate_current(&application).map_err(invalid)?;
        Ok(response)
    })
}
