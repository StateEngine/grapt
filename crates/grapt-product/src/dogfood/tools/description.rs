//! Display-only input: validate its bound; execution uses the original command.
use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(in crate::dogfood) struct ToolDescription;
impl<'de> Deserialize<'de> for ToolDescription {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        let value = String::deserialize(decoder)?;
        if value.chars().take(161).count() > 160 {
            return Err(D::Error::custom("tool description exceeds 160 characters"));
        }
        Ok(Self)
    }
}
impl ProviderFieldSchema for ToolDescription {
    fn provider_schema() -> serde_json::Value {
        serde_json::json!({"type":"string", "maxLength":160,
            "description":"Clear 5-10 word description of what this does, shown to the user."})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_descriptions_are_optional_flat_strings_with_a_unicode_bound(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for schema in [
            ShellArguments::provider_schema(),
            terminal::ExecArguments::provider_schema(),
        ] {
            let schema: serde_json::Value = serde_json::from_str(&schema)?;
            assert_eq!(schema["properties"]["description"]["type"], "string");
            assert_eq!(schema["properties"]["description"]["maxLength"], 160);
            assert!(!schema["required"]
                .as_array()
                .ok_or("required fields")?
                .iter()
                .any(|field| field == "description"));
            let encoded = schema.to_string();
            assert!(!encoded.contains("oneOf") && !encoded.contains("anyOf"));
        }
        for description in [
            None,
            Some("Check the current build and status".to_owned()),
            Some("中".repeat(160)),
        ] {
            let mut shell = serde_json::json!({"command":"echo unchanged"});
            let mut exec = serde_json::json!({"cmd":"echo unchanged"});
            if let Some(description) = description {
                shell["description"] = description.clone().into();
                exec["description"] = description.into();
            }
            assert_eq!(
                serde_json::from_value::<ShellArguments>(shell)?
                    .command
                    .as_str(),
                "echo unchanged"
            );
            assert_eq!(
                serde_json::from_value::<terminal::ExecArguments>(exec)?
                    .cmd
                    .as_str(),
                "echo unchanged"
            );
        }
        for description in [
            serde_json::Value::Null,
            serde_json::json!(3),
            serde_json::json!("中".repeat(161)),
        ] {
            assert!(serde_json::from_value::<ShellArguments>(
                serde_json::json!({"command":"echo unchanged", "description":description})
            )
            .is_err());
            assert!(serde_json::from_value::<terminal::ExecArguments>(
                serde_json::json!({"cmd":"echo unchanged", "description":description})
            )
            .is_err());
        }
        Ok(())
    }
}
