//! Build a replacement before the sole provider publication changes.
use super::*;

impl LiveExecutor {
    pub(in crate::dogfood) fn provider_publication(
        &self,
    ) -> super::super::provider_generation::Publication {
        self.location.providers.clone()
    }

    pub(in crate::dogfood) fn prepare_provider_generation(
        &self,
        inputs: grapt_platform::PreparedProviderInputs,
        endpoint: &LiveEndpoint,
        limits: ProductionProviderLimits,
    ) -> Result<super::super::provider_generation::Generation, String> {
        let parts = inputs
            .into_components()
            .map_err(|error| error.to_string())?;
        let catalog = super::super::CatalogProjection::production(
            &self.location.service_root,
            &parts.targets,
            &parts.credential,
        )?;
        let targets = Arc::new(PlatformTargets(parts.targets));
        targets.require_context()?;
        let (first, openers) = providers::open(
            &targets.0,
            parts.credential,
            parts.secondary_credentials,
            endpoint,
            limits,
            self.dispatch.clone(),
            self.error_text.clone(),
            Arc::clone(&self.clock) as Arc<dyn ClockAuthority + Send + Sync>,
        )
        .map_err(|error| error.to_string())?;
        Ok(super::super::provider_generation::Generation {
            catalog,
            live: Some(Arc::new(LiveProviders {
                configured_provider: first.provider().clone(),
                configured_model: first.model().clone(),
                targets,
                first_target: Mutex::new(Some(first)),
                openers,
            })),
        })
    }
}
