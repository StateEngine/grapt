//! Plugin web-search operations over the oracle-compatible selection and the
//! provider callbacks each plugin registered.

use grapt_types::{ClockAuthority as _, IdAuthority as _};

use super::authority::Authorized;
use super::*;

const MAX_QUERY_BYTES: usize = 16 * 1024;

impl ProductPluginHost {
    pub(crate) fn websearch(
        &self,
        operation: HostOperation,
        application: &Arc<super::ServerApplication>,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        let registry = application.plugins.websearch_registry()?;
        match operation {
            HostOperation::WebsearchProviders => Ok(Self::located(
                application,
                Value::Array(
                    registry
                        .providers
                        .iter()
                        .map(|provider| json!({"id":provider.id,"name":provider.name}))
                        .collect(),
                ),
            )),
            HostOperation::WebsearchSelectionGet => Ok(Self::located(
                application,
                selection(application, &registry)?,
            )),
            HostOperation::WebsearchSelectionPut => {
                let requested = payload.get("payload").unwrap_or(payload);
                put_selection(
                    application.lock_inbox().store_mut(),
                    &registry,
                    requested,
                    application.clock.now().as_millis(),
                )?;
                publish_updated(application)?;
                Ok(Self::located(
                    application,
                    selection(application, &registry)?,
                ))
            }
            HostOperation::WebsearchQuery => {
                let call = authorized.call()?;
                let query = payload
                    .get("query")
                    .and_then(Value::as_str)
                    .filter(|query| !query.is_empty() && query.len() <= MAX_QUERY_BYTES)
                    .ok_or_else(|| {
                        format!("websearch query must contain 1..={MAX_QUERY_BYTES} UTF-8 bytes")
                    })?;
                let selected = match payload.get("providerID").and_then(Value::as_str) {
                    Some(provider) => provider.to_owned(),
                    None => selection(application, &registry)?
                        .get("providerID")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| "websearch is disabled".to_owned())?,
                };
                let provider = registry
                    .providers
                    .iter()
                    .find(|provider| provider.id == selected)
                    .ok_or_else(|| format!("websearch provider is unavailable: {selected}"))?;
                let generation = application
                    .plugins
                    .generation()
                    .ok_or_else(|| "plugin generation is no longer active".to_owned())?;
                let results = application
                    .plugins
                    .supervisor()?
                    .invoke_callback(
                        generation.id(),
                        &provider.plugin_id,
                        provider.callback,
                        json!({"query": query}),
                        CallAuthority::Inherit {
                            parent: call.grant.clone(),
                            call: format!("websearch:{}", call.grant.id()),
                        },
                    )
                    .map_err(|error| error.to_string())?;
                if !results.is_array() {
                    return Err(format!(
                        "websearch provider {} returned a malformed result list",
                        provider.id
                    ));
                }
                Ok(json!({"providerID": provider.id, "results": results}))
            }
            HostOperation::WebsearchReload => Ok(Value::Null),
            _ => Err("websearch dispatch changed".to_owned()),
        }
    }
}

fn selection(
    application: &super::ServerApplication,
    registry: &WebSearchRegistry,
) -> Result<Value, String> {
    selected(application.lock_inbox().store(), registry)
}

fn selected(
    store: &grapt_store::EventStore,
    registry: &WebSearchRegistry,
) -> Result<Value, String> {
    use grapt_store::plugin_compatibility::WebsearchProviderSelection;
    let provider = match store
        .websearch_provider_selection()
        .map_err(|error| error.to_string())?
    {
        Some(WebsearchProviderSelection::Disabled) => None,
        Some(WebsearchProviderSelection::Provider(provider)) => Some(provider),
        None => registry.default.clone(),
    };
    Ok(match provider {
        Some(provider_id) => json!({"status":"enabled","providerID":provider_id}),
        None => json!({"status":"disabled"}),
    })
}

fn put_selection(
    store: &mut grapt_store::EventStore,
    registry: &WebSearchRegistry,
    requested: &Value,
    now_ms: i64,
) -> Result<(), String> {
    match requested.get("status").and_then(Value::as_str) {
        Some("disabled") => store
            .disable_websearch_provider_selection(now_ms)
            .map_err(|error| error.to_string()),
        Some("enabled") => {
            let provider_id = requested
                .get("providerID")
                .and_then(Value::as_str)
                .ok_or_else(|| "websearch selection requires providerID".to_owned())?;
            if !registry
                .providers
                .iter()
                .any(|provider| provider.id == provider_id)
            {
                return Err(format!("websearch provider is unavailable: {provider_id}"));
            }
            store
                .put_websearch_provider_selection(provider_id, now_ms)
                .map_err(|error| error.to_string())
        }
        _ => Err("websearch selection status is invalid".to_owned()),
    }
}

fn publish_updated(application: &super::ServerApplication) -> Result<(), String> {
    let id = grapt_api::u3a::WireId::parse(format!("evt_{}", application.ids.mint().as_str()))
        .ok_or_else(|| "websearch update event identity is invalid".to_owned())?;
    let created = grapt_api::u3a::FiniteF64::try_new(application.clock.now().as_millis() as f64)
        .map_err(str::to_owned)?;
    application
        .hub
        .publish(&grapt_api::u3a::DogfoodEvent::WebSearchUpdated(
            grapt_api::u3a::LiveEvent::new(
                id,
                created,
                grapt_api::u3a::WebSearchUpdatedData::default(),
            ),
        ))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_websearch_disable_survives_reopen_and_suppresses_default(
    ) -> Result<(), Box<dyn std::error::Error>> {
        struct Profile(std::path::PathBuf);
        impl Drop for Profile {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let path = std::env::temp_dir().join(format!(
            "grapt-websearch-disable-{}",
            grapt_store::OsRandomIds::seed()?.instance()
        ));
        std::fs::create_dir(&path)?;
        let profile = Profile(path);
        let database = profile.0.join("store.db");
        let registry = WebSearchRegistry {
            default: Some("default-search".to_owned()),
            providers: vec![WebSearchProvider {
                id: "chosen-search".to_owned(),
                name: "Chosen search".to_owned(),
                plugin_id: "fixture-search".to_owned(),
                callback: 1,
            }],
        };
        {
            let mut store = grapt_store::EventStore::open(&database, "websearch-disable-test")?;
            assert_eq!(
                selected(&store, &registry)?,
                json!({"status":"enabled","providerID":"default-search"})
            );
            put_selection(&mut store, &registry, &json!({"status":"disabled"}), 1)?;
            assert_eq!(selected(&store, &registry)?, json!({"status":"disabled"}));
        }
        let mut store = grapt_store::EventStore::open(&database, "websearch-disable-test")?;
        assert_eq!(selected(&store, &registry)?, json!({"status":"disabled"}));
        put_selection(
            &mut store,
            &registry,
            &json!({"status":"enabled","providerID":"chosen-search"}),
            2,
        )?;
        assert_eq!(
            selected(&store, &registry)?,
            json!({"status":"enabled","providerID":"chosen-search"})
        );
        put_selection(&mut store, &registry, &json!({"status":"disabled"}), 3)?;
        assert!(put_selection(
            &mut store,
            &registry,
            &json!({"status":"enabled","providerID":"unregistered"}),
            4
        )
        .is_err());
        assert!(put_selection(&mut store, &registry, &json!({"status":"other"}), 4).is_err());
        assert_eq!(selected(&store, &registry)?, json!({"status":"disabled"}));
        store.clear_websearch_provider_selection()?;
        assert_eq!(
            selected(&store, &registry)?,
            json!({"status":"enabled","providerID":"default-search"})
        );
        drop(store);
        Ok(())
    }
}
