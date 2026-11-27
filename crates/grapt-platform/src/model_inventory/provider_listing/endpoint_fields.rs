//! Catalogue facts an endpoint listing row states beside the model identity.
//!
//! A row may carry `context_length`, `max_completion_tokens`,
//! `supported_parameters` and modality arrays. Enrichment is strictly additive
//! and best effort: a field the row does not report, or reports in a form no
//! consumer can use, stays absent so downstream surfaces record it as unknown,
//! never as zero or a denial. Nothing here may remove a fact the catalogue
//! already carried, refuse a listing, or panic on remote data. The one malformed
//! shape that is reported is a local catalogue's own `limit`, and that check
//! belongs to `admit_listing`, which reads the catalogue rather than this row.
use serde_json::{json, Value};

/// Copy the reported catalogue fields onto provider-listing metadata.
/// The context length is validated here but combined in `admit_listing`,
/// where the explicit window takes precedence over the endpoint's report.
pub(super) fn apply(metadata: &mut Value, row: &Value) -> Result<(), String> {
    if let Some(output) = optional_positive(row.get("max_completion_tokens")) {
        // A reported ceiling joins an existing limit object only. When the
        // stored limit is not an object, that is a local-catalogue fault the
        // caller reports; here the reported field simply stays unknown.
        if let Some(limit) = metadata.get_mut("limit").and_then(Value::as_object_mut) {
            limit.insert("output".to_owned(), json!(output));
        }
    }
    if let Some(parameters) = row.get("supported_parameters") {
        // A reported list is a statement about the request surface; a malformed
        // one is unusable, so the field stays unknown instead of refusing.
        if let Some(names) = bounded_strings(parameters, super::MAX_ACCOUNT_MODELS, 64) {
            metadata["tool_call"] = json!(names.contains(&"tools"));
        }
    }
    // Each side merges independently, so reporting one never erases the other.
    for side in ["input", "output"] {
        let Some(names) = row
            .get(format!("{side}_modalities"))
            .and_then(|value| bounded_strings(value, super::MAX_MODALITIES, 32))
            .filter(|names| !names.is_empty())
        else {
            continue;
        };
        if !metadata["modalities"].is_object() {
            metadata["modalities"] = json!({});
        }
        metadata["modalities"][side] = json!(names);
    }
    Ok(())
}

/// A reported limit must be a positive integer within what a consumer can read;
/// absence, and anything unreadable, stays absent.
fn optional_positive(value: Option<&Value>) -> Option<u64> {
    value?
        .as_u64()
        .filter(|value| *value > 0 && *value <= u32::MAX as u64)
}

fn bounded_strings(value: &Value, bound: usize, width: usize) -> Option<Vec<&str>> {
    value
        .as_array()
        .filter(|items| items.len() <= bound)
        .and_then(|items| items.iter().map(Value::as_str).collect())
        .filter(|names: &Vec<&str>| {
            names
                .iter()
                .all(|name| !name.is_empty() && name.len() <= width)
        })
}

#[cfg(test)]
mod tests;
