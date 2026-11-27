//! Closed, bounded read queries emitted by the StateEngine resume handshake.
use super::ContinuityUnavailable as E;
use serde_json::Value;

pub(super) fn inbox_arguments(arguments: &Value) -> Result<(), E> {
    let fields = arguments.as_object().ok_or(E::Contract)?;
    for (key, value) in fields {
        let valid = match key.as_str() {
            "unread_only" => value.is_boolean(),
            "select" => matches!(value.as_str(), Some("newest" | "oldest")),
            // The producer bounds required inbox reads to twenty messages.
            "limit" => value.as_u64().is_some_and(|n| (1..=20).contains(&n)),
            "continuation" => value.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991),
            _ => false,
        };
        if !valid {
            return Err(E::Contract);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_the_producer_query_and_legacy_pages() -> Result<(), E> {
        for args in [
            json!({"unread_only":true,"select":"newest","limit":20}),
            json!({"unread_only":true}),
            json!({"select":"oldest","limit":1,"continuation":0}),
        ] {
            inbox_arguments(&args)?;
        }
        Ok(())
    }

    #[test]
    fn rejects_principal_overrides_unknown_fields_and_unbounded_queries() {
        for args in [
            json!({"agent_id":"foreign"}),
            json!({"project_id":"foreign"}),
            json!({"dismiss":true}),
            json!({"scope":"agent"}),
            json!({"select":"random"}),
            json!({"select":true}),
            json!({"unread_only":"true"}),
            json!({"limit":0}),
            json!({"limit":21}),
            json!({"limit":1.5}),
            json!({"limit":"20"}),
            json!({"continuation":-1}),
            json!({"continuation":1.5}),
            json!({"continuation":9_007_199_254_740_992_u64}),
            json!([]),
        ] {
            assert!(inbox_arguments(&args).is_err(), "accepted {args}");
        }
    }
}
