use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use grapt_api::{
    register_dogfood_event_route, register_product_health, register_product_health_with_reconciles,
    ApiHostLimits, ApiRouterBuilder, AuthenticatedApiService, AuthenticationPolicyVersion,
    HeaderReadAllowance, MaxConnections, MaxHeaders, MaxInflightRequests, MaxInflightStreams,
    MaxPrincipalMappings, MaxRequestBodyBytes, MaxRequestHeadBytes, MaxRequestsPerConnection,
    MaxRouterModules, ProductHealth, RunningApiHost, SeatDirectoryEntry, SeatPrincipalDirectory,
    SeatTrustBoundary, ShutdownDrainAllowance, StreamWriteAllowance, SubjectId,
    DOGFOOD_NON_SSE_ROUTE_COUNT,
};
use grapt_auth_basic::BasicAuthVerifier;
use grapt_location::{
    BindHost, BindLimits, BindPort, ListenerBindPlan, ListenerBinder, LiteralAddressResolver,
    MaxBindAttempts, MaxResolvedAddresses,
};
use grapt_service::StopOutcome;
use grapt_store::{OsRandomIds, SystemClock};
use grapt_types::authority::DeterministicAuthorities;
use grapt_types::service::ProcessId;
use grapt_types::SeatRecipientId;

use crate::compose::{ProductRefusal, ServerComposition};
use crate::input::{Credential, ServiceConfig};
pub(crate) fn registration_was_released(outcome: &StopOutcome) -> bool {
    matches!(outcome, StopOutcome::Released { .. })
}

pub(crate) async fn stop_and_refuse<T>(
    host: RunningApiHost,
    refusal: ProductRefusal,
) -> Result<T, ProductRefusal> {
    let _exit = host.stop().await;
    Err(refusal)
}

pub(crate) async fn bind_loopback() -> Result<grapt_location::BoundListener, ProductRefusal> {
    let addresses =
        MaxResolvedAddresses::new(1).map_err(|error| ProductRefusal::Bind(error.to_string()))?;
    let attempts =
        MaxBindAttempts::new(3).map_err(|error| ProductRefusal::Bind(error.to_string()))?;
    let plan = ListenerBindPlan::new(
        BindHost::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        BindPort::Ephemeral,
        BindLimits::new(addresses, attempts),
    );
    ListenerBinder::new(Arc::new(LiteralAddressResolver))
        .bind(&plan)
        .await
        .map_err(|error| ProductRefusal::Bind(error.to_string()))
}

pub(crate) fn product_service(
    limits: &ApiHostLimits,
    credential: &Credential,
    pid: ProcessId,
    config: &ServiceConfig,
    dogfood: Option<ServerComposition>,
) -> Result<AuthenticatedApiService, ProductRefusal> {
    product_service_mode(limits, credential, pid, config, dogfood, false)
}

pub(crate) fn ephemeral_service(
    limits: &ApiHostLimits,
    credential: &Credential,
    pid: ProcessId,
    config: &ServiceConfig,
    dogfood: ServerComposition,
) -> Result<AuthenticatedApiService, ProductRefusal> {
    product_service_mode(limits, credential, pid, config, Some(dogfood), true)
}

fn product_service_mode(
    limits: &ApiHostLimits,
    credential: &Credential,
    pid: ProcessId,
    config: &ServiceConfig,
    dogfood: Option<ServerComposition>,
    ephemeral: bool,
) -> Result<AuthenticatedApiService, ProductRefusal> {
    let clock = SystemClock;
    let ids = OsRandomIds::seed().map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let subject = SubjectId::try_new(credential.username.clone())
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let recipient = id(
        SeatRecipientId::new(config.instance.clone()),
        "health recipient",
    )?;
    let mappings = MaxPrincipalMappings::try_new(1)
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let directory = SeatPrincipalDirectory::configure(
        vec![SeatDirectoryEntry::new(subject, recipient)],
        mappings,
        &authorities,
    )
    .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let policy = AuthenticationPolicyVersion::try_new("product-basic-v1")
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let verifier = BasicAuthVerifier::new(&credential.username, &credential.secret)
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let boundary = Arc::new(SeatTrustBoundary::with_injected_basic_verifier(
        policy, directory, verifier,
    ));
    let module_count = if let Some(dogfood) = &dogfood {
        2 + DOGFOOD_NON_SSE_ROUTE_COUNT
            + usize::from(!ephemeral && dogfood.installer.is_some())
            + 2
            + if dogfood.integrations.is_some() {
                grapt_api::u3a::INTEGRATION_ROUTE_COUNT - 1
            } else {
                0
            }
    } else {
        1
    };
    let modules = MaxRouterModules::try_new(module_count)
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let mut builder = ApiRouterBuilder::new(modules);
    let health = ProductHealth::new(
        pid.value(),
        config.build.clone(),
        config.environment.clone(),
        config.instance.clone(),
    )
    .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    match dogfood.as_ref() {
        Some(dogfood) => register_product_health_with_reconciles(
            &mut builder,
            health,
            Arc::clone(&dogfood.hub),
            env!("CARGO_PKG_VERSION"),
        ),
        None => register_product_health(&mut builder, health, env!("CARGO_PKG_VERSION")),
    }
    .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    if let Some(dogfood) = dogfood {
        if !ephemeral {
            if let Some(installer) = dogfood.installer {
                grapt_api::u3a::register_claude_plugin_route(&mut builder, installer)
                    .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
            }
        }
        if ephemeral {
            let application = dogfood.ports.application().ok_or_else(|| {
                ProductRefusal::Compose("ephemeral application is unavailable".into())
            })?;
            grapt_api::register_ephemeral_dogfood_routes(&mut builder, application)
                .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
        } else {
            grapt_api::u3a::register_pending_dogfood_routes(
                &mut builder,
                Arc::clone(&dogfood.ports),
                dogfood.integrations,
            )
            .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
        }
        let hub = dogfood.hub;
        register_dogfood_event_route(&mut builder, Arc::clone(&hub))
            .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
        grapt_api::u3a::register_pending_capture_event_route(&mut builder, hub, dogfood.ports)
            .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    }
    Ok(AuthenticatedApiService::with_inline_image_prompts(
        builder.seal(),
        boundary,
        limits.max_request_body_bytes,
    ))
}

pub(crate) fn host_limits() -> Result<ApiHostLimits, ProductRefusal> {
    let invalid = |error: grapt_api::ApiHostConfigError| ProductRefusal::Compose(error.to_string());
    Ok(ApiHostLimits {
        max_connections: MaxConnections::try_new(64).map_err(invalid)?,
        max_requests_per_connection: MaxRequestsPerConnection::try_new(100).map_err(invalid)?,
        max_inflight_requests: MaxInflightRequests::try_new(32).map_err(invalid)?,
        // A renderer and its host capture use two streams per managed profile.
        // Leave room for additional windows and diagnostic clients.
        max_inflight_streams: MaxInflightStreams::try_new(16).map_err(invalid)?,
        max_request_head_bytes: MaxRequestHeadBytes::try_new(16 * 1_024).map_err(invalid)?,
        max_headers: MaxHeaders::try_new(64).map_err(invalid)?,
        max_request_body_bytes: MaxRequestBodyBytes::try_new(64 * 1_024).map_err(invalid)?,
        header_read_allowance: HeaderReadAllowance::try_new(Duration::from_secs(5))
            .map_err(invalid)?,
        shutdown_drain_allowance: ShutdownDrainAllowance::try_new(Duration::from_secs(5))
            .map_err(invalid)?,
        stream_write_allowance: StreamWriteAllowance::try_new(Duration::from_secs(30))
            .map_err(invalid)?,
    })
}

pub(crate) fn configured_host_limits(
    config: &ServiceConfig,
) -> Result<ApiHostLimits, ProductRefusal> {
    let mut limits = host_limits()?;
    if let Some(value) = config.max_inflight_streams {
        limits.max_inflight_streams = MaxInflightStreams::try_new(value)
            .map_err(|error| ProductRefusal::Input(error.to_string()))?;
    }
    limits
        .validate()
        .map_err(|error| ProductRefusal::Input(error.to_string()))?;
    Ok(limits)
}

pub(crate) fn id<T>(value: Option<T>, label: &str) -> Result<T, ProductRefusal> {
    value.ok_or_else(|| ProductRefusal::Input(format!("{label} is empty")))
}

pub(crate) async fn drain_host(host: RunningApiHost) -> grapt_api::ApiHostExit {
    host.stop().await
}
