//! Composition boundary for authority-bound external plugin generations.

mod authority;
mod command;
mod generate;
mod host;
mod integration;
mod materialize;
mod websearch;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, Weak};

use grapt_plugin::{
    resolve_declarations, ActivePlugin, BridgeRuntime, CallAuthority, CandidateGeneration,
    ConfigDocument, HostOperation, PluginGeneration, PluginGenerationId, PluginHost,
    PluginHostContext, PluginSupervisor, PluginToolRoute, RuntimeIdentity, RuntimePin,
    DEFAULT_BRIDGE_LIMITS,
};
use grapt_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::{json, Value};

use grapt_api::u3a::DogfoodApplicationPort as _;

use super::catalog_config::{CatalogEnvironment, CatalogSources};
use super::mcp::{PublishedTools, WithheldTool};
use super::{
    catalogs, connections, model_inventory, selection, support, DeterministicAuthorities,
    DogfoodApplication, SessionPorts, SessionSeed,
};
pub(crate) use authority::{PluginCallScope, SESSION_CALL_OPERATIONS};
use host::ProductPluginHost;
use materialize::{materialize_commands, materialize_integrations, materialize_websearch, runtime};

/// The plugin declarations one source set resolves to, each under its
/// document's origin.
pub(super) fn resolve_sources(
    sources: &CatalogSources,
) -> Result<grapt_plugin::ResolvedDeclarations, String> {
    let documents = sources
        .documents
        .iter()
        .map(|document| {
            Ok(ConfigDocument {
                directory: document.directory.clone(),
                source: document.source.clone(),
                origin: document.origin,
                value: super::catalogs::config_json(&document.value)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    resolve_declarations(&documents).map_err(|error| error.to_string())
}

/// The blocked declarations a project adds to the service's own `listed`
/// ones: those from documents that are not the user's, each once.
pub(super) fn project_blocked(
    resolved: &grapt_plugin::ResolvedDeclarations,
    listed: &[Value],
) -> Result<Vec<Value>, String> {
    resolved
        .blocked
        .iter()
        .filter(|entry| !super::catalog_config::user_owned(entry.origin))
        .map(|entry| {
            serde_json::to_value(entry).map_err(|error| format!("plugin provenance: {error}"))
        })
        .filter(|entry| entry.as_ref().map_or(true, |entry| !listed.contains(entry)))
        .collect()
}

pub(super) fn list(
    application: &super::DogfoodApplication,
    query: grapt_api::u3a::ProjectCurrentLocationQueryDto,
) -> grapt_api::u3a::ApplicationFuture<
    grapt_api::u3a::PluginListResponseDto,
    grapt_api::u3a::InvalidRequestErrorDto,
> {
    let catalog = Arc::clone(&application.catalog_snapshot());
    let plugins = Arc::clone(&application.plugins);
    Box::pin(async move {
        plugins.refresh().await.map_err(super::support::invalid)?;
        let location = super::locations::resolve(&catalog, &query)
            .await
            .map_err(super::support::invalid)?;
        let mut data = plugins
            .active()
            .into_iter()
            .map(|plugin| {
                json!({
                    "id": plugin.id,
                    "exportedID": plugin.id,
                    "declaredTarget": plugin.target,
                    "origin": plugin.origin,
                    "source": plugin.source,
                    "status": plugin.status,
                    "generation": plugin.generation.value(),
                })
            })
            .collect::<Vec<_>>();
        let project = plugins
            .blocked_at(std::path::Path::new(&location.directory))
            .map_err(super::support::invalid)?;
        data.extend(plugins.blocked().iter().chain(&project).map(|plugin| {
            json!({
                "id": plugin["target"],
                "exportedID": plugin["target"],
                "declaredTarget": plugin["target"],
                "origin": plugin["origin"],
                "source": plugin["source"],
                "status": "blocked",
                "reason": plugin["reason"],
            })
        }));
        super::support::decode(json!({"location": location.wire(), "data": data}))
            .map_err(super::support::invalid)
    })
}

pub(super) fn catalog_draft(application: &super::DogfoodApplication) -> Result<Value, String> {
    let providers = super::model_inventory::providers(&application.catalog_snapshot())
        .as_array()
        .cloned()
        .ok_or_else(|| "provider inventory is malformed".to_owned())?;
    let models = super::model_inventory::models(&application.catalog_snapshot())
        .as_array()
        .cloned()
        .ok_or_else(|| "model inventory is malformed".to_owned())?;
    let records = providers
        .into_iter()
        .map(|provider| {
            let id = provider.get("id").and_then(Value::as_str);
            let owned = models
                .iter()
                .filter(|model| model.get("providerID").and_then(Value::as_str) == id)
                .cloned()
                .collect::<Vec<_>>();
            json!({"provider":provider,"models":owned})
        })
        .collect::<Vec<_>>();
    let default = super::model_inventory::default_model(&application.catalog_snapshot());
    let selected = if default.is_null() {
        Value::Null
    } else {
        json!({
            "providerID":default.get("providerID"),
            "modelID":default.get("id"),
        })
    };
    Ok(json!({"providers":records,"default":selected}))
}

/// Plugin tool names reserved for product-internal host effects.
pub(crate) const RESERVED_TOOL_NAMES: &[&str] = &[super::tools::INTEGRATION_COMMAND_TOOL];

#[derive(Debug)]
pub(crate) struct PluginRuntime {
    service_root: PathBuf,
    ephemeral: bool,
    state: RwLock<PluginState>,
    refresh: Mutex<()>,
    application: RwLock<Weak<super::DogfoodApplication>>,
}

#[derive(Debug, Default)]
struct PluginState {
    supervisor: Option<Arc<PluginSupervisor>>,
    generation: Option<Arc<PluginGeneration>>,
    declarations: Vec<grapt_plugin::PluginDeclaration>,
    blocked: Vec<Value>,
    websearch: WebSearchRegistry,
    integrations: IntegrationRegistry,
    commands: CommandRegistry,
}

#[derive(Debug, Clone, Default)]
struct WebSearchRegistry {
    providers: Vec<WebSearchProvider>,
    default: Option<String>,
}

#[derive(Debug, Clone)]
struct WebSearchProvider {
    id: String,
    name: String,
    plugin_id: String,
    callback: u64,
}

#[derive(Debug, Clone, Default)]
struct IntegrationRegistry {
    methods: Vec<IntegrationMethod>,
}

#[derive(Debug, Clone)]
enum IntegrationMethod {
    Key {
        integration_id: String,
    },
    Oauth {
        integration_id: String,
        method_id: String,
        plugin_id: String,
        authorize: u64,
    },
    Command {
        integration_id: String,
        method_id: String,
        command: Vec<String>,
    },
}

#[derive(Debug, Clone, Default)]
struct CommandRegistry {
    commands: BTreeMap<String, CommandDefinition>,
}

#[derive(Debug, Clone)]
struct CommandDefinition {
    template: String,
    agent: Option<String>,
    model: Option<Value>,
}

struct Materialized {
    websearch: WebSearchRegistry,
    integrations: IntegrationRegistry,
    commands: CommandRegistry,
}

impl PluginRuntime {
    #[cfg(test)]
    pub(crate) fn empty() -> Arc<Self> {
        Arc::new(Self {
            service_root: PathBuf::new(),
            ephemeral: false,
            state: RwLock::new(PluginState::default()),
            refresh: Mutex::new(()),
            application: RwLock::new(Weak::new()),
        })
    }

    pub(crate) fn compose(service_root: &Path) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            service_root: service_root.to_path_buf(),
            ephemeral: false,
            state: RwLock::new(PluginState::default()),
            refresh: Mutex::new(()),
            application: RwLock::new(Weak::new()),
        }))
    }

    pub(crate) fn compose_ephemeral(service_root: &Path) -> Arc<Self> {
        Arc::new(Self {
            service_root: service_root.to_path_buf(),
            ephemeral: true,
            state: RwLock::new(PluginState::default()),
            refresh: Mutex::new(()),
            application: RwLock::new(Weak::new()),
        })
    }

    pub(crate) fn bind(&self, application: &Arc<super::DogfoodApplication>) -> Result<(), String> {
        let mut bound = self
            .application
            .write()
            .map_err(|_| "plugin application binding is poisoned".to_owned())?;
        *bound = Arc::downgrade(application);
        Ok(())
    }

    /// The profile this runtime serves; its `grapt.jsonc` heads every
    /// reader's sources.
    pub(crate) fn service_root(&self) -> &Path {
        &self.service_root
    }

    fn declarations(&self) -> Result<grapt_plugin::ResolvedDeclarations, String> {
        self.declarations_at(&self.service_root)
    }

    /// The declarations the sources of `directory` resolve to.
    fn declarations_at(
        &self,
        directory: &Path,
    ) -> Result<grapt_plugin::ResolvedDeclarations, String> {
        let environment = CatalogEnvironment::read(&self.service_root)?;
        resolve_sources(&CatalogSources::read(directory, environment)?)
    }

    fn state(&self) -> Result<std::sync::RwLockReadGuard<'_, PluginState>, String> {
        self.state
            .read()
            .map_err(|_| "plugin runtime state is poisoned".to_owned())
    }

    fn supervisor(&self) -> Result<Arc<PluginSupervisor>, String> {
        self.state()?
            .supervisor
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| "plugin generation is no longer active".to_owned())
    }

    /// Reconcile declarations with the running generation. Refreshes are
    /// serialized; a candidate that fails to start or materialize is retired
    /// and the published predecessor keeps serving unchanged.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(crate) fn refresh_blocking(&self, handle: tokio::runtime::Handle) -> Result<bool, String> {
        let _serial = self
            .refresh
            .lock()
            .map_err(|_| "plugin refresh is poisoned".to_owned())?;
        let resolved = self.declarations()?;
        if self.ephemeral && (!resolved.active.is_empty() || !resolved.blocked.is_empty()) {
            return Err("ephemeral runs do not support configured plugins".into());
        }
        let blocked = resolved
            .blocked
            .iter()
            .map(|entry| {
                serde_json::to_value(entry).map_err(|error| format!("plugin provenance: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let existing = {
            let state = self.state()?;
            if state.declarations == resolved.active && state.blocked == blocked {
                let healthy = match (&state.supervisor, &state.generation) {
                    (_, None) => resolved.active.is_empty(),
                    (Some(supervisor), Some(generation)) => supervisor
                        .current_is_running(generation.id())
                        .map_err(|error| error.to_string())?,
                    (None, Some(_)) => false,
                };
                if healthy {
                    return Ok(false);
                }
            }
            state.supervisor.as_ref().map(Arc::clone)
        };
        if resolved.active.is_empty() {
            if let Some(supervisor) = &existing {
                supervisor.shutdown().map_err(|error| error.to_string())?;
            }
            let mut state = self
                .state
                .write()
                .map_err(|_| "plugin runtime state is poisoned".to_owned())?;
            state.generation = None;
            state.declarations = resolved.active;
            state.blocked = blocked;
            state.websearch = WebSearchRegistry::default();
            state.integrations = IntegrationRegistry::default();
            state.commands = CommandRegistry::default();
            return Ok(true);
        }
        let application = self
            .application
            .read()
            .map_err(|_| "plugin application binding is poisoned".to_owned())?
            .clone();
        let supervisor = match existing {
            Some(supervisor) => supervisor,
            None => {
                let supervisor = Arc::new(
                    PluginSupervisor::with_host(
                        runtime(&self.service_root)?,
                        DEFAULT_BRIDGE_LIMITS,
                        Arc::new(ProductPluginHost::new(application.clone(), handle)),
                    )
                    .map_err(|error| error.to_string())?,
                );
                self.state
                    .write()
                    .map_err(|_| "plugin runtime state is poisoned".to_owned())?
                    .supervisor = Some(Arc::clone(&supervisor));
                supervisor
            }
        };
        let candidate = supervisor
            .prepare(
                resolved.active.clone(),
                json!({"name":"Grapt", "version":env!("CARGO_PKG_VERSION")}),
            )
            .map_err(|error| error.to_string())?;
        let materialized = application
            .upgrade()
            .ok_or_else(|| "plugin application is unavailable".to_owned())
            .and_then(|application| Self::materialize(&candidate, &application));
        let materialized = match materialized {
            Ok(materialized) => materialized,
            Err(error) => {
                let _ = candidate.discard();
                return Err(error);
            }
        };
        let generation = supervisor
            .publish(candidate)
            .map_err(|error| error.to_string())?;
        let mut state = self
            .state
            .write()
            .map_err(|_| "plugin runtime state is poisoned".to_owned())?;
        state.generation = Some(generation);
        state.declarations = resolved.active;
        state.blocked = blocked;
        state.websearch = materialized.websearch;
        state.integrations = materialized.integrations;
        state.commands = materialized.commands;
        Ok(true)
    }

    fn materialize(
        candidate: &CandidateGeneration,
        application: &super::DogfoodApplication,
    ) -> Result<Materialized, String> {
        Ok(Materialized {
            websearch: materialize_websearch(candidate)?,
            integrations: materialize_integrations(candidate)?,
            commands: materialize_commands(
                candidate,
                super::catalogs::command_draft(&application.catalog_snapshot())?,
            )?,
        })
    }

    pub(crate) async fn refresh(self: &Arc<Self>) -> Result<bool, String> {
        let runtime = Arc::clone(self);
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || runtime.refresh_blocking(handle))
            .await
            .map_err(|error| format!("plugin reload task was lost: {error}"))?
    }

    pub(crate) fn active(&self) -> Vec<ActivePlugin> {
        self.generation()
            .map_or_else(Vec::new, |generation| generation.active())
    }

    /// The plugins a project at `directory` declares and may not run: the
    /// service's own list never sees a Session's project, so a plugin
    /// declared there is reported here instead of silently never loading.
    pub(crate) fn blocked_at(&self, directory: &Path) -> Result<Vec<Value>, String> {
        project_blocked(&self.declarations_at(directory)?, &self.blocked())
    }

    pub(crate) fn blocked(&self) -> Vec<Value> {
        self.state()
            .map_or_else(|_| Vec::new(), |state| state.blocked.clone())
    }

    pub(crate) fn generation(&self) -> Option<Arc<PluginGeneration>> {
        self.state()
            .ok()
            .and_then(|state| state.generation.as_ref().map(Arc::clone))
    }

    fn websearch_registry(&self) -> Result<WebSearchRegistry, String> {
        self.state().map(|state| state.websearch.clone())
    }

    fn integration_registry(&self) -> Result<IntegrationRegistry, String> {
        self.state().map(|state| state.integrations.clone())
    }

    fn command_registry(&self) -> Result<CommandRegistry, String> {
        self.state().map(|state| state.commands.clone())
    }

    pub(crate) fn runtime_identity(&self) -> Option<RuntimeIdentity> {
        self.state().ok().and_then(|state| {
            state
                .supervisor
                .as_ref()
                .map(|supervisor| supervisor.runtime_identity().clone())
        })
    }

    pub(crate) fn contribute_tools(&self, published: &mut PublishedTools) -> Result<(), String> {
        let Some(generation) = self.generation() else {
            return Ok(());
        };
        let mut claimed = published
            .definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<BTreeSet<_>>();
        let mut plugin_claims = BTreeSet::new();
        let mut additions = Vec::new();
        for registration in generation.registrations() {
            for tool in &registration.tools {
                let withheld = WithheldTool {
                    server: format!("plugin:{}", registration.plugin_id),
                    name: tool.name.clone(),
                };
                if super::tools::BUILTIN_TOOL_NAMES.contains(&tool.name.as_str())
                    || RESERVED_TOOL_NAMES.contains(&tool.name.as_str())
                {
                    published.shadowed.push(withheld);
                    continue;
                }
                if claimed.contains(&tool.name) || !plugin_claims.insert(tool.name.clone()) {
                    published.ambiguous.push(withheld);
                    continue;
                }
                let input_schema = serde_json::to_string(&tool.input_schema)
                    .map_err(|error| format!("plugin tool schema: {error}"))?;
                additions.push(ToolDefinition {
                    input_format: grapt_provider::ToolInputFormat::Json,
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema,
                });
                claimed.insert(tool.name.clone());
            }
        }
        published.definitions.extend(additions);
        Ok(())
    }

    pub(crate) fn route(
        generation: &PluginGeneration,
        published: &PublishedTools,
        name: &str,
    ) -> Option<PluginToolRoute> {
        if published.routes.contains_key(name)
            || RESERVED_TOOL_NAMES.contains(&name)
            || !published
                .definitions
                .iter()
                .any(|definition| definition.name == name)
        {
            return None;
        }
        generation.registrations().iter().find_map(|registration| {
            registration
                .tools
                .iter()
                .find(|tool| tool.name == name)
                .map(|tool| PluginToolRoute {
                    plugin_id: registration.plugin_id.clone(),
                    callback: tool.callback,
                    generation: generation.id(),
                })
        })
    }

    /// Invoke a plugin tool under a grant scoped to its Session and call.
    pub(crate) async fn invoke(
        &self,
        route: PluginToolRoute,
        input: Value,
        context: Value,
        scope: PluginCallScope,
        call: String,
    ) -> Result<Vec<u8>, String> {
        let supervisor = self.supervisor()?;
        tokio::task::spawn_blocking(move || {
            supervisor
                .invoke(
                    route.generation,
                    &route.plugin_id,
                    route.callback,
                    input,
                    context,
                    CallAuthority::Scoped {
                        scope: Arc::new(scope),
                        call,
                    },
                )
                .and_then(|value| {
                    serde_json::to_vec(&value)
                        .map_err(|error| grapt_plugin::PluginError::Invocation(error.to_string()))
                })
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("plugin bridge task was lost: {error}"))?
    }

    fn callbacks(generation: &PluginGeneration, domain: &str, name: &str) -> Vec<(String, u64)> {
        generation
            .registrations()
            .iter()
            .flat_map(|registration| {
                registration
                    .callbacks
                    .iter()
                    .filter(move |callback| callback.domain == domain && callback.name == name)
                    .map(move |callback| (registration.plugin_id.clone(), callback.callback))
            })
            .collect()
    }

    pub(crate) async fn apply_callbacks(
        self: &Arc<Self>,
        generation: &PluginGeneration,
        domain: &str,
        name: &str,
        value: Value,
        authority: CallAuthority,
    ) -> Result<Option<Value>, String> {
        if Self::callbacks(generation, domain, name).is_empty() {
            return Ok(None);
        }
        let runtime = Arc::clone(self);
        let generation = generation.id();
        let (domain, name) = (domain.to_owned(), name.to_owned());
        tokio::task::spawn_blocking(move || {
            let generation = runtime
                .generation()
                .filter(|current| current.id() == generation)
                .ok_or_else(|| "plugin generation is no longer active".to_owned())?;
            runtime.apply_callbacks_blocking(&generation, &domain, &name, value, authority)
        })
        .await
        .map_err(|error| format!("plugin callback task was lost: {error}"))?
    }

    pub(crate) fn apply_callbacks_blocking(
        &self,
        generation: &PluginGeneration,
        domain: &str,
        name: &str,
        mut value: Value,
        authority: CallAuthority,
    ) -> Result<Option<Value>, String> {
        let callbacks = Self::callbacks(generation, domain, name);
        if callbacks.is_empty() {
            return Ok(None);
        }
        let supervisor = self.supervisor()?;
        for (plugin_id, callback) in callbacks {
            value = supervisor
                .invoke_callback(
                    generation.id(),
                    &plugin_id,
                    callback,
                    value,
                    authority.clone(),
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(Some(value))
    }

    pub(crate) async fn transform_current(
        self: &Arc<Self>,
        domain: &str,
        value: Value,
    ) -> Result<Value, String> {
        let Some(generation) = self.generation() else {
            return Ok(value);
        };
        self.apply_callbacks(
            &generation,
            domain,
            "transform",
            value.clone(),
            CallAuthority::Unscoped,
        )
        .await
        .map(|transformed| transformed.unwrap_or(value))
    }

    /// Cancel one plugin call; other calls of the same generation continue.
    pub(crate) async fn cancel_call(
        &self,
        generation: PluginGenerationId,
        plugin: String,
        call: String,
    ) -> Result<(), String> {
        let supervisor = self.supervisor()?;
        tokio::task::spawn_blocking(move || supervisor.cancel_call(generation, &plugin, &call))
            .await
            .map_err(|error| format!("plugin cancellation task was lost: {error}"))?
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}
