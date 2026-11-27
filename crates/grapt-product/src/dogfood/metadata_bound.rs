//! Keep generated tool metadata inside the reader's budget, marked when cut.
//!
//! A history row's metadata is generated from durable artifacts at read time.
//! The public reader refuses metadata over its own budget, so a row that named
//! a large edit diff could never be served. This bounds the projection where it
//! is produced and states what it dropped.
use serde_json::{json, Value};

/// The marker key both sides use, so a reader handles one vocabulary.
pub(super) const DROPPED: &str = "metadataDropped";

/// Metadata this row can carry. A projection that fits stays exact.
pub(super) fn bounded(mut metadata: Value) -> Value {
    if fits(&metadata) {
        return metadata;
    }
    // The files of an edit receipt carry a bounded diff preview each; several
    // together exceed the budget though no single one does. Nested tool calls
    // are the same. Replace each large optional field with its name, and say so.
    let mut omitted = Vec::new();
    for field in ["files", "toolCalls"] {
        if !fits(&metadata) && metadata.get(field).is_some() {
            if let Some(object) = metadata.as_object_mut() {
                object.remove(field);
                omitted.push(field);
            }
        }
    }
    let marker = json!({
        "reason": "generated metadata exceeded the reader's bound",
        "omitted": omitted,
    });
    if let Some(object) = metadata.as_object_mut() {
        object.insert(DROPPED.into(), marker.clone());
        if fits(&metadata) {
            return metadata;
        }
    }
    // Nothing left to cut explains the size, so the row keeps only the marker
    // rather than refusing its page. The marker still names what went.
    json!({ DROPPED: marker })
}

/// Whether the public reader accepts this metadata unchanged.
fn fits(metadata: &Value) -> bool {
    serde_json::from_value::<grapt_api::u3a::OpenObject>(metadata.clone()).is_ok()
}

#[cfg(test)]
#[path = "metadata_bound_tests.rs"]
mod tests;
