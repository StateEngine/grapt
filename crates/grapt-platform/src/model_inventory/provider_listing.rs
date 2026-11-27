//! Metadata from an account's own provider listing, including local endpoints.
use super::*;

mod endpoint_fields;

impl ModelInventory {
    /// A generic OpenAI-compatible account needs no composed public catalog.
    /// Its bounded discovery snapshot still defines its own available models.
    pub(crate) fn load_provider_lists(
        root: &Path,
        accounts: &[InventoryAccount],
        selected: &ModelId,
        family: &ModelFamilyId,
        window: Option<u64>,
    ) -> Result<Self, String> {
        check_account_count(accounts.len())?;
        let mut public = match std::fs::symlink_metadata(root.join("model-catalog.json")) {
            Ok(metadata) if metadata.is_file() => read(root, "model-catalog.json")?,
            Ok(_) => return Err("model-catalog.json is not a regular file".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(error) => return Err(format!("model-catalog.json is unreadable: {error}")),
        };
        let providers = public
            .as_object_mut()
            .ok_or("model catalog must be an object")?;
        for account in accounts {
            let id = account.catalog_provider.as_str();
            if id == "cloudflare-ai-gateway" && !providers.contains_key(id) {
                providers.insert(
                    id.into(),
                    grapt_connections::catalog::cloudflare::metadata()?,
                );
            }
            providers
                .entry(id.to_owned())
                .or_insert_with(|| serde_json::json!({"id":id,"name":id,"models":{}}));
        }
        let mut inventory = Self::load_with_public(root, public, accounts, window)?;
        if let Some(primary) = accounts.first() {
            for model in &mut inventory.available {
                if model.provider() == &primary.provider
                    && model.id() == selected
                    && model.metadata.get("family").is_none()
                {
                    model.metadata["family"] = family.as_str().into();
                    model.metadata["family_source"] = "configuration".into();
                }
            }
        }
        Ok(inventory)
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn admit_listing(
        provider: &ProviderId,
        models: &serde_json::Map<String, Value>,
        account: &Value,
        available: &mut Vec<ConnectedModel>,
    ) -> Result<(), String> {
        if account.get("has_more").and_then(Value::as_bool) == Some(true) {
            return Err("account model listing continues past one page".into());
        }
        let listed = account
            .get("data")
            .and_then(Value::as_array)
            .ok_or("account model listing must contain a data array")?;
        if listed.len() > MAX_ACCOUNT_MODELS {
            return Err("account model count exceeds its bound".into());
        }
        let mut seen = BTreeSet::new();
        for row in listed {
            let raw = row
                .get("id")
                .and_then(Value::as_str)
                .ok_or("account model has no identity")?;
            let id = ModelId::new(raw).ok_or("account model identity is invalid")?;
            if !seen.insert(raw) {
                return Err(format!("account discovery repeats model {raw}"));
            }
            let catalog_id = match row.get("catalog_model_id") {
                Some(value)
                    if matches!(provider.as_str(), "azure" | "azure-cognitive-services") =>
                {
                    value
                        .as_str()
                        .filter(|id| !id.is_empty() && id.len() <= 256)
                        .ok_or("Azure deployment catalog identity is invalid")?
                }
                _ => raw,
            };
            let mut metadata = match models.get(catalog_id) {
                Some(value) if value.get("id").and_then(Value::as_str) == Some(catalog_id) => {
                    value.clone()
                }
                Some(_) => return Err(format!("model catalog identity disagrees with key {raw}")),
                None if matches!(provider.as_str(), "azure" | "azure-cognitive-services")
                    && row.get("catalog_model_id").is_some() =>
                {
                    return Err("Azure deployment model is absent from catalog".into())
                }
                None => unknown_metadata(raw, row)?,
            };
            if catalog_id != raw {
                metadata["id"] = raw.into();
                metadata["catalog_model_id"] = catalog_id.into();
            }
            // A listing row may state its own catalogue facts beside the
            // identity; only reported fields become metadata.
            endpoint_fields::apply(&mut metadata, row)?;
            let catalog_context = match metadata.get("limit") {
                None | Some(Value::Null) => None,
                Some(limit) => {
                    let value = limit
                        .as_object()
                        .ok_or("model limits is not an object")?
                        .get("context");
                    // models.dev uses zero for image-only models with no
                    // reported text context. Keep them listed, never runnable
                    // on an invented capacity.
                    if value.and_then(Value::as_u64) == Some(0) {
                        None
                    } else {
                        positive(value, raw, "catalog context limit")?
                    }
                }
            };
            let live_context = positive(row.get("max_model_len"), raw, "provider max_model_len")?;
            // An endpoint-reported context length never widens an explicitly
            // configured window; it can only tighten it, like a catalog limit.
            // A malformed report is remote data, so that field stays unknown
            // rather than refusing a listing that was already valid.
            let reported_context =
                optional_positive(row.get("context_length")).filter(|value| *value > 0);
            let context = match (catalog_context, live_context) {
                (Some(catalog), Some(live)) => Some(catalog.min(live)),
                (catalog, live) => live.or(catalog),
            };
            let context = match (context, reported_context) {
                (Some(known), Some(reported)) => Some(known.min(reported)),
                (known, reported) => known.or(reported),
            };
            if let Some(context) = context {
                if metadata.get("limit").is_none_or(Value::is_null) {
                    metadata["limit"] = serde_json::json!({});
                }
                metadata["limit"]["context"] = context.into();
            }
            if available.len() >= MAX_ISSUABLE_RECORDS {
                return Err("configured accounts exceed the issuable model bound".into());
            }
            let variants = reasoning::offered(provider.as_str(), raw, row)?;
            let default_variant = reasoning::default_for(provider.as_str(), raw, row, &variants)?;
            let image_input = image_capabilities::supports_images(&metadata, None)?;
            available.push(ConnectedModel {
                provider: provider.clone(),
                id,
                metadata,
                context,
                variants,
                default_variant,
                responses_lite: false,
                summary_parameter: false,
                effort_updates: false,
                thinking: if provider.as_str() == "google-vertex-anthropic" {
                    thinking::mode(raw.split_once('@').map_or(raw, |(native, _)| native), row)?
                } else if provider.as_str() == "anthropic" {
                    thinking::mode(raw, row)?
                } else {
                    None
                },
                original_image_detail: false,
                image_input,
            });
        }
        if listed.is_empty() {
            return Err("connected account has no listed models".into());
        }
        Ok(())
    }
}

/// A remote optional value no consumer can read stays absent.
fn optional_positive(value: Option<&Value>) -> Option<u64> {
    value?.as_u64()
}

fn positive(value: Option<&Value>, model: &str, field: &str) -> Result<Option<u64>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| format!("model {model} has an invalid {field}")),
    }
}

fn unknown_metadata(model: &str, row: &Value) -> Result<Value, String> {
    let name = match row.get("display_name") {
        None => model,
        Some(value) => value
            .as_str()
            .filter(|name| !name.is_empty() && name.len() <= 256)
            .ok_or_else(|| format!("model {model} has an invalid display name"))?,
    };
    // Missing modalities/tool flags project as null in the public model DTO.
    // A listing's id and context length do not establish other capabilities.
    Ok(
        serde_json::json!({"id":model,"name":name,"metadata_source":"provider-listing",
        "limit":{"context":null,"output":null}}),
    )
}

#[cfg(test)]
mod tests;
