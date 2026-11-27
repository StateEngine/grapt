//! Export the existing volatile comparison before the service retires it.
use grapt_types::SessionId;
use serde_json::{json, Value};

pub(super) fn record(session: Option<&SessionId>, metadata: Option<&Value>) -> Value {
    // This allowlist is deliberately narrower than live history metadata:
    // counts and closed change labels, never prompt text, hashes or routing tokens.
    let metadata = metadata.map(|source| {
        let mut result = serde_json::Map::new();
        for key in [
            "cacheRequestChange",
            "cacheRequestChangedItem",
            "cacheRequestPreviousAttempt",
            "cacheRequestBodyBytes",
            "cacheRequestToolsBytes",
            "cacheRequestInstructionsBytes",
        ] {
            if let Some(value) = source.get(key) {
                result.insert(key.to_owned(), value.clone());
            }
        }
        Value::Object(result)
    });
    json!({"session":session.map(SessionId::as_str),"metadata":metadata})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_keeps_comparison_identity_and_excludes_unrelated_payloads() -> Result<(), String> {
        let session = "ses_cache_export"
            .parse()
            .map_err(|error| format!("{error}"))?;
        let metadata = json!({"cacheRequestChange":"input_changed",
            "cacheRequestChangedItem":"7", "cacheRequestPreviousAttempt":"attempt_before",
            "cacheRequestBodyBytes":1234,"cacheRequestToolsBytes":234,
            "cacheRequestInstructionsBytes":345,
            "futurePrivateField":"PRIVATE_SENTINEL",
            "cacheObservedCalls":{"future":"PRIVATE_SENTINEL"}});
        let exported = record(Some(&session), Some(&metadata));
        assert_eq!(exported["session"], "ses_cache_export");
        assert_eq!(exported["metadata"]["cacheRequestChange"], "input_changed");
        assert_eq!(exported["metadata"]["cacheRequestChangedItem"], "7");
        assert_eq!(
            exported["metadata"]["cacheRequestPreviousAttempt"],
            "attempt_before"
        );
        assert!(!exported.to_string().contains("PRIVATE_SENTINEL"));
        assert!(exported.to_string().len() < 1024);
        assert!(record(None, None)["metadata"].is_null());
        Ok(())
    }
}
