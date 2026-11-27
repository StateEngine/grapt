//! Read-only model preflight before the CLI stages any credential or parent.
use std::io::Read;

use serde::Deserialize;
use serde_json::Value;

const SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;
const INPUT_BYTES: u64 = 2 * SNAPSHOT_BYTES + 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshots {
    catalog: Value,
    listing: Value,
    model: String,
    provider: String,
    source: String,
    family: String,
}

pub(crate) fn check(input: &mut impl Read) -> Result<(), String> {
    let mut bytes = Vec::new();
    input
        .take(INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "model preflight input unreadable")?;
    if bytes.len() as u64 > INPUT_BYTES {
        return Err("model preflight exceeds its input bound".into());
    }
    let snapshots: Snapshots =
        serde_json::from_slice(&bytes).map_err(|_| "model preflight input is malformed")?;
    for snapshot in [&snapshots.catalog, &snapshots.listing] {
        if serde_json::to_vec(snapshot)
            .map_err(|_| "snapshot encoding failed")?
            .len() as u64
            > SNAPSHOT_BYTES
        {
            return Err("model snapshot exceeds its byte bound".into());
        }
    }
    let inventory = grapt_platform::ModelInventory::first_run(
        snapshots.catalog,
        snapshots.listing,
        &snapshots.provider,
        &snapshots.source,
        &snapshots.family,
    )?;
    let selected = inventory
        .available()
        .iter()
        .find(|row| row.id().as_str() == snapshots.model)
        .ok_or("chosen first-run model is not issuable")?;
    if selected.context().is_none() {
        return Err("chosen first-run model inventory is missing a positive context limit".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sc_fr01_preflight_accepts_live_context_and_names_missing_selected_metadata(
    ) -> Result<(), String> {
        let mut request = json!({"catalog":{"openai":{"id":"openai","models":{}}},
            "listing":{"data":[{"id":"qwen3.8-27b","max_model_len":200000},{"id":"unknown-model"}]},
            "model":"qwen3.8-27b","provider":"openai","source":"models.dev-openai","family":"openai-responses"});
        let bytes = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
        check(&mut bytes.as_slice())?;
        request["model"] = "unknown-model".into();
        let bytes = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
        let error = check(&mut bytes.as_slice())
            .err()
            .ok_or("missing context was accepted")?;
        assert!(error.contains("inventory") && error.contains("context limit"));
        Ok(())
    }

    #[test]
    fn read_only_preflight_uses_publication_rules() -> Result<(), String> {
        let catalog = json!({"openai":{"id":"openai","models":{}}});
        let listing = json!({"models":[{"slug":"gpt-new","visibility":"list","context_window":1000,"supported_reasoning_levels":[]}]});
        let request = |catalog: Value, listing: Value| {
            serde_json::to_vec(&json!({"catalog":catalog,"listing":listing,"model":"gpt-new","provider":"openai","source":"models.dev-chatgpt","family":"chatgpt-codex"}))
                .map_err(|error| error.to_string())
        };
        check(&mut request(catalog.clone(), listing.clone())?.as_slice())?;
        for catalog in [
            Value::Null,
            json!({}),
            json!({"openai":{"id":"wrong","models":{}}}),
            json!({"openai":{"id":"openai","models":[]}}),
        ] {
            assert!(check(&mut request(catalog, listing.clone())?.as_slice()).is_err());
        }
        for field in ["context_window", "supported_reasoning_levels"] {
            let mut malformed = listing.clone();
            malformed["models"][0]
                .as_object_mut()
                .ok_or("fixture model")?
                .remove(field);
            assert!(check(&mut request(catalog.clone(), malformed)?.as_slice()).is_err());
        }
        Ok(())
    }
    #[test]
    fn api_preflight_requires_context_for_the_selected_listed_model() -> Result<(), String> {
        for pair in grapt_platform::COMPOSED_CATALOGS
            .iter()
            .filter(|pair| pair.family != "chatgpt-codex")
        {
            let catalog = json!({pair.provider:{"id":pair.provider,"models":{
                "model-one":{"id":"model-one","family":"family-one","limit":{"context":1000}}
            }}});
            let listing = json!({"data":[{"id":"model-one"},{"id":"embedding-unknown"}]});
            let request = json!({"catalog":catalog,"listing":listing,"model":"model-one",
                "provider":pair.provider,"source":pair.source,"family":pair.family});
            let encode =
                |request: &Value| serde_json::to_vec(request).map_err(|error| error.to_string());
            check(&mut encode(&request)?.as_slice())?;
            let mut unavailable = request.clone();
            unavailable["model"] = json!("embedding-unknown");
            assert!(check(&mut encode(&unavailable)?.as_slice()).is_err());
            let mut wrong_pair = request.clone();
            wrong_pair["family"] = json!("chatgpt-codex");
            assert!(check(&mut encode(&wrong_pair)?.as_slice()).is_err());
            for bad in [json!(null), json!("unreadable"), json!(0)] {
                let mut invalid = request.clone();
                invalid["catalog"][pair.provider]["models"]["model-one"]["limit"]["context"] = bad;
                // An unrelated field cannot repair absent or invalid context metadata.
                invalid["listing"]["data"][0]["context_window"] = json!(1000);
                assert!(check(&mut encode(&invalid)?.as_slice()).is_err());
            }
        }
        Ok(())
    }
}
