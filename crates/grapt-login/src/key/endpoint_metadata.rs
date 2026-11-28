//! Optional model metadata for explicit endpoint connections.
//!
//! An endpoint that publishes catalogue fields beside its model identity
//! states its own capacities. Discovery is best effort: it never
//! authenticates a key and never fails a connect. An unreported field stays
//! absent in the listing row, so downstream metadata records it as unknown
//! rather than as a measured zero or a denial.
use super::*;
use serde_json::{json, Value};

/// Whole-request bound for the optional metadata read. The completion check
/// keeps its own deadline; discovery must not hold a connect open longer.
const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const BYTE_CAP: usize = 4 * 1024 * 1024;
const MAX_ROWS: usize = 1024;
/// Catalogue modality lists are small; a longer list is not a model fact.
const MAX_MODALITIES: usize = 8;
/// The most reasoning levels one row may offer, as the model inventory bounds them.
const MAX_REASONING_LEVELS: usize = 32;
/// The output ceiling must fit both consumers: the model DTO carries it as an
/// i64, and the request-budget reader as a u32. A larger report is unusable, so
/// the field stays unknown rather than making the whole list unreadable.
const MAX_OUTPUT_CEILING: u64 = u32::MAX as u64;

/// Merge the endpoint's own catalogue row for `model` into a verified listing.
///
/// `listing` already carries the user's coordinates, including the explicit
/// context window, which always wins over any endpoint-reported context.
/// Every failure — unreachable endpoint, refusal, timeout, malformed body or
/// an absent model row — returns the listing unchanged.
pub async fn enrich_listing(verified: &VerifiedKey, model: &str) -> Vec<u8> {
    let listing = verified.listing().to_vec();
    match tokio::time::timeout(DISCOVERY_TIMEOUT, read_row(verified, model)).await {
        Ok(Some(fields)) => merge(listing, model, fields),
        Ok(None) | Err(_) => listing,
    }
}

async fn read_row(verified: &VerifiedKey, model: &str) -> Option<Value> {
    // The base and the guard are the verified key's own, never a caller's: a
    // key may only be sent where it was checked.
    let api_base = verified.base();
    let url = format!("{}/models", api_base.trim_end_matches('/'));
    verified.binding().check_api(&url).ok()?;
    let client = crate::chatgpt::bounded_client(api_base.starts_with("https://")).ok()?;
    let CredentialView::ApiKey(key) = verified.secret().expose() else {
        return None;
    };
    let mut response = client.get(&url).bearer_auth(key).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if chunk.len() > BYTE_CAP.saturating_sub(bytes.len()) {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    let parsed: Value = serde_json::from_slice(&bytes).ok()?;
    // The same two listing shapes the key check admits: an object with a
    // `data` array, or a bare array. A paginated body is not one bounded read.
    if parsed["has_more"].as_bool() == Some(true) {
        return None;
    }
    let rows = parsed["data"].as_array().or_else(|| parsed.as_array())?;
    if rows.len() > MAX_ROWS {
        return None;
    }
    let row = rows.iter().find(|row| row["id"].as_str() == Some(model))?;
    Some(fields(row))
}

/// The catalogue fields one listing row states, each kept only when the
/// endpoint actually reports it in a bounded, readable form.
pub(super) fn fields(row: &Value) -> Value {
    let mut fields = json!({});
    if let Some(context) = row
        .get("context_length")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
    {
        fields["context_length"] = json!(context);
    }
    if let Some(output) = row
        .get("top_provider")
        .and_then(|provider| provider.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .filter(|value| *value > 0 && *value <= MAX_OUTPUT_CEILING)
    {
        fields["max_completion_tokens"] = json!(output);
    }
    if let Some(parameters) = row.get("supported_parameters").and_then(Value::as_array) {
        if parameters.len() <= MAX_ROWS
            && parameters.iter().all(|value| {
                value
                    .as_str()
                    .is_some_and(|name| !name.is_empty() && name.len() <= 64)
            })
        {
            fields["supported_parameters"] = json!(parameters);
        }
    }
    for side in ["input", "output"] {
        if let Some(modalities) = stated_modalities(row, side) {
            fields[format!("{side}_modalities")] = json!(modalities);
        }
    }
    if let Some(levels) = reasoning_levels(row) {
        fields["supported_reasoning_levels"] = levels;
    }
    fields
}

/// The modalities a listing row states for one side ("input" or "output"),
/// read one way wherever a row is judged: the row's own list first, as a local
/// server states it, then the one under `architecture`. An empty list states
/// nothing, and a list outside its bounds is not a model fact.
pub fn stated_modalities<'a>(row: &'a Value, side: &str) -> Option<&'a Vec<Value>> {
    let name = format!("{side}_modalities");
    [
        row.get(&name),
        row.get("architecture")
            .and_then(|architecture| architecture.get(&name)),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| value.as_array().filter(|items| !items.is_empty()))
    .filter(|modalities| {
        modalities.len() <= MAX_MODALITIES
            && modalities.iter().all(|value| {
                value
                    .as_str()
                    .is_some_and(|name| !name.is_empty() && name.len() <= 32)
            })
    })
}

/// The reasoning efforts a row offers, each once in the row's order, kept
/// only when every level names one bounded effort.
fn reasoning_levels(row: &Value) -> Option<Value> {
    let levels = row.get("supported_reasoning_levels")?.as_array()?;
    if levels.len() > MAX_REASONING_LEVELS {
        return None;
    }
    let mut efforts = Vec::new();
    for level in levels {
        let effort = level["effort"]
            .as_str()
            .filter(|effort| !effort.is_empty() && effort.len() <= 32)?;
        if !efforts.contains(&effort) {
            efforts.push(effort);
        }
    }
    Some(json!(efforts
        .into_iter()
        .map(|effort| json!({ "effort": effort }))
        .collect::<Vec<_>>()))
}

/// Copy the reported fields onto the verified listing's own row for `model`.
/// The row's existing coordinates are never overwritten.
fn merge(listing: Vec<u8>, model: &str, fields: Value) -> Vec<u8> {
    let Some(object) = fields.as_object() else {
        return listing;
    };
    if object.is_empty() {
        return listing;
    }
    let merged = (|| -> Option<Vec<u8>> {
        let mut parsed: Value = serde_json::from_slice(&listing).ok()?;
        let row = parsed
            .get_mut("data")?
            .as_array_mut()?
            .iter_mut()
            .find(|row| row["id"].as_str() == Some(model))?;
        for (key, value) in object {
            if row.get(key).is_none() {
                row[key] = value.clone();
            }
        }
        let bytes = serde_json::to_vec(&parsed).ok()?;
        (bytes.len() <= BYTE_CAP).then_some(bytes)
    })();
    merged.unwrap_or(listing)
}

#[cfg(test)]
mod tests;
