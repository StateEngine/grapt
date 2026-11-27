use super::super::legacy_names;
use super::*;

/// A callback reference a transform draft binds to a plugin-owned callback.
type CallbackRefs = BTreeMap<String, (String, u64)>;

/// Run one domain's transforms on the candidate. A transform may edit the
/// shared draft, but every callback reference it adds or changes must name
/// the transforming plugin itself.
fn transform(
    candidate: &CandidateGeneration,
    domain: &str,
    mut value: Value,
    refs: fn(&Value) -> CallbackRefs,
) -> Result<Value, String> {
    let generation = candidate.generation();
    for registration in generation.registrations() {
        for callback in registration
            .callbacks
            .iter()
            .filter(|callback| callback.domain == domain && callback.name == "transform")
        {
            let before = refs(&value);
            value = candidate
                .invoke_callback(&registration.plugin_id, callback.callback, value)
                .map_err(|error| error.to_string())?;
            refuse_foreign_binding(&before, &refs(&value), &registration.plugin_id, domain)?;
        }
    }
    Ok(value)
}

/// Every callback reference a transform adds or changes must name the
/// transforming plugin itself.
fn refuse_foreign_binding(
    before: &CallbackRefs,
    after: &CallbackRefs,
    plugin: &str,
    domain: &str,
) -> Result<(), String> {
    for (key, reference) in after {
        if before.get(key) != Some(reference) && reference.0 != plugin {
            return Err(format!(
                "plugin {plugin} {domain} transform bound {key} to a callback owned by plugin {}",
                reference.0
            ));
        }
    }
    Ok(())
}

fn callback_ref(value: Option<&Value>) -> Option<(String, u64)> {
    let value = value?;
    Some((
        value.get("plugin_id")?.as_str()?.to_owned(),
        value.get("callback")?.as_u64()?,
    ))
}

fn websearch_refs(value: &Value) -> CallbackRefs {
    value
        .get("providers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, row)| {
            let key = row.get("id").and_then(Value::as_str).map_or_else(
                || format!("provider #{index}"),
                |id| format!("provider {id}"),
            );
            callback_ref(Some(row)).map(|reference| (key, reference))
        })
        .collect()
}

fn integration_refs(value: &Value) -> CallbackRefs {
    let mut refs = CallbackRefs::new();
    for (index, row) in value
        .get("methods")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        for slot in ["authorize", "refresh", "label"] {
            if let Some(reference) = callback_ref(row.get(slot)) {
                refs.insert(format!("method #{index} {slot}"), reference);
            }
        }
    }
    refs
}

fn no_refs(_value: &Value) -> CallbackRefs {
    CallbackRefs::new()
}

pub(crate) fn materialize_websearch(
    candidate: &CandidateGeneration,
) -> Result<WebSearchRegistry, String> {
    const MAX_PROVIDERS: usize = 32;
    let value = transform(
        candidate,
        "websearch",
        json!({"providers":[],"default":null}),
        websearch_refs,
    )?;
    let rows = value
        .get("providers")
        .and_then(Value::as_array)
        .ok_or_else(|| "websearch transform returned a malformed provider registry".to_owned())?;
    if rows.len() > MAX_PROVIDERS {
        return Err(format!(
            "websearch provider registry exceeds the {MAX_PROVIDERS}-provider bound"
        ));
    }
    let mut ids = BTreeSet::new();
    let mut providers = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or_else(|| "websearch provider id is invalid".to_owned())?;
        if !ids.insert(id.to_owned()) {
            return Err(format!("websearch provider id is duplicated: {id}"));
        }
        let name = row
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty() && name.len() <= 512)
            .ok_or_else(|| format!("websearch provider {id} name is invalid"))?;
        let plugin_id = row
            .get("plugin_id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("websearch provider {id} has no plugin owner"))?;
        let callback = row
            .get("callback")
            .and_then(Value::as_u64)
            .filter(|callback| *callback > 0)
            .ok_or_else(|| format!("websearch provider {id} has no callback"))?;
        providers.push(WebSearchProvider {
            id: id.to_owned(),
            name: name.to_owned(),
            plugin_id: plugin_id.to_owned(),
            callback,
        });
    }
    let default = value
        .get("default")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(selected) = &default {
        if !providers.iter().any(|provider| &provider.id == selected) {
            return Err(format!(
                "websearch default names an unavailable provider: {selected}"
            ));
        }
    }
    Ok(WebSearchRegistry { providers, default })
}

pub(crate) fn materialize_integrations(
    candidate: &CandidateGeneration,
) -> Result<IntegrationRegistry, String> {
    const MAX_METHODS: usize = 64;
    const MAX_COMMAND_PARTS: usize = 32;
    let value = transform(
        candidate,
        "integration",
        json!({
            "items":[{"id":"openai","name":"OpenAI"}],
            "methods":[{"integrationID":"openai","method":{"type":"key","label":"API key"}}],
        }),
        integration_refs,
    )?;
    let rows = value
        .get("methods")
        .and_then(Value::as_array)
        .ok_or_else(|| "integration transform returned a malformed method registry".to_owned())?;
    if rows.len() > MAX_METHODS {
        return Err(format!(
            "integration method registry exceeds the {MAX_METHODS}-method bound"
        ));
    }
    let mut identities = BTreeSet::new();
    let mut methods = Vec::with_capacity(rows.len());
    for row in rows {
        let integration_id = bounded_json_string(row, "integrationID", 256)?;
        let method = row
            .get("method")
            .ok_or_else(|| format!("integration method for {integration_id} is absent"))?;
        let kind = bounded_json_string(method, "type", 32)?;
        let method_id = method
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(kind.as_str())
            .to_owned();
        if !identities.insert((integration_id.clone(), method_id.clone())) {
            return Err(format!(
                "integration method is duplicated: {integration_id}/{method_id}"
            ));
        }
        match kind.as_str() {
            "key" | "env" => methods.push(IntegrationMethod::Key { integration_id }),
            "oauth" => {
                let authorize = row.get("authorize").ok_or_else(|| {
                    format!("OAuth method {integration_id}/{method_id} has no implementation")
                })?;
                let plugin_id = bounded_json_string(authorize, "plugin_id", 256)?;
                let callback = authorize
                    .get("callback")
                    .and_then(Value::as_u64)
                    .filter(|callback| *callback > 0)
                    .ok_or_else(|| {
                        format!("OAuth method {integration_id}/{method_id} has no callback")
                    })?;
                methods.push(IntegrationMethod::Oauth {
                    integration_id,
                    method_id,
                    plugin_id,
                    authorize: callback,
                });
            }
            "command" => {
                let command = method
                    .get("command")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        format!("command method {integration_id}/{method_id} has no argv")
                    })?;
                if command.is_empty() || command.len() > MAX_COMMAND_PARTS {
                    return Err(format!(
                        "command method {integration_id}/{method_id} argv is outside the 1..={MAX_COMMAND_PARTS} bound"
                    ));
                }
                let command = command
                    .iter()
                    .map(|part| {
                        part.as_str()
                            .filter(|part| !part.is_empty() && part.len() <= 4096)
                            .map(str::to_owned)
                            .ok_or_else(|| {
                                format!("command method {integration_id}/{method_id} has an invalid argv part")
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                methods.push(IntegrationMethod::Command {
                    integration_id,
                    method_id,
                    command,
                });
            }
            _ => {
                return Err(format!(
                    "integration method {integration_id}/{method_id} has unsupported type {kind}"
                ));
            }
        }
    }
    Ok(IntegrationRegistry { methods })
}

pub(crate) fn materialize_commands(
    candidate: &CandidateGeneration,
    value: Value,
) -> Result<CommandRegistry, String> {
    const MAX_COMMANDS: usize = 128;
    const MAX_TEMPLATE_BYTES: usize = 256 * 1_024;
    let value = transform(candidate, "command", value, no_refs)?;
    let rows = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "command transform returned a malformed registry".to_owned())?;
    if rows.len() > MAX_COMMANDS {
        return Err(format!(
            "command registry exceeds the {MAX_COMMANDS}-command bound"
        ));
    }
    let mut commands = BTreeMap::new();
    for row in rows {
        let name = bounded_json_string(row, "name", 256)?;
        let template = row
            .get("template")
            .and_then(Value::as_str)
            .filter(|template| template.len() <= MAX_TEMPLATE_BYTES)
            .ok_or_else(|| format!("command {name} has no bounded template"))?
            .to_owned();
        let agent = row.get("agent").and_then(Value::as_str).map(str::to_owned);
        let model = row.get("model").cloned();
        if commands
            .insert(
                name.clone(),
                CommandDefinition {
                    template,
                    agent,
                    model,
                },
            )
            .is_some()
        {
            return Err(format!("command is duplicated after transform: {name}"));
        }
    }
    Ok(CommandRegistry { commands })
}

fn bounded_json_string(value: &Value, field: &'static str, bound: usize) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= bound)
        .map(str::to_owned)
        .ok_or_else(|| format!("{field} must contain 1..={bound} UTF-8 bytes"))
}

pub(crate) fn runtime(service_root: &Path) -> Result<BridgeRuntime, String> {
    let manifest = environment_path("GRAPT_PLUGIN_RUNTIME_MANIFEST", legacy_names::LEGACY_PLUGIN_RUNTIME_MANIFEST)?
        .or_else(packaged_runtime_manifest)
        .ok_or_else(|| {
            "configured plugins require the packaged plugin-runtime/runtime.json manifest or GRAPT_PLUGIN_RUNTIME_MANIFEST"
                .to_owned()
        })?;
    let pin = runtime_pin(&manifest)?;
    let executable = environment_path("GRAPT_PLUGIN_NODE", legacy_names::LEGACY_PLUGIN_NODE)?
        .or_else(|| packaged_node(service_root))
        .ok_or_else(|| {
            "configured plugins require the packaged Node runtime or GRAPT_PLUGIN_NODE".to_owned()
        })?;
    let bridge = environment_path("GRAPT_PLUGIN_BRIDGE", legacy_names::LEGACY_PLUGIN_BRIDGE)?
        .or_else(|| packaged_bridge(service_root))
        .ok_or_else(|| {
            "configured plugins require the packaged plugin-bridge/index.mjs asset or GRAPT_PLUGIN_BRIDGE"
                .to_owned()
        })?;
    Ok(BridgeRuntime {
        executable,
        bridge,
        private_root: service_root.join("plugin-runtime"),
        // Stable across runtime pins and outside the runtime directory that
        // runtime cleanup removes, so a crashed supervisor's bridges can be
        // reclaimed by its successor (SC-MAC-02b).
        recovery: grapt_plugin::RecoveryJournal {
            root: service_root.join("plugin-recovery"),
            clock: std::sync::Arc::new(grapt_store::SystemClock),
            ids: std::sync::Arc::new(
                grapt_store::OsRandomIds::seed().map_err(|error| error.to_string())?,
            ),
        },
        expected: RuntimePin {
            version: pin.version,
            executable_sha256: pin.executable_sha256,
            bridge_sha256: pin.bridge_sha256,
        },
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeManifest {
    schema: String,
    target: String,
    version: String,
    executable_sha256: String,
    bridge_sha256: String,
    source_url: String,
    source_sha256: String,
    license_sha256: String,
}

fn runtime_pin(path: &Path) -> Result<RuntimeManifest, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("plugin runtime manifest {}: {error}", path.display()))?;
    if bytes.len() > 64 * 1024 {
        return Err("plugin runtime manifest exceeds 64 KiB".to_owned());
    }
    let value: RuntimeManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("plugin runtime manifest {}: {error}", path.display()))?;
    if value.schema != "grapt.plugin-runtime.v1" {
        return Err(format!(
            "plugin runtime manifest schema {} is unsupported",
            value.schema
        ));
    }
    if value.target != runtime_target()? {
        return Err(format!(
            "plugin runtime target {} does not match {}",
            value.target,
            runtime_target()?
        ));
    }
    if !value.source_url.starts_with("https://nodejs.org/dist/v")
        || !valid_digest(&value.source_sha256)
        || !valid_digest(&value.license_sha256)
    {
        return Err("plugin runtime provenance is malformed".to_owned());
    }
    Ok(value)
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn runtime_target() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        (os, arch) => Err(format!(
            "plugin runtime has no qualified artifact for {os}/{arch}"
        )),
    }
}

/// The current override, or its pre-rename name only when the current one is
/// unset; the value read is reported, and its own name is used in refusals.
fn environment_path(current: &str, legacy: &'static str) -> Result<Option<PathBuf>, String> {
    let name = if std::env::var_os(current).is_none() && std::env::var_os(legacy).is_some() {
        legacy_names::report([format!(
            "using legacy environment variable {legacy}; rename to {current} when convenient"
        )]);
        legacy
    } else {
        current
    };
    match std::env::var_os(name) {
        Some(value) if value.is_empty() => Err(format!("{name} is empty")),
        Some(value) => Ok(Some(PathBuf::from(value))),
        None => Ok(None),
    }
}

fn packaged_node(_service_root: &Path) -> Option<PathBuf> {
    let parent = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let path = if cfg!(windows) {
        parent.join("plugin-runtime/node.exe")
    } else {
        parent.join("plugin-runtime/node")
    };
    path.is_file().then_some(path)
}

fn packaged_runtime_manifest() -> Option<PathBuf> {
    let path = std::env::current_exe()
        .ok()?
        .parent()?
        .join("plugin-runtime/runtime.json");
    path.is_file().then_some(path)
}

fn packaged_bridge(_service_root: &Path) -> Option<PathBuf> {
    let path = std::env::current_exe()
        .ok()?
        .parent()?
        .join("plugin-bridge/index.mjs");
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transform_cannot_bind_a_reference_to_another_plugins_callback() {
        let owned = json!({"providers":[{"id":"search","plugin_id":"plugin.a","callback":4}]});
        let before = websearch_refs(&owned);
        assert_eq!(
            refuse_foreign_binding(&before, &before, "plugin.b", "websearch"),
            Ok(())
        );
        let own_addition = json!({"providers":[{"id":"search","plugin_id":"plugin.a","callback":4},{"id":"mine","plugin_id":"plugin.b","callback":1}]});
        assert_eq!(
            refuse_foreign_binding(
                &before,
                &websearch_refs(&own_addition),
                "plugin.b",
                "websearch"
            ),
            Ok(())
        );
        let hijack = json!({"providers":[{"id":"search","plugin_id":"plugin.a","callback":9}]});
        let refused =
            refuse_foreign_binding(&before, &websearch_refs(&hijack), "plugin.b", "websearch");
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.contains("owned by plugin plugin.a")),
            "a transform rebound another plugin's provider callback: {refused:?}"
        );
        let methods = json!({"methods":[{"integrationID":"x","method":{"type":"oauth"},"authorize":{"plugin_id":"plugin.a","callback":2}}]});
        assert!(refuse_foreign_binding(
            &CallbackRefs::new(),
            &integration_refs(&methods),
            "plugin.b",
            "integration"
        )
        .is_err());
    }
}
