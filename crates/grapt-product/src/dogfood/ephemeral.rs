//! Volatile application construction; the persistent constructor remains unchanged.
use super::*;

impl DogfoodApplication {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_ephemeral(
        service_root: &Path,
        store: grapt_store::EventStore,
        targets: ProviderTargetAuthority,
        credential: CredentialAuthority,
        secondary_credentials: Vec<(grapt_types::ProviderId, CredentialAuthority)>,
        endpoint: LiveEndpoint,
        limits: ProductionProviderLimits,
        hub: Arc<DogfoodEventHub>,
        plugins: Arc<plugins::PluginRuntime>,
    ) -> Result<(Arc<Self>, prompt_driver::PromptDriver), DogfoodRefusal> {
        Self::open_production_mode(
            service_root,
            store,
            targets,
            credential,
            secondary_credentials,
            endpoint,
            limits,
            hub,
            plugins,
            true,
            false,
        )
    }
}
