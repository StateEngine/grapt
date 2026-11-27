//! Effective image modality: explicit account data precedes catalog metadata.
use serde_json::Value;

pub(super) fn supports_images(metadata: &Value, account: Option<&Value>) -> Result<bool, String> {
    let input = match account {
        Some(input) => Some(input),
        None => match metadata.get("modalities") {
            None => None,
            Some(modalities) => modalities
                .as_object()
                .ok_or("model modalities is not an object")?
                .get("input"),
        },
    };
    let Some(input) = input else {
        return Ok(false);
    };
    let modalities: Vec<&str> = input
        .as_array()
        .filter(|input| !input.is_empty() && input.len() <= super::MAX_MODALITIES)
        .and_then(|input| input.iter().map(Value::as_str).collect())
        .ok_or("model input modalities is not a bounded string array")?;
    Ok(modalities.contains(&"image"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn explicit_account_modalities_override_stale_public_metadata() -> Result<(), String> {
        let public = json!({"modalities":{"input":["text","image"]}});
        assert!(supports_images(&public, None)?);
        assert!(!supports_images(&public, Some(&json!(["text"])))?);
        assert!(supports_images(
            &json!({}),
            Some(&json!(["text", "image"]))
        )?);
        assert!(!supports_images(&json!({}), None)?);
        Ok(())
    }

    #[test]
    fn malformed_consumed_modalities_are_not_absence() {
        for invalid in [Value::Null, json!([]), json!(["image", 1]), json!(true)] {
            assert!(supports_images(&json!({}), Some(&invalid)).is_err());
            assert!(supports_images(&json!({"modalities":{"input":invalid}}), None).is_err());
        }
        assert!(supports_images(&json!({"modalities":"unreadable"}), None).is_err());
        assert!(supports_images(
            &json!({}),
            Some(&json!([
                "image", "image", "image", "image", "image", "image", "image", "image", "image"
            ]))
        )
        .is_err());
    }
}
