use std::path::Path;
use std::sync::Arc;

use grapt_api::{
    CaptureApplicationPort, DogfoodApplicationPort, DogfoodEventHub, PreparedApiHost,
    TokioDeadlineDriver,
};
use grapt_service::{AuthenticatedHealthProbe, HealthExpectation, ServiceController};
use grapt_store::SystemClock;
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceRegistrationVersion,
    ServiceSlotId,
};
use thiserror::Error;

use crate::compose_host::{
    bind_loopback, drain_host, id, product_service, registration_was_released, stop_and_refuse,
};
use crate::fixture_seam::{self, HoldPoint};
use crate::input::{read_config, read_credential, Credential, ServiceConfig};
use crate::ready;
use crate::receipt::LaunchReceipt;
use crate::startup::Admission;

mod composition;
mod endpoint;
mod setup;
pub(crate) use composition::DogfoodComposition;
mod driver;
pub(crate) use driver::ServiceDriver;
pub(crate) mod ephemeral;
pub(crate) mod external_host;
pub(crate) mod renewal;
mod terminal;
pub use terminal::run_service;
use terminal::Lifetime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostMode {
    Standalone,
    External,
}

#[derive(Debug, Error)]
pub enum ProductRefusal {
    #[error("platform bootstrap refused before bind: {0}")]
    Platform(String),
    #[error("{0}")]
    TerminalSetup(String),
    #[error("{0}. {guidance}", guidance = crate::installation::STORE_RECOVERY)]
    StoreCompatibility(grapt_platform::PlatformStoreRefusal),
    #[error("service input refused: {0}")]
    Input(String),
    #[error("product composition refused: {0}")]
    Compose(String),
    #[error("listener bind refused: {0}")]
    Bind(String),
    #[error("authenticated self-probe refused: {0}")]
    Probe(String),
    #[error("registration refused: {0}")]
    Registration(String),
    #[error("ready receipt custody refused: {0}")]
    Custody(String),
    #[error("host drain or registration release refused: {0}")]
    Shutdown(String),
}

pub fn run(service_root: &Path) -> Result<LaunchReceipt, ProductRefusal> {
    run_lifetime(service_root, Lifetime::Window, HostMode::Standalone)
}

pub fn run_terminal(
    service_root: &Path,
    notify_ready: bool,
) -> Result<LaunchReceipt, ProductRefusal> {
    run_lifetime(
        service_root,
        Lifetime::terminal(notify_ready)?,
        HostMode::Standalone,
    )
    .map_err(terminal::redact)
}

pub(crate) fn run_connections(service_root: &Path) -> Result<LaunchReceipt, ProductRefusal> {
    setup::administration(service_root).map_err(terminal::redact)
}

fn run_lifetime(
    service_root: &Path,
    lifetime: Lifetime,
    host_mode: HostMode,
) -> Result<LaunchReceipt, ProductRefusal> {
    setup::run_lifetime(service_root, lifetime, host_mode)
}

pub(crate) fn run_inner(
    service_root: &Path,
    admission: Admission,
) -> Result<LaunchReceipt, ProductRefusal> {
    setup::run_inner(service_root, admission)
}

fn run_with_platform(
    service_root: &Path,
    platform: grapt_platform::PlatformOwner,
    handover: Admission,
    lifetime: Lifetime,
    protection: grapt_login::profile_security::ProtectionBatch,
    host_mode: HostMode,
) -> Result<LaunchReceipt, ProductRefusal> {
    let config = read_config(
        &service_root.join("service.conf"),
        platform.build().as_str(),
    )?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let mut parts = platform
        .into_provider_parts()
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
    let secondary_credentials = parts.take_secondary_credentials();
    let (mut store, targets, _build, credential, _first_contact) = parts.into_components();
    crate::commit_executor::install(&mut store);
    let dogfood = crate::dogfood_composition::production_dogfood_composition(
        service_root,
        store,
        targets,
        credential,
        secondary_credentials,
        crate::dogfood::production::provider_endpoint().map_err(ProductRefusal::Compose)?,
        endpoint::provider_limits(service_root, &config)?,
        config.close_grace_ms.map_or(
            grapt_api::u3a::DEFAULT_CLOSE_GRACE,
            std::time::Duration::from_millis,
        ),
        host_mode,
    )?;
    let credential = read_credential(&service_root.join("basic.credential"))?;
    let probe_secret = credential.secret.clone();
    // Startup is complete. Later credential transactions own their own batches,
    // so an idle service does not retain the managed protection process.
    protection
        .finish()
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
    runtime.block_on(run_ready_with_dogfood(
        service_root,
        config,
        credential,
        probe_secret,
        dogfood,
        handover,
        lifetime,
    ))
}

#[cfg(test)]
async fn run_ready(
    service_root: &Path,
    config: ServiceConfig,
    credential: Credential,
    probe_secret: String,
) -> Result<LaunchReceipt, ProductRefusal> {
    let listener = bind_loopback().await?;
    run_ready_on_listener(service_root, config, credential, probe_secret, listener).await
}

async fn run_ready_with_dogfood(
    service_root: &Path,
    config: ServiceConfig,
    credential: Credential,
    probe_secret: String,
    dogfood: DogfoodComposition,
    handover: Admission,
    lifetime: Lifetime,
) -> Result<LaunchReceipt, ProductRefusal> {
    let listener = bind_loopback().await?;
    run_ready_on_listener_inner(
        service_root,
        config,
        credential,
        probe_secret,
        listener,
        Some(dogfood),
        (handover, lifetime),
    )
    .await
}

#[cfg(test)]
async fn run_ready_on_listener(
    service_root: &Path,
    config: ServiceConfig,
    credential: Credential,
    probe_secret: String,
    listener: grapt_location::BoundListener,
) -> Result<LaunchReceipt, ProductRefusal> {
    run_ready_on_listener_inner(
        service_root,
        config,
        credential,
        probe_secret,
        listener,
        None,
        (Admission::Ordinary(None), Lifetime::Window),
    )
    .await
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn run_ready_on_listener_inner(
    service_root: &Path,
    mut config: ServiceConfig,
    credential: Credential,
    probe_secret: String,
    listener: grapt_location::BoundListener,
    mut dogfood: Option<DogfoodComposition>,
    (handover, lifetime): (Admission, Lifetime),
) -> Result<LaunchReceipt, ProductRefusal> {
    let pid = ProcessId::new(std::process::id())
        .ok_or_else(|| ProductRefusal::Compose("process id was zero".to_owned()))?;
    let mut handover_receipt = None;
    let slot = id(ServiceSlotId::new(config.slot.clone()), "service slot")?;
    let environment = id(
        EnvironmentReceipt::new(config.environment.clone()),
        "environment receipt",
    )?;
    let startup = crate::startup::StartupSnapshot::read(service_root, &slot, &environment)?;
    let instance = match &handover {
        Admission::Handover(proof) => proof.successor().clone(),
        Admission::Ordinary(_) => startup.instance(&config.instance)?,
    };
    config.instance = instance.as_str().to_owned();
    let version = id(
        ServiceRegistrationVersion::new(config.version.clone()),
        "service version",
    )?;
    let limits = crate::compose_host::configured_host_limits(&config)?;
    let mut prompt_driver = dogfood
        .as_mut()
        .and_then(|composition| composition.driver.take());
    let shutdown_hub = dogfood
        .as_ref()
        .map(|composition| Arc::clone(&composition.hub));
    let service = product_service(&limits, &credential, pid, &config, dogfood)?;
    let host = PreparedApiHost::new(
        listener,
        service,
        limits,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .map_err(|error| ProductRefusal::Compose(error.to_string()))?
    .start();
    let endpoint = match Endpoint::new(format!("http://{}", host.ready().observed_address())) {
        Some(endpoint) => endpoint,
        None => {
            return stop_and_refuse(
                host,
                ProductRefusal::Compose("observed endpoint was invalid".to_owned()),
            )
            .await;
        }
    };
    let declared = OwnerReceipt::new(
        slot.clone(),
        instance.clone(),
        environment.clone(),
        startup.generation(),
        pid,
        endpoint.clone(),
    );
    let expectation = match HealthExpectation::new(declared.clone(), config.build.clone()) {
        Ok(expectation) => expectation,
        Err(error) => {
            return stop_and_refuse(host, ProductRefusal::Probe(error.to_string())).await;
        }
    };
    let probe =
        match AuthenticatedHealthProbe::new(credential.username.clone(), probe_secret, expectation)
        {
            Ok(probe) => probe,
            Err(error) => {
                return stop_and_refuse(host, ProductRefusal::Probe(error.to_string())).await;
            }
        };
    let probe_outcome = match probe.probe().await {
        Ok(outcome) => outcome,
        Err(error) => {
            return stop_and_refuse(host, ProductRefusal::Probe(error.to_string())).await;
        }
    };
    // Both paths consume the measured probe and retain the same election.
    let authority = match handover {
        Admission::Ordinary(Some(authority)) => match startup
            .commit_elected(service_root, &declared, probe_outcome, &version, authority)
            .await
        {
            Ok(authority) => authority,
            Err(error) => return stop_and_refuse(host, error).await,
        },
        Admission::Ordinary(None) => match startup
            .commit(service_root, &declared, probe_outcome, &version)
            .await
        {
            Ok(authority) => authority,
            Err(error) => return stop_and_refuse(host, error).await,
        },
        Admission::Handover(validated) => {
            match crate::handover::acquire_and_elect(
                *validated,
                service_root,
                &declared,
                probe_outcome,
                &version,
            ) {
                Ok((authority, receipt)) => {
                    handover_receipt = Some(receipt);
                    authority
                }
                Err(error) => return stop_and_refuse(host, error).await,
            }
        }
    };
    let clock = SystemClock;
    let controller = ServiceController::new(slot, environment, &authority, &clock);
    fixture_seam::hold(HoldPoint::ServeBeforeReady).await;
    let ready_refusal = ready::publish(
        service_root,
        &declared,
        config.serve_window_ms,
        &config.build,
    )
    .err();
    let execution_refusal = if ready_refusal.is_none() {
        let refusal =
            terminal::until(lifetime, config.serve_window_ms, prompt_driver.as_mut()).await;
        fixture_seam::hold(HoldPoint::ServeWindowEnd).await;
        refusal
    } else {
        None
    };
    if let Some(hub) = &shutdown_hub {
        hub.shutdown();
    }
    let exit = drain_host(host).await;
    crate::diagnostics::report(format!("service listener stopped outcome={exit:?}"));
    let released = controller
        .discover()
        .map_err(|error| error.to_string())
        .and_then(|registered| {
            controller
                .stop(&registered, &declared)
                .map_err(|error| error.to_string())
        });
    let ready_retired = if ready_refusal.is_none() {
        ready::retire(
            service_root,
            &declared,
            config.serve_window_ms,
            &config.build,
        )
        .map_err(|error| ProductRefusal::Shutdown(error.to_string()))?
    } else {
        false
    };
    let released = released.map_err(ProductRefusal::Shutdown)?;
    let registration_released = registration_was_released(&released);
    if !registration_released {
        return Err(ProductRefusal::Shutdown(format!("{released:?}")));
    }
    if let Some(refusal) = ready_refusal {
        return Err(ProductRefusal::Custody(refusal.to_string()));
    }
    if let Some(refusal) = execution_refusal {
        return Err(ProductRefusal::Shutdown(refusal));
    }
    if ready_refusal.is_none() && !ready_retired {
        return Err(ProductRefusal::Shutdown(
            "matching ready receipt was absent during retirement".to_owned(),
        ));
    }
    Ok(LaunchReceipt {
        schema: "grapt.product.launch.v1",
        outcome: "served",
        pid: pid.value(),
        endpoint: endpoint.as_str().to_owned(),
        build: config.build,
        environment: config.environment,
        service_instance: config.instance,
        registration_released,
        handover: handover_receipt.map(crate::handover::HandoverReceipt::into_evidence),
    })
}

#[cfg(test)]
mod tests;
