//! Compose the live execution application and its HTTP integration owners.

use std::path::Path;
use std::sync::Arc;

use crate::compose::{DogfoodComposition, HostMode, ProductRefusal};
use crate::dogfood::DogfoodApplication;

#[allow(clippy::too_many_arguments)]
pub(crate) fn production_dogfood_composition(
    service_root: &Path,
    store: grapt_store::EventStore,
    targets: grapt_platform::ProviderTargetAuthority,
    credential: grapt_platform::CredentialAuthority,
    secondary_credentials: Vec<(grapt_types::ProviderId, grapt_platform::CredentialAuthority)>,
    endpoint: crate::dogfood::LiveEndpoint,
    limits: crate::dogfood::production::ProductionProviderLimits,
    close_grace: std::time::Duration,
    host_mode: HostMode,
) -> Result<DogfoodComposition, ProductRefusal> {
    let hub = crate::dogfood::event_hub_with_grace(close_grace).map_err(ProductRefusal::Compose)?;
    crate::dogfood::stream_diagnostics::install(&hub, service_root);
    let seat = crate::dogfood::stateengine_seat::install(&hub, service_root);
    let (application, driver, _plugins) = open_production_application(
        service_root,
        store,
        targets,
        credential,
        secondary_credentials,
        endpoint,
        limits,
        Arc::clone(&hub),
        matches!(host_mode, HostMode::External),
    )?;
    let capture = Arc::clone(&application);
    let mut composition =
        DogfoodComposition::ready(application.clone(), capture, Arc::clone(&hub), Some(driver));
    composition.seat = seat;
    composition.installer = Some(Arc::new(crate::dogfood::claude_plugins::Installer::new(
        service_root,
    )));
    composition.integrations = Some(
        crate::dogfood::connection_setup::ConnectionSetup::configured(
            service_root,
            application,
            Arc::clone(&composition.ports),
            hub,
            limits,
            matches!(host_mode, HostMode::External),
        ),
    );
    Ok(composition)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn open_production_application(
    service_root: &Path,
    store: grapt_store::EventStore,
    targets: grapt_platform::ProviderTargetAuthority,
    credential: grapt_platform::CredentialAuthority,
    secondary_credentials: Vec<(grapt_types::ProviderId, grapt_platform::CredentialAuthority)>,
    endpoint: crate::dogfood::LiveEndpoint,
    limits: crate::dogfood::production::ProductionProviderLimits,
    hub: Arc<grapt_api::DogfoodEventHub>,
    external_host: bool,
) -> Result<
    (
        Arc<DogfoodApplication>,
        crate::dogfood::prompt_driver::PromptDriver,
        Arc<crate::dogfood::plugins::PluginRuntime>,
    ),
    ProductRefusal,
> {
    let plugins = crate::dogfood::plugins::PluginRuntime::compose(service_root)
        .map_err(ProductRefusal::Compose)?;
    let (application, driver) = DogfoodApplication::open_production_with_external_host(
        service_root,
        store,
        targets,
        credential,
        secondary_credentials,
        endpoint,
        limits,
        Arc::clone(&hub),
        Arc::clone(&plugins),
        external_host,
    )
    .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    plugins
        .bind(&application)
        .map_err(ProductRefusal::Compose)?;
    Ok((application, driver, plugins))
}
