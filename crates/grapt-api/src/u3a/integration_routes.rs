//! Integration routes share the authenticated router and transport boundary.
use super::{
    ApplicationFuture, IntegrationAttemptIdDto, IntegrationGetResponseDto, IntegrationIdDto,
    IntegrationKeyConnectRequestDto, IntegrationListResponseDto,
    IntegrationOAuthCompleteRequestDto, IntegrationOAuthConnectRequestDto,
    IntegrationOAuthConnectResponseDto, IntegrationOAuthStatusResponseDto, InvalidRequestErrorDto,
    NoContent, ProjectCurrentLocationQueryDto,
};
use crate::{ApiRefusal, ApiRouterBuilder, RouteMethod, RoutePath, SealedPrincipal};
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

const CONNECTION_HEADER: &str = "x-grapt-connection-id";
/// The pre-rename spelling an older CLI still sends.
const LEGACY_CONNECTION_HEADER: &str = "x-secode-connection-id"; // grapt-rename: keep

pub const INTEGRATION_ROUTE_COUNT: usize = 15;

pub trait IntegrationApplicationPort: Send + Sync + 'static {
    fn authentication_status(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::AuthenticationStatusResponseDto, InvalidRequestErrorDto>;
    fn connect_environment(
        &self,
        principal: SealedPrincipal,
        integration: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: super::IntegrationEnvironmentConnectRequestDto,
        connection: Option<super::DtoString>,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto>;
    fn oauth_challenge(
        &self,
        principal: SealedPrincipal,
        integration: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<super::IntegrationOAuthChallengeResponseDto, InvalidRequestErrorDto>;
    fn connect_command(
        &self,
        principal: SealedPrincipal,
        integration: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: super::IntegrationCommandConnectRequestDto,
    ) -> ApplicationFuture<super::IntegrationCommandConnectResponseDto, InvalidRequestErrorDto>;
    fn command_status(
        &self,
        principal: SealedPrincipal,
        integration: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<super::IntegrationCommandStatusResponseDto, InvalidRequestErrorDto>;
    fn cancel_command(
        &self,
        principal: SealedPrincipal,
        integration: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto>;
    fn remove_credential(
        &self,
        principal: SealedPrincipal,
        credential: super::CredentialIdDto,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto>;

    fn update_credential(
        &self,
        principal: SealedPrincipal,
        credential: super::CredentialIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: super::CredentialUpdateRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto>;

    fn location(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<super::LocationInfoDto, InvalidRequestErrorDto>;
    fn list(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<IntegrationListResponseDto, InvalidRequestErrorDto>;
    fn get(
        &self,
        principal: SealedPrincipal,
        integration: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<IntegrationGetResponseDto, InvalidRequestErrorDto>;
    fn connect_key(
        &self,
        principal: SealedPrincipal,
        integration: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: IntegrationKeyConnectRequestDto,
        connection: Option<super::DtoString>,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto>;
    fn connect_oauth(
        &self,
        principal: SealedPrincipal,
        integration: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: IntegrationOAuthConnectRequestDto,
        connection: Option<super::DtoString>,
    ) -> ApplicationFuture<IntegrationOAuthConnectResponseDto, InvalidRequestErrorDto>;
    fn oauth_status(
        &self,
        principal: SealedPrincipal,
        integration: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<IntegrationOAuthStatusResponseDto, InvalidRequestErrorDto>;
    fn complete_oauth(
        &self,
        principal: SealedPrincipal,
        integration: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
        request: IntegrationOAuthCompleteRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto>;
    fn cancel_oauth(
        &self,
        principal: SealedPrincipal,
        integration: IntegrationIdDto,
        query: ProjectCurrentLocationQueryDto,
        attempt: IntegrationAttemptIdDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto>;
}

pub fn register_integration_routes(
    builder: &mut ApiRouterBuilder,
    owner: Arc<dyn IntegrationApplicationPort>,
) -> Result<(), ApiRefusal> {
    for method in [RouteMethod::Patch, RouteMethod::Delete] {
        let credentials = Arc::clone(&owner);
        builder.register(
            method,
            RoutePath::try_new("/api/credential/{credentialID}")?,
            move |request: Request<Body>| {
                let owner = Arc::clone(&credentials);
                async move { credential(owner, request).await }
            },
        )?;
    }
    for (method, path, operation) in [
        (
            RouteMethod::Get,
            "/api/auth/status",
            Operation::AuthenticationStatus,
        ),
        (
            RouteMethod::Post,
            "/api/integration/{integrationID}/connect/env",
            Operation::Environment,
        ),
        (
            RouteMethod::Get,
            "/api/integration/{integrationID}/connect/oauth/{attemptID}/challenge",
            Operation::OAuthChallenge,
        ),
        (
            RouteMethod::Post,
            "/api/integration/{integrationID}/connect/command",
            Operation::Command,
        ),
        (
            RouteMethod::Get,
            "/api/integration/{integrationID}/connect/command/{attemptID}",
            Operation::CommandStatus,
        ),
        (
            RouteMethod::Delete,
            "/api/integration/{integrationID}/connect/command/{attemptID}",
            Operation::CommandCancel,
        ),
        (RouteMethod::Get, "/api/integration", Operation::List),
        (
            RouteMethod::Get,
            "/api/integration/{integrationID}",
            Operation::Get,
        ),
        (
            RouteMethod::Post,
            "/api/integration/{integrationID}/connect/key",
            Operation::Key,
        ),
        (
            RouteMethod::Post,
            "/api/integration/{integrationID}/connect/oauth",
            Operation::OAuth,
        ),
        (
            RouteMethod::Get,
            "/api/integration/{integrationID}/connect/oauth/{attemptID}",
            Operation::OAuthStatus,
        ),
        (
            RouteMethod::Post,
            "/api/integration/{integrationID}/connect/oauth/{attemptID}/complete",
            Operation::OAuthComplete,
        ),
        (
            RouteMethod::Delete,
            "/api/integration/{integrationID}/connect/oauth/{attemptID}",
            Operation::OAuthCancel,
        ),
    ] {
        let owner = Arc::clone(&owner);
        builder.register(
            method,
            RoutePath::try_new(path)?,
            move |request: Request<Body>| {
                let owner = Arc::clone(&owner);
                async move { handle(owner, operation, request).await }
            },
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Operation {
    AuthenticationStatus,
    Environment,
    OAuthChallenge,
    Command,
    CommandStatus,
    CommandCancel,
    List,
    Get,
    Key,
    OAuth,
    OAuthStatus,
    OAuthComplete,
    OAuthCancel,
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn handle(
    owner: Arc<dyn IntegrationApplicationPort>,
    operation: Operation,
    request: Request<Body>,
) -> Response {
    let Some(principal) = request.extensions().get::<SealedPrincipal>().cloned() else {
        return super::routes::unauthorized_response();
    };
    if !matches!(operation, Operation::List | Operation::Get)
        && !request
            .extensions()
            .get::<crate::admission::TransportPeer>()
            .is_some_and(|peer| peer.is_loopback())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let query = match super::query::project_current(request.uri().query()) {
        Ok(query) => query,
        Err(_) => return super::routes::invalid_request_response("invalid integration location"),
    };
    if matches!(operation, Operation::AuthenticationStatus) {
        return response(owner.authentication_status(principal, query).await);
    }
    if matches!(operation, Operation::List) {
        return response(owner.list(principal, query).await);
    }
    let id = request
        .uri()
        .path()
        .strip_prefix("/api/integration/")
        .and_then(|path| path.split('/').next())
        .and_then(|id| super::query::percent_decode(id).ok())
        .and_then(|id| IntegrationIdDto::try_new(id).ok());
    let Some(id) = id else {
        return super::routes::invalid_request_response("invalid integration identity");
    };
    if matches!(operation, Operation::Get) {
        return response(owner.get(principal, id, query).await);
    }
    if matches!(operation, Operation::Command) {
        let body = match super::routes::request_body::body::<
            super::IntegrationCommandConnectRequestDto,
        >(request)
        .await
        {
            Ok(body) => body,
            Err(response) => return response,
        };
        return response(owner.connect_command(principal, id, query, body).await);
    }
    if matches!(
        operation,
        Operation::CommandStatus | Operation::CommandCancel
    ) {
        let attempt = request
            .uri()
            .path()
            .split('/')
            .nth(6)
            .and_then(|raw| super::query::percent_decode(raw).ok())
            .and_then(|raw| super::DtoString::try_new(raw).ok())
            .map(IntegrationAttemptIdDto::new);
        let Some(attempt) = attempt else {
            return super::routes::invalid_request_response("invalid command attempt identity");
        };
        return if matches!(operation, Operation::CommandStatus) {
            response(owner.command_status(principal, id, query, attempt).await)
        } else {
            no_content(owner.cancel_command(principal, id, query, attempt).await)
        };
    }
    let connection = if matches!(
        operation,
        Operation::Environment | Operation::Key | Operation::OAuth
    ) {
        // A pre-rename CLI still names its target with the legacy header.
        let mut named = None;
        for name in [CONNECTION_HEADER, LEGACY_CONNECTION_HEADER] {
            let values = request.headers().get_all(name);
            let mut values = values.iter();
            let first = values.next();
            if values.next().is_some() {
                return super::routes::invalid_request_response("duplicate connection identity");
            }
            let Some(value) = first else {
                continue;
            };
            let Some(value) = value
                .to_str()
                .ok()
                .and_then(|value| super::connection_target(value).ok())
            else {
                return super::routes::invalid_request_response("invalid connection identity");
            };
            match &named {
                Some(earlier) if *earlier != value => {
                    return super::routes::invalid_request_response(
                        "conflicting connection identity",
                    )
                }
                Some(_) => {}
                None => named = Some(value),
            }
        }
        named
    } else {
        None
    };
    if matches!(operation, Operation::Environment) {
        let body = match super::routes::request_body::body::<
            super::IntegrationEnvironmentConnectRequestDto,
        >(request)
        .await
        {
            Ok(body) => body,
            Err(response) => return response,
        };
        return no_content(
            owner
                .connect_environment(principal, id, query, body, connection)
                .await,
        );
    }
    if matches!(operation, Operation::OAuth) {
        let body =
            match super::routes::request_body::body::<IntegrationOAuthConnectRequestDto>(request)
                .await
            {
                Ok(body) => body,
                Err(response) => return response,
            };
        return response(
            owner
                .connect_oauth(principal, id, query, body, connection)
                .await,
        );
    }
    if matches!(
        operation,
        Operation::OAuthStatus
            | Operation::OAuthComplete
            | Operation::OAuthCancel
            | Operation::OAuthChallenge
    ) {
        let attempt = request
            .uri()
            .path()
            .split('/')
            .nth(6)
            .and_then(|raw| super::query::percent_decode(raw).ok())
            .and_then(|raw| super::DtoString::try_new(raw).ok())
            .map(IntegrationAttemptIdDto::new);
        let Some(attempt) = attempt else {
            return super::routes::invalid_request_response("invalid OAuth attempt identity");
        };
        if matches!(operation, Operation::OAuthChallenge) {
            return response(owner.oauth_challenge(principal, id, query, attempt).await);
        }
        if matches!(operation, Operation::OAuthStatus) {
            return response(owner.oauth_status(principal, id, query, attempt).await);
        }
        let outcome = if matches!(operation, Operation::OAuthCancel) {
            owner.cancel_oauth(principal, id, query, attempt).await
        } else {
            let body =
                match super::routes::request_body::body::<IntegrationOAuthCompleteRequestDto>(
                    request,
                )
                .await
                {
                    Ok(body) => body,
                    Err(response) => return response,
                };
            owner
                .complete_oauth(principal, id, query, attempt, body)
                .await
        };
        return no_content(outcome);
    }
    let body =
        match super::routes::request_body::body::<IntegrationKeyConnectRequestDto>(request).await {
            Ok(body) => body,
            Err(response) => return response,
        };
    no_content(
        owner
            .connect_key(principal, id, query, body, connection)
            .await,
    )
}

fn no_content(result: Result<NoContent, InvalidRequestErrorDto>) -> Response {
    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => super::routes::json(StatusCode::BAD_REQUEST, &error),
    }
}

fn response<T: serde::Serialize>(result: Result<T, InvalidRequestErrorDto>) -> Response {
    match result {
        Ok(value) => super::routes::json(StatusCode::OK, &value),
        Err(error) => super::routes::json(StatusCode::BAD_REQUEST, &error),
    }
}

#[cfg(test)]
#[path = "integration_routes_tests.rs"]
mod tests;

// Use the accepted transport's peer before decoding any submitted body.
async fn credential(
    owner: Arc<dyn IntegrationApplicationPort>,
    request: Request<Body>,
) -> Response {
    let Some(principal) = request.extensions().get::<SealedPrincipal>().cloned() else {
        return super::routes::unauthorized_response();
    };
    if !request
        .extensions()
        .get::<crate::admission::TransportPeer>()
        .is_some_and(|peer| peer.is_loopback())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let query = match super::query::project_current(request.uri().query()) {
        Ok(query) => query,
        Err(_) => return super::routes::invalid_request_response("invalid credential location"),
    };
    let id = request
        .uri()
        .path()
        .strip_prefix("/api/credential/")
        .and_then(|raw| super::query::percent_decode(raw).ok())
        .and_then(|raw| super::CredentialIdDto::try_new(raw).ok());
    let Some(id) = id else {
        return super::routes::invalid_request_response("invalid credential identity");
    };
    if request.method() == axum::http::Method::DELETE {
        return no_content(owner.remove_credential(principal, id, query).await);
    }
    let body = match super::routes::request_body::body::<super::CredentialUpdateRequestDto>(request)
        .await
    {
        Ok(body) => body,
        Err(response) => return response,
    };
    no_content(owner.update_credential(principal, id, query, body).await)
}
