//! Location-scoped catalog projection into ACP configuration options.

use serde_json::{json, Value};

use super::RpcFault;
use crate::ApiClient;

const MAX_CATALOG_ATTEMPTS: usize = 4;
/// The effort that leaves the level to the model's account default.
const DEFAULT_EFFORT: &str = "default";

#[derive(Debug, Clone)]
pub(crate) struct Catalog {
    models: Vec<Model>,
    modes: Vec<Mode>,
    pub(crate) default_model: ModelRef,
    pub(crate) default_mode: String,
    pub(crate) commands: Vec<Value>,
    pub(crate) skills: Vec<Value>,
}

#[derive(Debug, Clone)]
struct Model {
    provider: String,
    id: String,
    name: String,
    variants: Vec<String>,
}

#[derive(Debug, Clone)]
struct Mode {
    id: String,
    name: String,
    description: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ModelRef {
    pub(crate) provider: String,
    pub(crate) id: String,
    pub(crate) variant: Option<String>,
}

impl ModelRef {
    pub(crate) fn api_value(&self) -> Value {
        let mut value = json!({"providerID": self.provider, "id": self.id});
        if let Some(variant) = &self.variant {
            value["variant"] = json!(variant);
        }
        value
    }
}

impl Catalog {
    /// Whether discovery contains a name reserved by an ACP built-in. Used
    /// to explain refusal; catalog contents never override built-in dispatch.
    pub(crate) fn names(&self, name: &str) -> bool {
        self.commands
            .iter()
            .chain(self.skills.iter())
            .any(|value| value.get("name").and_then(Value::as_str) == Some(name))
    }

    pub(crate) async fn load(client: &ApiClient, cwd: &str) -> Result<Self, RpcFault> {
        let location = [("location[directory]", cwd)];
        let mut attempts = 0usize;
        let (default, models, agents, commands, skills) = loop {
            attempts = attempts.saturating_add(1);
            let fetched = tokio::try_join!(
                client.get_value_query("api/model/default", &location),
                client.get_value_query("api/model", &location),
                client.get_value_query("api/agent", &location),
                client.get_value_query("api/command", &location),
                client.get_value_query("api/skill", &location),
            );
            match fetched {
                Ok(values) => break values,
                Err(_error) if attempts < MAX_CATALOG_ATTEMPTS => {
                    // A finite scheduler handoff lets a just-composed catalog
                    // owner publish without introducing an ambient clock or
                    // an unbounded readiness loop.
                    tokio::task::yield_now().await;
                }
                Err(error) => return Err(remote(error)),
            }
        };

        let models = rows(&models)?
            .iter()
            .filter(|value| value.get("enabled").and_then(Value::as_bool) == Some(true))
            .map(model)
            .collect::<Result<Vec<_>, _>>()?;
        let modes = rows(&agents)?
            .iter()
            .filter(|value| {
                value.get("hidden").and_then(Value::as_bool) == Some(false)
                    && value.get("mode").and_then(Value::as_str) != Some("subagent")
            })
            .map(mode)
            .collect::<Result<Vec<_>, _>>()?;
        let default_model = default
            .get("data")
            .filter(|value| !value.is_null())
            .map(model_ref)
            .transpose()?
            .or_else(|| models.first().map(Model::reference))
            .ok_or_else(|| {
                RpcFault::internal("no enabled model is available for this directory")
            })?;
        let default_mode = modes
            .iter()
            .find(|mode| {
                rows(&agents).ok().is_some_and(|values| {
                    values.iter().any(|value| {
                        value.get("id").and_then(Value::as_str) == Some(mode.id.as_str())
                            && value.get("mode").and_then(Value::as_str) == Some("primary")
                    })
                })
            })
            .or_else(|| modes.first())
            .map(|mode| mode.id.clone())
            .ok_or_else(|| RpcFault::internal("no visible primary agent is available"))?;

        Ok(Self {
            models,
            modes,
            default_model,
            default_mode,
            commands: rows(&commands)?.to_vec(),
            skills: rows(&skills)?
                .iter()
                .filter(|value| value.get("slash").and_then(Value::as_bool) != Some(false))
                .cloned()
                .collect(),
        })
    }

    pub(crate) fn options(&self, model: &ModelRef, mode: &str) -> Vec<Value> {
        let mut providers = self
            .models
            .iter()
            .map(|candidate| candidate.provider.as_str())
            .collect::<Vec<_>>();
        providers.sort_unstable();
        providers.dedup();
        let model_options = providers
            .into_iter()
            .flat_map(|provider| {
                let mut models = self
                    .models
                    .iter()
                    .filter(|candidate| candidate.provider == provider)
                    .collect::<Vec<_>>();
                models.sort_by(|left, right| left.name.cmp(&right.name));
                models.into_iter().map(move |candidate| {
                    json!({
                        "value": format!("{}/{}", candidate.provider, candidate.id),
                        "name": format!("{}/{}", provider, candidate.name),
                    })
                })
            })
            .collect::<Vec<_>>();
        let mut options = vec![json!({
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": format!("{}/{}", model.provider, model.id),
            "options": model_options,
        })];
        if let Some(selected) = self.find_model(&model.provider, &model.id) {
            if !selected.variants.is_empty() {
                let current = model
                    .variant
                    .as_deref()
                    .filter(|value| selected.variants.iter().any(|candidate| candidate == value))
                    .unwrap_or(DEFAULT_EFFORT);
                // The model's own default is a choice of its own, not its first
                // level: the server issues the account's default reasoning
                // level for it.
                let default = (!selected
                    .variants
                    .iter()
                    .any(|variant| variant == DEFAULT_EFFORT))
                .then(|| json!({"value": DEFAULT_EFFORT, "name": "Default"}));
                options.push(json!({
                    "id": "effort",
                    "name": "Effort",
                    "description": "Available effort levels for this model",
                    "category": "thought_level",
                    "type": "select",
                    "currentValue": current,
                    "options": default.into_iter().chain(selected.variants.iter().map(|variant| json!({
                        "value": variant,
                        "name": variant_name(variant),
                    }))).collect::<Vec<_>>(),
                }));
            }
        }
        options.push(json!({
            "id": "mode",
            "name": "Session Mode",
            "category": "mode",
            "type": "select",
            "currentValue": mode,
            "options": self.modes.iter().map(|candidate| {
                let mut option = json!({"value": candidate.id, "name": candidate.name});
                if let Some(description) = &candidate.description {
                    option["description"] = json!(description);
                }
                option
            }).collect::<Vec<_>>(),
        }));
        options
    }

    pub(crate) fn select_model(&self, value: &str) -> Result<ModelRef, RpcFault> {
        let mut providers = self
            .models
            .iter()
            .map(|model| model.provider.as_str())
            .collect::<Vec<_>>();
        providers.sort_by_key(|provider| core::cmp::Reverse(provider.len()));
        providers.dedup();
        let (provider, tail) = providers
            .into_iter()
            .find_map(|provider| {
                value
                    .strip_prefix(&format!("{provider}/"))
                    .map(|tail| (provider, tail))
            })
            .ok_or_else(|| RpcFault::invalid_params("unknown model provider"))?;
        if let Some(model) = self.find_model(provider, tail) {
            return Ok(model.reference());
        }
        let (base, variant) = tail.rsplit_once('/').unwrap_or((tail, ""));
        let model = self
            .find_model(provider, base)
            .filter(|model| model.variants.iter().any(|candidate| candidate == variant))
            .ok_or_else(|| RpcFault::invalid_params("unknown model or effort"))?;
        let mut selected = model.reference();
        selected.variant = Some(variant.to_owned());
        Ok(selected)
    }

    pub(crate) fn select_effort(
        &self,
        model: &ModelRef,
        value: &str,
    ) -> Result<ModelRef, RpcFault> {
        let candidate = self
            .find_model(&model.provider, &model.id)
            .filter(|candidate| {
                value == DEFAULT_EFFORT || candidate.variants.iter().any(|variant| variant == value)
            })
            .ok_or_else(|| RpcFault::invalid_params("unknown effort for selected model"))?;
        let mut selected = candidate.reference();
        if value != DEFAULT_EFFORT {
            selected.variant = Some(value.to_owned());
        }
        Ok(selected)
    }

    pub(crate) fn has_mode(&self, value: &str) -> bool {
        self.modes.iter().any(|mode| mode.id == value)
    }

    fn find_model(&self, provider: &str, id: &str) -> Option<&Model> {
        self.models
            .iter()
            .find(|model| model.provider == provider && model.id == id)
    }
}

impl Model {
    fn reference(&self) -> ModelRef {
        ModelRef {
            provider: self.provider.clone(),
            id: self.id.clone(),
            // No effort is chosen until the user chooses one. Picking the
            // first level here sent every session "low" (benchmark rerun,
            // 2026-10-07) where Codex sends the account default.
            variant: self
                .variants
                .iter()
                .find(|variant| variant.as_str() == DEFAULT_EFFORT)
                .cloned(),
        }
    }
}

fn rows(value: &Value) -> Result<&[Value], RpcFault> {
    value
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| RpcFault::internal("catalog response is malformed"))
}

fn model(value: &Value) -> Result<Model, RpcFault> {
    Ok(Model {
        provider: string(value, "providerID")?,
        id: string(value, "id")?,
        name: string(value, "name")?,
        variants: value
            .get("variants")
            .and_then(Value::as_array)
            .ok_or_else(|| RpcFault::internal("model variants are malformed"))?
            .iter()
            .map(|variant| string(variant, "id"))
            .collect::<Result<_, _>>()?,
    })
}

fn model_ref(value: &Value) -> Result<ModelRef, RpcFault> {
    Ok(ModelRef {
        provider: string(value, "providerID")?,
        id: string(value, "id")?,
        variant: value
            .get("variant")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn mode(value: &Value) -> Result<Mode, RpcFault> {
    Ok(Mode {
        id: string(value, "id")?,
        name: string(value, "name")?,
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn string(value: &Value, key: &'static str) -> Result<String, RpcFault> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| RpcFault::internal(format!("catalog {key} is malformed")))
}

fn variant_name(value: &str) -> String {
    value
        .split(['_', '-'])
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

fn remote(error: crate::ApiClientError) -> RpcFault {
    RpcFault::internal(format!("ordinary API request failed: {error}"))
}
