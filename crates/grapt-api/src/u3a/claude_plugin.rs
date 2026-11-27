//! Explicit plugin installation. Source content is never echoed in diagnostics.
use super::BoundedString;
use serde::{Deserialize, Serialize};

/// Covers bounded remote acquisition plus immutable tree publication.
pub const CLAUDE_PLUGIN_ALLOWANCE: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ClaudePluginRequestDto {
    MarketplaceAdd { source: BoundedString<4096> },
    Install { plugin: BoundedString<257> },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudePluginResponseDto {
    pub message: BoundedString<4096>,
}

/// The installer is available before any provider execution application exists.
pub trait ClaudePluginApplicationPort: Send + Sync + 'static {
    fn install(
        &self,
        principal: crate::SealedPrincipal,
        query: super::ProjectCurrentLocationQueryDto,
        request: ClaudePluginRequestDto,
    ) -> super::ApplicationFuture<ClaudePluginResponseDto, super::InvalidRequestErrorDto>;
}

pub fn register_claude_plugin_route(
    builder: &mut crate::ApiRouterBuilder,
    owner: std::sync::Arc<dyn ClaudePluginApplicationPort>,
) -> Result<(), crate::ApiRefusal> {
    builder.register(
        crate::RouteMethod::Post,
        crate::RoutePath::try_new("/api/plugin/claude")?,
        move |request| handle(std::sync::Arc::clone(&owner), request),
    )
}
async fn handle(
    owner: std::sync::Arc<dyn ClaudePluginApplicationPort>,
    request: axum::http::Request<axum::body::Body>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let Some(principal) = request
        .extensions()
        .get::<crate::SealedPrincipal>()
        .cloned()
    else {
        return super::routes::unauthorized_response();
    };
    if !request
        .extensions()
        .get::<crate::admission::TransportPeer>()
        .is_some_and(|peer| peer.is_loopback())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let query = match super::query::mcp_catalog(request.uri().query()) {
        Ok(query) => query,
        Err(_) => return super::routes::invalid_request_response("invalid plugin location"),
    };
    let body = match super::routes::request_body::body::<ClaudePluginRequestDto>(request).await {
        Ok(body) => body,
        Err(error) => return error,
    };
    match owner.install(principal, query, body).await {
        Ok(value) => super::routes::json(StatusCode::OK, &value),
        Err(error) => super::routes::json(StatusCode::BAD_REQUEST, &error),
    }
}
#[cfg(test)]
mod tests;
