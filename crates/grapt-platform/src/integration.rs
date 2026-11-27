//! Existing-platform credential authority extended for external integrations.
//!
//! SQLite remains the durable owner, and credential material is stored as
//! plaintext JSON in the legacy `credential` table exactly as the oracle stores
//! it; this layer does not seal it. It validates product-facing coordinates,
//! converts between the public credential JSON shape and stored text, and
//! scopes every read and write to the transport-authenticated owning plugin.

use grapt_store::{EventStore, IntegrationCredentialWrite};
use grapt_types::Secret;
use serde_json::Value;

const MAX_ID_BYTES: usize = grapt_store::MAX_PLUGIN_COMPATIBILITY_ID_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationConnection {
    id: String,
    integration_id: String,
    label: String,
}

impl IntegrationConnection {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn integration_id(&self) -> &str {
        &self.integration_id
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

#[allow(
    missing_debug_implementations,
    reason = "resolved integration credential contains secret material"
)]
pub struct ResolvedIntegrationCredential {
    value: Secret<Value>,
}

impl ResolvedIntegrationCredential {
    pub fn into_secret(self) -> Secret<Value> {
        self.value
    }
}

/// Who writes a credential and when, from the caller's injected authorities.
#[derive(Debug, Clone, Copy)]
pub struct IntegrationCredentialOwner<'a> {
    pub plugin: &'a str,
    pub now_ms: i64,
}

#[allow(
    missing_debug_implementations,
    reason = "the OAuth write carries secret credential material"
)]
pub struct IntegrationOAuthWrite {
    pub id: String,
    pub integration_id: String,
    pub label: String,
    pub method_id: String,
    pub credential: Secret<Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum IntegrationCredentialRefusal {
    #[error("integration {field} must contain 1..={bound} UTF-8 bytes")]
    InvalidCoordinate { field: &'static str, bound: usize },
    #[error("integration credential has no supported type")]
    UnsupportedCredential,
    #[error("integration credential could not be encoded")]
    Encoding,
    #[error("stored integration credential is unreadable")]
    CorruptStoredCredential,
    #[error(transparent)]
    Store(#[from] grapt_store::write::WriteError),
}

/// Stateless authority facade over the store-owned credential truth.
#[derive(Debug, Default, Clone, Copy)]
pub struct IntegrationCredentialAuthority;

impl IntegrationCredentialAuthority {
    pub fn new() -> Self {
        Self
    }

    fn coordinate(field: &'static str, value: &str) -> Result<(), IntegrationCredentialRefusal> {
        if value.is_empty() || value.len() > MAX_ID_BYTES {
            return Err(IntegrationCredentialRefusal::InvalidCoordinate {
                field,
                bound: MAX_ID_BYTES,
            });
        }
        Ok(())
    }

    pub fn connect_key(
        &self,
        store: &mut EventStore,
        owner: IntegrationCredentialOwner<'_>,
        id: String,
        integration_id: String,
        label: String,
        key: Secret<String>,
    ) -> Result<IntegrationConnection, IntegrationCredentialRefusal> {
        Self::coordinate("credential id", &id)?;
        Self::coordinate("id", &integration_id)?;
        Self::coordinate("owner plugin", owner.plugin)?;
        let value = serde_json::to_string(&serde_json::json!({
            "type": "key",
            "key": key.expose(),
        }))
        .map_err(|_| IntegrationCredentialRefusal::Encoding)?;
        let record = store.put_integration_credential(IntegrationCredentialWrite {
            id,
            integration_id,
            label,
            value: Secret::new(value),
            method_id: None,
            owner_plugin: owner.plugin.to_owned(),
            now_ms: owner.now_ms,
        })?;
        Ok(IntegrationConnection {
            id: record.id,
            integration_id: record.integration_id,
            label: record.label,
        })
    }

    pub fn connect_oauth(
        &self,
        store: &mut EventStore,
        owner: IntegrationCredentialOwner<'_>,
        input: IntegrationOAuthWrite,
    ) -> Result<IntegrationConnection, IntegrationCredentialRefusal> {
        let IntegrationOAuthWrite {
            id,
            integration_id,
            label,
            method_id,
            credential,
        } = input;
        Self::coordinate("credential id", &id)?;
        Self::coordinate("id", &integration_id)?;
        Self::coordinate("method id", &method_id)?;
        Self::coordinate("owner plugin", owner.plugin)?;
        let kind = credential
            .expose()
            .get("type")
            .and_then(Value::as_str)
            .ok_or(IntegrationCredentialRefusal::UnsupportedCredential)?;
        if !matches!(kind, "oauth" | "key") {
            return Err(IntegrationCredentialRefusal::UnsupportedCredential);
        }
        let value = serde_json::to_string(credential.expose())
            .map_err(|_| IntegrationCredentialRefusal::Encoding)?;
        let record = store.put_integration_credential(IntegrationCredentialWrite {
            id,
            integration_id,
            label,
            value: Secret::new(value),
            method_id: Some(method_id),
            owner_plugin: owner.plugin.to_owned(),
            now_ms: owner.now_ms,
        })?;
        Ok(IntegrationConnection {
            id: record.id,
            integration_id: record.integration_id,
            label: record.label,
        })
    }

    /// The active credential `plugin` itself created for `integration_id`.
    pub fn active(
        &self,
        store: &EventStore,
        plugin: &str,
        integration_id: &str,
    ) -> Result<Option<IntegrationConnection>, IntegrationCredentialRefusal> {
        Self::coordinate("id", integration_id)?;
        Self::coordinate("owner plugin", plugin)?;
        Ok(store
            .active_integration_credential_for_plugin(integration_id, plugin)?
            .map(|record| IntegrationConnection {
                id: record.id,
                integration_id: record.integration_id,
                label: record.label,
            }))
    }

    /// Resolve a credential only when `plugin` owns it.
    pub fn resolve(
        &self,
        store: &EventStore,
        plugin: &str,
        connection_id: &str,
    ) -> Result<Option<ResolvedIntegrationCredential>, IntegrationCredentialRefusal> {
        Self::coordinate("connection id", connection_id)?;
        Self::coordinate("owner plugin", plugin)?;
        store
            .integration_credential_for_plugin(connection_id, plugin)?
            .map(|record| {
                let value = serde_json::from_str::<Value>(record.value.expose())
                    .map_err(|_| IntegrationCredentialRefusal::CorruptStoredCredential)?;
                let kind = value.get("type").and_then(Value::as_str);
                if !matches!(kind, Some("key" | "oauth")) {
                    return Err(IntegrationCredentialRefusal::CorruptStoredCredential);
                }
                Ok(ResolvedIntegrationCredential {
                    value: Secret::new(value),
                })
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_round_trip_is_owner_scoped_and_diagnostics_redact_conflicts(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let authority = IntegrationCredentialAuthority::new();
        let mut store = EventStore::open_in_memory()?;
        let owner = IntegrationCredentialOwner {
            plugin: "plugin.a",
            now_ms: 10,
        };
        let connected = authority.connect_key(
            &mut store,
            owner,
            "crd_1".to_owned(),
            "fixture".to_owned(),
            "Fixture".to_owned(),
            Secret::new("sentinel-secret".to_owned()),
        )?;
        let active = authority
            .active(&store, "plugin.a", "fixture")?
            .ok_or("active")?;
        assert_eq!(active, connected);
        assert!(authority.active(&store, "plugin.b", "fixture")?.is_none());
        assert!(authority
            .resolve(&store, "plugin.b", active.id())?
            .is_none());
        let resolved = authority
            .resolve(&store, "plugin.a", active.id())?
            .ok_or("resolved")?;
        assert_eq!(resolved.into_secret().expose()["key"], "sentinel-secret");
        let error = authority
            .connect_key(
                &mut store,
                owner,
                "crd_1".to_owned(),
                "fixture".to_owned(),
                "Fixture".to_owned(),
                Secret::new("different-secret".to_owned()),
            )
            .err()
            .ok_or("conflict")?;
        let diagnostic = error.to_string();
        assert!(!diagnostic.contains("sentinel-secret"));
        assert!(!diagnostic.contains("different-secret"));
        Ok(())
    }
}
