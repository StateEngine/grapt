//! Connection catalogs reflect the capabilities actually composed by this
//! product. Declaring an MCP server does not establish a transport.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use grapt_api::u3a::{
    ApplicationFuture, InvalidRequestErrorDto, McpAddRequestDto, NoContent,
    ProjectCurrentLocationQueryDto,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

#[cfg(test)]
use super::catalog_config::CatalogEnvironment;
use super::catalog_config::CatalogSources;
use super::catalogs::config_json;
use super::mcp::McpTimeouts;
use super::support::{decode, invalid};
pub(crate) use grapt_mcp_host::declaration::runtime_declaration;

pub(crate) mod project_approval;

pub(crate) enum Kind {
    Integrations,
    McpServers,
    McpInspection,
}

/// Apply the existing plugin transform to the composed connection catalog.
pub(crate) fn integrations(
    application: &super::DogfoodApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<grapt_api::u3a::IntegrationListResponseDto, InvalidRequestErrorDto> {
    let application = application.clone();
    let plugins = Arc::clone(&application.plugins);
    Box::pin(async move {
        let mut response = response::<Value>(application, query, Kind::Integrations).await?;
        let items = response["data"].as_array().cloned().unwrap_or_default();
        let methods = items
            .iter()
            .flat_map(|integration| {
                let id = integration.get("id").cloned().unwrap_or(Value::Null);
                integration["methods"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(move |method| json!({"integrationID":id,"method":method}))
            })
            .collect::<Vec<_>>();
        let transformed = plugins
            .transform_current("integration", json!({"items":items,"methods":methods}))
            .await
            .map_err(invalid)?;
        let transformed_methods = transformed["methods"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut data = transformed["items"]
            .as_array()
            .cloned()
            .ok_or_else(|| invalid("plugin integration transform removed the item list"))?;
        for integration in &mut data {
            let id = integration.get("id");
            integration["methods"] = Value::Array(
                transformed_methods
                    .iter()
                    .filter(|row| row.get("integrationID") == id)
                    .map(|row| row["method"].clone())
                    .collect(),
            );
        }
        response["data"] = Value::Array(data);
        decode(response).map_err(invalid)
    })
}

pub(crate) fn inspect(
    application: &super::DogfoodApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<grapt_api::u3a::McpInspectionResponseDto, InvalidRequestErrorDto> {
    response(application.clone(), query, Kind::McpInspection)
}

/// Configuration-only MCP catalog for the configuration route: declared
/// names with their document scope and disabled state.
///
/// Unlike [`response`], this never consults the live registry, so a
/// declared server is listed without being started — a preview must report
/// what a write would do without spawning a peer to find out.
pub(crate) fn configuration(
    application: &super::DogfoodApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<grapt_api::u3a::McpConfigurationListResponseDto, InvalidRequestErrorDto> {
    let application = application.clone();
    Box::pin(async move {
        let selection = super::session_config::catalog::select(&application, &query)
            .await
            .map_err(invalid)?;
        let snapshot = McpSnapshot::read(&selection.sources, &selection.location.directory)
            .map_err(invalid)?;
        decode(json!({
            "location": selection.location.wire(),
            "data": snapshot.configuration_rows(),
        }))
        .map_err(invalid)
    })
}

pub(crate) fn reconnect(
    application: super::DogfoodApplication,
    server: String,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
    Box::pin(async move {
        let selection = super::session_config::catalog::select(&application, &query)
            .await
            .map_err(invalid)?;
        let snapshot = McpSnapshot::read(&selection.sources, &selection.location.directory)
            .map_err(invalid)?;
        selection.validate_current(&application).map_err(invalid)?;
        application
            .mcp
            .reconnect(&selection.registry_key, &server, &snapshot.declared)
            .await
            .map_err(invalid)?;
        Ok(NoContent)
    })
}

pub(crate) fn add_runtime(
    application: super::DogfoodApplication,
    server: String,
    query: ProjectCurrentLocationQueryDto,
    request: McpAddRequestDto,
) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
    let registry = Arc::clone(&application.mcp);
    Box::pin(async move {
        let selection = super::session_config::catalog::select(&application, &query)
            .await
            .map_err(invalid)?;
        let location = &selection.location;
        let config = serde_json::to_value(request.config().get()).map_err(invalid)?;
        let declaration =
            runtime_declaration(&location.directory, &server, &config).map_err(invalid)?;
        let snapshot =
            McpSnapshot::read(&selection.sources, &location.directory).map_err(invalid)?;
        registry
            .register_runtime(&selection.registry_key, server, declaration)
            .await
            .map_err(invalid)?;
        snapshot
            .prepare_roots(&registry, &selection.registry_key, selection.roots_revision)
            .await
            .map_err(invalid)?;
        registry
            .with_status_at(
                &selection.registry_key,
                &location.directory,
                &snapshot.declared,
                |_| (),
            )
            .await;
        selection.validate_current(&application).map_err(invalid)?;
        Ok(NoContent)
    })
}

pub(crate) fn response<T: DeserializeOwned + Send + 'static>(
    application: super::DogfoodApplication,
    query: ProjectCurrentLocationQueryDto,
    kind: Kind,
) -> ApplicationFuture<T, InvalidRequestErrorDto> {
    Box::pin(async move {
        if matches!(kind, Kind::Integrations) {
            let catalog = &application.catalog_snapshot();
            let location = super::locations::resolve(catalog, &query)
                .await
                .map_err(invalid)?;
            let data = match grapt_connections::store::Store::read_snapshot(Path::new(
                &catalog.directory,
            ))
            .map_err(invalid)?
            {
                Some(records) => super::connection_setup::integration_rows(
                    &records,
                    catalog
                        .credential_record
                        .as_deref()
                        .map(|id| (catalog.provider.as_str(), id)),
                )
                .map_err(invalid)?,
                None => match &catalog.credential_record {
                    None => json!([]),
                    Some(record) => {
                        json!([{"id":catalog.provider,"name":catalog.provider,"methods":[],
                        "connections":[{"type":"credential","id":record,"label":"Configured product credential"}]}])
                    }
                },
            };
            return decode(json!({"location":location.wire(), "data":data})).map_err(invalid);
        }
        let selection = super::session_config::catalog::select(&application, &query)
            .await
            .map_err(invalid)?;
        let location = &selection.location;
        let snapshot =
            McpSnapshot::read(&selection.sources, &location.directory).map_err(invalid)?;
        snapshot
            .prepare_roots(
                &application.mcp,
                &selection.registry_key,
                selection.roots_revision,
            )
            .await
            .map_err(invalid)?;
        // The registry retains healthy sessions and retries failed
        // local startups under bounded backoff and one reservation.
        let reading = application.mcp.with_status_at(
            &selection.registry_key,
            &location.directory,
            &snapshot.declared,
            |entries| match kind {
                Kind::McpServers => Ok(Value::Array(snapshot.servers(entries))),
                Kind::McpInspection => snapshot.inspection(entries).map(Value::Array),
                Kind::Integrations => Err("connection catalog dispatch changed".to_owned()),
            },
        );
        tokio::pin!(reading);
        let data = loop {
            tokio::select! {
                result = &mut reading => break result.map_err(invalid)?,
                () = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                    // Dropping this read drops its startup reservation and live
                    // form owner; a stale Session revision cannot accept input.
                    selection.validate_current(&application).map_err(invalid)?;
                }
            }
        };
        selection.validate_current(&application).map_err(invalid)?;
        let mut response = json!({"location":location.wire(), "data":data});
        if matches!(kind, Kind::McpInspection) {
            response["subscriptions"] = json!(
                application
                    .mcp
                    .resource_subscriptions(&selection.registry_key, selection.session.as_ref())
                    .await
            );
        }
        decode(response).map_err(invalid)
    })
}

/// The document class a declared server came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigScope {
    User,
    Project,
}

fn scope_of(origin: grapt_plugin::PluginOrigin) -> ConfigScope {
    if super::catalog_config::user_owned(origin) {
        ConfigScope::User
    } else {
        ConfigScope::Project
    }
}

pub(crate) struct McpSnapshot {
    pub(crate) roots: super::mcp::roots::Config,
    pub(crate) declared: BTreeMap<String, super::mcp::Declaration>,
    /// Document scope per declared name, filled lockstep with `declared`.
    pub(crate) scope: BTreeMap<String, ConfigScope>,
    pub(crate) codemode_servers: std::collections::BTreeSet<String>,
    /// Project declarations without matching user approval. They cannot start
    /// a process or contact a remote service; each is reported to the Session.
    pub(crate) blocked: Vec<String>,
    pub(in crate::dogfood) project_servers: BTreeMap<String, project_approval::ProjectServer>,
}

/// Why a [`McpSnapshot::blocked`] server was not started, as the Session's
/// MCP notice reason.
pub(crate) const PROJECT_LOCAL_BLOCKED: &str =
    "MCP servers from project config require approval; use /mcp approve <server> or add them to your user config";

impl McpSnapshot {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(in crate::dogfood) fn read(
        sources: &CatalogSources,
        location: &str,
    ) -> Result<Self, String> {
        let roots = super::mcp::roots::Config::read(sources, location)?;
        // Each declaration keeps the document that made it, so a refusal names
        // that document: the one a user must edit, not merely the first one
        // that mentions MCP.
        let mut declarations: BTreeMap<
            String,
            (
                serde_json::Map<String, Value>,
                grapt_plugin::PluginOrigin,
                usize,
            ),
        > = BTreeMap::new();
        // Global phase budgets, later documents overriding earlier ones, exactly
        // as the oracle's `Object.assign` over its document list does.
        let mut global = McpTimeouts::default();
        for (index, document) in sources.documents.iter().enumerate() {
            let mut read_document = || -> Result<(), String> {
                let Some(config) = document.value.get("mcp") else {
                    return Ok(());
                };
                let config = config_json(config.value())?;
                let config = config
                    .as_object()
                    .ok_or_else(|| "MCP declarations must be an object".to_owned())?;
                // WHICH SHAPE IS THIS `mcp` NODE?
                //
                // The frozen contract (the oracle's `mcp.ts`) is
                // `mcp: { timeout?, servers? }`. This product read `mcp` as the
                // server map DIRECTLY, so a config written to the documented shape
                // was taken as two servers named `servers` and `timeout`, both
                // failing for declaring no type -- following the documentation
                // produced an error blaming the user's server names.
                //
                // THE RULE READS THE VALUE, NOT JUST THE KEY. Keying on presence
                // alone left the ambiguous case unreachable: a flat server called
                // `servers` also made the key present, so it silently took the
                // nested path and the guard written for it could never fire. A
                // server declaration names its `type`; a server MAP does not.
                // That is the discriminator, and the one genuinely ambiguous
                // arrangement is refused below naming where to move it.
                let servers_value = config.get("servers");
                // A DECLARATION'S `type` IS A STRING; A SERVER MAP'S `type` ENTRY
                // IS A DECLARATION OBJECT. Testing for the KEY alone reserved the
                // name: `servers: { type: { type: "local", ... } }` is a perfectly
                // valid server called `type` -- the frozen `Schema.Record` admits
                // any name -- and it was refused as though `servers` were itself a
                // declaration.
                let servers_is_a_declaration = servers_value
                    .and_then(Value::as_object)
                    .and_then(|fields| fields.get("type"))
                    .is_some_and(Value::is_string);
                if servers_is_a_declaration {
                    return Err(
                    "`servers` names a server declaration here, but at the top of `mcp` it is the documented map of servers. Move that declaration to `mcp.servers.servers`, or rename it."
                        .to_owned(),
                );
                }
                let oracle_shape = servers_value.is_some() || config.contains_key("timeout");
                if oracle_shape {
                    for key in config.keys() {
                        if key != "servers" && key != "timeout" {
                            return Err(format!(
                            "the MCP configuration declares `servers` or `timeout`, so it is read in the documented shape `mcp: {{ servers, timeout }}`; move `{key}` under `mcp.servers`"
                        ));
                        }
                    }
                    if let Some(fields) = config.get("timeout") {
                        global = McpTimeouts::layer(global, fields, "the global MCP")?;
                    }
                }
                let servers = if oracle_shape {
                    match servers_value {
                        Some(value) => value.as_object().ok_or_else(|| {
                            "MCP `servers` must be an object of server declarations".to_owned()
                        })?,
                        None => return Ok(()),
                    }
                } else {
                    config
                };
                for (name, value) in servers {
                    if name.is_empty() || name.len() > 1024 {
                        return Err("MCP server name is invalid".to_owned());
                    }
                    let fields = value
                        .as_object()
                        .ok_or_else(|| "MCP server declaration must be an object".to_owned())?;
                    // A LATER DOCUMENT REPLACES THE WHOLE DECLARATION, it does not
                    // merge into it. The oracle does `runtime.set(name, {...server})`
                    // (mcp/index.ts:235-238): only the GLOBAL timeout phases are
                    // merged, and then only under that one selected server.
                    //
                    // Merging per key, which this did, is not a smaller difference
                    // than it sounds. A server declared in a user document with an
                    // `environment` and a `cwd`, then REDECLARED in a project
                    // document with only a `command`, kept the first document's
                    // environment and directory: a replacement declaration silently
                    // carrying the credentials and working directory it was
                    // written to replace.
                    declarations.insert(name.clone(), (fields.clone(), document.origin, index));
                    if declarations.len() > 256 {
                        return Err("MCP catalog exceeds 256 servers".to_owned());
                    }
                }
                Ok(())
            };
            read_document().map_err(|reason| document.refusal(&reason))?;
        }
        let mut declared = BTreeMap::new();
        let mut scope = BTreeMap::new();
        let mut codemode_servers = std::collections::BTreeSet::new();
        let mut blocked = Vec::new();
        let approvals = project_approval::Approvals::read(sources, location)?;
        let mut project_servers = BTreeMap::new();
        for (name, (mut fields, origin, index)) in declarations {
            let approved = if !super::catalog_config::user_owned(origin) {
                let mut preview = project_approval::preview(
                    location,
                    &sources.documents[index].source,
                    &name,
                    &fields,
                    global,
                )?;
                let enabled = approvals.enabled(&name, &preview.fingerprint);
                preview.approved = enabled.is_some();
                project_servers.insert(name.clone(), preview);
                if let Some(enabled) = enabled {
                    fields.insert("disabled".into(), Value::Bool(!enabled));
                    fields.remove("enabled");
                }
                enabled.is_some()
            } else {
                false
            };
            let mut declare = || -> Result<(), String> {
                let name = name.clone();
                let flag = |key: &str| -> Result<Option<bool>, String> {
                    fields
                        .get(key)
                        .map(|value| {
                            value
                                .as_bool()
                                .ok_or_else(|| format!("MCP {key} must be boolean"))
                        })
                        .transpose()
                };
                let disabled =
                    flag("disabled")?.unwrap_or(false) || flag("enabled")? == Some(false);
                if disabled {
                    scope.insert(name.clone(), scope_of(origin));
                    declared.insert(name, super::mcp::Declaration::Disabled);
                    return Ok(());
                }
                let declaration = grapt_mcp_host::declaration::configured_declaration(
                    location, &name, &fields, global,
                )?;
                // Validated like any other, then not started when a project
                // declared it: a malformed one still refuses.
                if !matches!(declaration, super::mcp::Declaration::Disabled)
                    && !super::catalog_config::user_owned(origin)
                    && !approved
                {
                    codemode_servers.remove(&name);
                    blocked.push(name);
                    return Ok(());
                }
                // Keep initial MCP schemas behind the fixed discovery shell.
                // An explicit false retains the user's direct-tool opt-out.
                if flag("codemode")?.unwrap_or(true) {
                    codemode_servers.insert(name.clone());
                }
                scope.insert(name.clone(), scope_of(origin));
                declared.insert(name, declaration);
                Ok(())
            };
            declare().map_err(|reason| sources.documents[index].refusal(&reason))?;
        }
        Ok(Self {
            roots,
            declared,
            scope,
            codemode_servers,
            blocked,
            project_servers,
        })
    }

    pub(crate) async fn prepare_roots(
        &self,
        registry: &super::mcp::McpRegistry,
        key: &str,
        revision: super::mcp::roots::Revision,
    ) -> Result<(), String> {
        registry
            .configure_roots(key, &self.roots, &self.declared, revision)
            .await
    }

    /// Configuration-only rows: name, document scope and disabled state.
    /// Reads no registry, so nothing is started and no transport status is
    /// invented.
    pub(crate) fn configuration_rows(&self) -> Vec<Value> {
        self.declared
            .iter()
            .map(|(name, declaration)| {
                debug_assert!(
                    self.scope.contains_key(name),
                    "scope is filled lockstep with declared"
                );
                let scope = match self.scope.get(name) {
                    Some(ConfigScope::User) => "user",
                    _ => "project",
                };
                json!({
                    "name": name,
                    "scope": scope,
                    "disabled": matches!(declaration, super::mcp::Declaration::Disabled),
                })
            })
            .collect()
    }

    /// Project the live registry's truth into the retained status shape.
    ///
    /// The frozen status union has connected, pending, disabled, failed,
    /// needs_auth and needs_client_registration — it has no "timed out". So a
    /// timeout and a hangup both carry the `failed` status, and the reason
    /// they are still distinguishable is the `error` string, which names which
    /// one happened. Collapsing them into one message is what would make an
    /// operator unable to tell a slow server from a misconfigured one.
    fn servers(&self, entries: &BTreeMap<String, super::mcp::ServerEntry>) -> Vec<Value> {
        // Keep the retained response closed. The separate native inspection
        // response carries tool names and omission reasons.
        let mut rows = entries
            .iter()
            .map(|(name, entry)| {
                let status = match entry.status_label() {
                    "disabled" => json!({"status":"disabled"}),
                    "connected" => json!({"status":"connected"}),
                    label => json!({
                        "status":"failed",
                        "error": format!(
                            "The configured MCP server is {label}: {}.",
                            entry.detail()
                        ),
                    }),
                };
                json!({"name":name, "status":status})
            })
            .collect::<Vec<_>>();
        rows.extend(self.blocked.iter().filter(|name| !entries.contains_key(*name)).map(|name|
            json!({"name":name,"status":{"status":"failed","error":PROJECT_LOCAL_BLOCKED}})
        ));
        rows
    }

    fn inspection(
        &self,
        entries: &BTreeMap<String, super::mcp::ServerEntry>,
    ) -> Result<Vec<Value>, String> {
        let published = super::mcp::tool_definitions(entries, super::tools::BUILTIN_TOOL_NAMES);
        self.servers(entries)
            .into_iter()
            .map(|server| {
                let name = server["name"]
                    .as_str()
                    .ok_or("MCP inspection server name is unreadable")?;
                let tools = inspection_tools(name, &published);
                Ok(json!({"server":server,"tools":tools}))
            })
            .collect()
    }
}

fn inspection_tools(server: &str, published: &super::mcp::PublishedTools) -> Vec<Value> {
    let mut tools: Vec<Value> = published
        .routes
        .iter()
        .filter(|(_, route)| route.server == server)
        .map(|(name, _)| json!({"name":name}))
        .collect();
    tools.extend(
        omissions(server, published)
            .into_iter()
            .map(|row| json!({"name":row["tool"],"omission":row["reason"]})),
    );
    tools
}

/// The tools this server published that a model was NOT offered, each with the
/// reason it was withheld.
///
/// Matched on the RECORDED originating server, never on the qualified name's
/// prefix. `a_b_tool` starts with `a_` as well as `a_b_`, so prefix matching
/// would tell a server named `a` about a tool that was never its own.
fn omissions(server: &str, published: &super::mcp::PublishedTools) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut collect = |withheld: &[super::mcp::WithheldTool], reason: &str| {
        for tool in withheld {
            if tool.server == server {
                rows.push(json!({"tool": tool.name, "reason": reason}));
            }
        }
    };
    collect(
        &published.shadowed,
        "a built-in tool already answers to this name",
    );
    collect(
        &published.ambiguous,
        "another configured server publishes the same name, so neither is offered",
    );
    collect(
        &published.unpublished,
        "the combined tool catalog reached its bound",
    );
    rows
}

#[cfg(test)]
#[path = "connections/tests.rs"]
mod tests;
