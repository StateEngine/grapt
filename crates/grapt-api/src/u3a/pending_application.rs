//! The first successful connection installs the application exactly once.
use super::{
    CaptureApplicationPort, DogfoodApplicationPort, DogfoodEventHub, DogfoodRouteRegistrationError,
};
use crate::{ApiRefusal, ApiRouterBuilder, RouteMethod, RoutePath};
use axum::{body::Body, http::Request};
use std::sync::{Arc, OnceLock};

#[derive(Default)]
pub struct ApplicationSlot(OnceLock<ApplicationPorts>);

struct ApplicationPorts {
    application: Arc<dyn DogfoodApplicationPort>,
    capture: Arc<dyn CaptureApplicationPort>,
}

impl std::fmt::Debug for ApplicationSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApplicationSlot")
            .field("installed", &self.0.get().is_some())
            .finish()
    }
}

impl ApplicationSlot {
    pub fn ready(
        application: Arc<dyn DogfoodApplicationPort>,
        capture: Arc<dyn CaptureApplicationPort>,
    ) -> Self {
        Self(OnceLock::from(ApplicationPorts {
            application,
            capture,
        }))
    }
    pub fn install(
        &self,
        application: Arc<dyn DogfoodApplicationPort>,
        capture: Arc<dyn CaptureApplicationPort>,
    ) -> Result<(), &'static str> {
        self.0
            .set(ApplicationPorts {
                application,
                capture,
            })
            .map_err(|_| "application already installed")
    }

    pub fn application(&self) -> Option<Arc<dyn DogfoodApplicationPort>> {
        self.0.get().map(|ports| Arc::clone(&ports.application))
    }
}

pub fn register_pending_capture_event_route(
    builder: &mut ApiRouterBuilder,
    hub: Arc<DogfoodEventHub>,
    slot: Arc<ApplicationSlot>,
) -> Result<(), ApiRefusal> {
    builder.register(
        RouteMethod::Get,
        RoutePath::try_new(super::CAPTURE_EVENT_PATH)?,
        move |request: Request<Body>| {
            let hub = Arc::clone(&hub);
            let slot = Arc::clone(&slot);
            async move {
                match slot.0.get() {
                    Some(ports) => {
                        super::capture::capture_event_handler(
                            request,
                            hub,
                            Arc::clone(&ports.capture),
                        )
                        .await
                    }
                    None => super::routes::unavailable_response(
                        super::ApplicationUnavailable::try_new("provider-setup"),
                    ),
                }
            }
        },
    )
}

/// Provider-independent discovery remains usable during setup. An unavailable
/// execution application is never represented by an invented model/credential.
enum SetupResponse {
    Location,
    Project,
    Providers,
    Models,
    DefaultModel,
}

pub(super) async fn dispatch_setup(
    owner: Option<&dyn super::IntegrationApplicationPort>,
    principal: crate::SealedPrincipal,
    request: super::ApplicationRequest,
) -> axum::response::Response {
    use super::*;
    use axum::http::StatusCode;
    use grapt_schema::{Nullable, Required};
    let unavailable =
        || routes::unavailable_response(ApplicationUnavailable::try_new("provider-setup"));
    let Some(owner) = owner else {
        return unavailable();
    };
    let (query, kind) = match request {
        ApplicationRequest::LocationGet(query) => (Ok(query), SetupResponse::Location),
        ApplicationRequest::ProjectCurrent(query) => (Ok(query), SetupResponse::Project),
        ApplicationRequest::ProviderList(query) => {
            (location_query(query), SetupResponse::Providers)
        }
        ApplicationRequest::ModelList(query) => (location_query(query), SetupResponse::Models),
        ApplicationRequest::ModelDefault(query) => {
            (location_query(query), SetupResponse::DefaultModel)
        }
        _ => return unavailable(),
    };
    let Ok(query) = query else {
        return routes::invalid_request_response("invalid setup location");
    };
    let location = match owner.location(principal, query).await {
        Ok(location) => location,
        Err(error) => return routes::json(StatusCode::BAD_REQUEST, &error),
    };
    match kind {
        SetupResponse::Location => routes::json(StatusCode::OK, &location),
        SetupResponse::Project => routes::json(StatusCode::OK, location.project().get()),
        SetupResponse::Providers => match BoundedVec::try_new(Vec::new()) {
            Ok(rows) => routes::json(
                StatusCode::OK,
                &ProviderListResponseDto::new(Required::new(location), Required::new(rows)),
            ),
            Err(_) => unavailable(),
        },
        SetupResponse::Models => match BoundedVec::try_new(Vec::new()) {
            Ok(rows) => routes::json(
                StatusCode::OK,
                &ModelListResponseDto::new(Required::new(location), Required::new(rows)),
            ),
            Err(_) => unavailable(),
        },
        SetupResponse::DefaultModel => routes::json(
            StatusCode::OK,
            &ModelDefaultResponseDto::new(Required::new(location), Nullable::null()),
        ),
    }
}

fn location_query<T: serde::Serialize>(
    query: T,
) -> Result<super::ProjectCurrentLocationQueryDto, serde_json::Error> {
    serde_json::from_value(serde_json::to_value(query)?)
}

pub(super) struct RouteState {
    pub(super) application: ApplicationBinding,
}

pub(super) enum ApplicationBinding {
    Fixed(Arc<dyn DogfoodApplicationPort>),
    Pending(
        Arc<super::ApplicationSlot>,
        Option<Arc<dyn super::IntegrationApplicationPort>>,
    ),
}

impl core::fmt::Debug for RouteState {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RouteState")
            .field("application", &"<typed application port>")
            .finish()
    }
}

/// Keep ordinary routes registered while setup owns the first connection.
/// Integration routes have their own live owner and are registered once.
pub fn register_pending_dogfood_routes(
    builder: &mut ApiRouterBuilder,
    slot: Arc<super::ApplicationSlot>,
    integrations: Option<Arc<dyn super::IntegrationApplicationPort>>,
) -> Result<(), DogfoodRouteRegistrationError> {
    let integration_owned = integrations.is_some();
    super::routes::register_binding(
        builder,
        ApplicationBinding::Pending(slot, integrations.clone()),
        super::routes::Endpoint::ALL
            .iter()
            .copied()
            .filter(|endpoint| {
                !integration_owned || *endpoint != super::routes::Endpoint::IntegrationList
            }),
    )?;
    if let Some(integrations) = integrations {
        super::register_integration_routes(builder, integrations)?;
    }
    Ok(())
}
