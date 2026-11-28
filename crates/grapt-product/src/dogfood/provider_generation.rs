//! One atomic provider publication; accepted chains retain their old snapshot.
use super::{execution::LiveProviders, CatalogProjection};
use std::sync::{Arc, RwLock};

pub(super) struct Generation {
    pub catalog: Arc<CatalogProjection>,
    pub live: Option<Arc<LiveProviders>>,
}

#[derive(Clone)]
pub(super) struct Publication(Arc<RwLock<Arc<Generation>>>);

impl std::fmt::Debug for Publication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProviderPublication(<generation>)")
    }
}

impl Publication {
    pub fn new(catalog: Arc<CatalogProjection>, live: Option<Arc<LiveProviders>>) -> Self {
        Self(Arc::new(RwLock::new(Arc::new(Generation {
            catalog,
            live,
        }))))
    }

    pub fn snapshot(&self) -> Arc<Generation> {
        match self.0.read() {
            Ok(current) => Arc::clone(&current),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    pub fn publish(&self, generation: Generation) {
        let mut current = match self.0.write() {
            Ok(current) => current,
            Err(poisoned) => poisoned.into_inner(),
        };
        *current = Arc::new(generation);
    }
}

impl super::ServerApplication {
    pub(crate) fn catalog_snapshot(&self) -> Arc<CatalogProjection> {
        Arc::clone(&self.providers.snapshot().catalog)
    }
}

impl CatalogProjection {
    pub(super) fn production(
        root: &std::path::Path,
        targets: &grapt_platform::ProviderTargetAuthority,
        credential: &grapt_platform::CredentialAuthority,
    ) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            accounts: targets
                .accounts()
                .map(|(provider, account)| (provider.clone(), account.clone()))
                .collect(),
            protocols: targets
                .accounts()
                .filter_map(|(provider, _)| {
                    targets
                        .protocol_family_for(provider)
                        .map(|family| (provider.clone(), family))
                })
                .collect(),
            project: grapt_types::ProjectId::new("prj_dogfood1")
                .ok_or("invalid product project")?,
            directory: root.to_string_lossy().into_owned(),
            inventory: targets.inventory(),
            provider: targets.provider().as_str().to_owned(),
            model: targets.model().as_str().to_owned(),
            default_effort: targets.default_effort(),
            credential_record: Some(credential.record().as_str().to_owned()),
        }))
    }
}

impl Generation {
    pub(super) fn disconnected(catalog: &CatalogProjection) -> Self {
        let mut catalog = catalog.clone();
        catalog.accounts.clear();
        catalog.protocols.clear();
        catalog.inventory = None;
        catalog.provider.clear();
        catalog.model.clear();
        catalog.default_effort = None;
        catalog.credential_record = None;
        Self {
            catalog: Arc::new(catalog),
            live: None,
        }
    }
}
