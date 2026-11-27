//! Credential changes share one synchronous store/live-generation owner.
use super::*;
use grapt_connections::store::{CredentialFormat, DefaultModel};

enum Change {
    Label(String),
    Remove,
}
impl ConnectionSetup {
    pub(super) fn update_credential_owned(
        &self,
        _principal: SealedPrincipal,
        id: CredentialIdDto,
        query: ProjectCurrentLocationQueryDto,
        request: CredentialUpdateRequestDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        self.change_credential(
            id,
            query,
            Change::Label(request.label().get().as_str().to_owned()),
        )
    }
    pub(super) fn remove_credential_owned(
        &self,
        _principal: SealedPrincipal,
        id: CredentialIdDto,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        self.change_credential(id, query, Change::Remove)
    }
    fn change_credential(
        &self,
        id: CredentialIdDto,
        query: ProjectCurrentLocationQueryDto,
        change: Change,
    ) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
        let owner = self.clone();
        Box::pin(async move {
            if let Change::Label(label) = &change {
                if label.trim().is_empty()
                    || label.len() > 256
                    || label.chars().any(char::is_control)
                {
                    return Err(invalid("credential label must contain 1..=256 bytes"));
                }
            }
            let permit = Arc::clone(&owner.operations)
                .try_acquire_owned()
                .map_err(|_| invalid("connections are being changed; retry shortly"))?;
            super::super::locations::resolve_default(&owner.root.to_string_lossy(), &query)
                .await
                .map_err(invalid)?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                owner.mutate_credential(&id, change)
            })
            .await
            .map_err(|_| invalid("credential storage owner stopped"))?
            .map_err(invalid)?;
            Ok(NoContent)
        })
    }
    fn mutate_credential(&self, id: &CredentialIdDto, change: Change) -> Result<(), String> {
        let _protection = grapt_login::profile_security::ProtectionBatch::begin()
            .map_err(|_| "profile protection is unavailable")?;
        let _mutation = self
            .mutation
            .try_lock()
            .map_err(|_| "connections are being changed; retry shortly")?;
        let mut store = Store::open(&self.root).map_err(|error| error.to_string())?;
        let before = store
            .load()
            .map_err(|error| error.to_string())?
            .ok_or("connection metadata is absent")?;
        let catalog = self
            .application()
            .map(|application| application.catalog_snapshot());
        let inventory = match &catalog {
            Some(catalog) => catalog.inventory.clone(),
            None => grapt_platform::PublishedConfig::load(&self.root)
                .map_err(|error| error.to_string())?
                .inventory(),
        };
        let selected = before.connections.iter().position(|record| {
            id.as_str() == format!("cred_{}", record.id.as_str())
                || catalog.as_ref().is_some_and(|catalog| {
                    catalog.provider == record.id.as_str()
                        && record.credential_format == CredentialFormat::LegacyFile
                        && catalog.credential_record.as_deref() == Some(id.as_str())
                })
        });
        let Some(selected) = selected else {
            return match change {
                Change::Remove => Ok(()),
                Change::Label(_) => Err("credential is not connected to this profile".into()),
            };
        };
        let mut after = before.clone();
        let removed = match change {
            Change::Label(label) => {
                after.connections[selected].label = label;
                None
            }
            Change::Remove => {
                let removed = after.connections.remove(selected).id;
                if after
                    .default_model
                    .as_ref()
                    .is_some_and(|model| model.provider == removed)
                {
                    after.default_model = if after.connections.is_empty() {
                        None
                    } else {
                        let row = inventory
                            .as_ref()
                            .and_then(|inventory| {
                                inventory
                                    .available()
                                    .iter()
                                    .find(|row| row.provider() != &removed)
                            })
                            .ok_or("no replacement default model is available")?;
                        Some(DefaultModel {
                            provider: row.provider().clone(),
                            model: row.id().clone(),
                            variant: row.default_variant().cloned(),
                        })
                    };
                }
                Some(removed)
            }
        };
        let approval = grapt_platform::profile_configuration::ConnectionChange::prepare(
            &self.root,
            Some(&before),
            &after,
        )
        .map_err(|error| error.to_string())?;
        let (prepared, commit) = match removed {
            Some(id) => store.remove_with(&id, &after, || self.prepare_generation(&after)),
            None => store.update_with(&after, || self.prepare_generation(&after)),
        }
        .map_err(|error| error.to_string())?;
        self.publish_generation(prepared, approval, commit)
    }
}
