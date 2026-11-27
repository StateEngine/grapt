//! Public input views summarize oversized native provenance; durable input stays exact.
use serde_json::{json, Value};

pub(super) fn public_metadata(mut value: Value) -> Value {
    if value.get("native_mail_wake").and_then(Value::as_u64) != Some(1)
        || serde_json::from_value::<grapt_api::u3a::OpenObject>(value.clone()).is_ok()
    {
        return value;
    }
    // A native wake may contain a full mailbox page of exact admission/digest
    // references. Its text already lists the source IDs; copying every reference
    // into open public metadata exceeds that DTO's aggregate key/scalar bounds.
    let Some(count) = value
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::len)
    else {
        return value;
    };
    if let Some(object) = value.as_object_mut() {
        object.remove("messages");
        object.insert("messageCount".into(), json!(count));
        object.insert("provenanceProjection".into(), json!("summary"));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_native_and_unrelated_metadata_stay_exact() {
        for metadata in [
            json!({"native_mail_wake":1,"messages":[{"source_message":"one"}]}),
            json!({"messages":[{"source_message":"one"}],"other":"value"}),
        ] {
            assert_eq!(public_metadata(metadata.clone()), metadata);
        }
    }
}
