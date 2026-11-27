//! Nullable discovery/loading fields remain usable under Responses strict mode.
use super::super::*;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::dogfood::tools) struct SkillArguments {
    pub id: Option<String>,
    pub file: Option<String>,
    pub arguments: Option<String>,
    pub query: Option<String>,
    pub cursor: Option<BoundedU64<0, 65_535>>,
}

impl ToolArguments for SkillArguments {
    const TOOL_NAME: &'static str = super::SKILL_TOOL;

    fn provider_schema() -> String {
        // Responses normalizes omitted strict settings by requiring every
        // property. Null expresses omission without inventing IDs or filenames.
        // Keep a flat object for Anthropic; no top-level union is needed.
        serde_json::json!({"type":"object", "additionalProperties":false,
            "properties":{
                "id":{"type":["string","null"],"description":"Skill ID to load; null when discovering."},
                "file":{"type":["string","null"],"description":"Relative resource to load; null for instructions or discovery."},
                "arguments":{"type":["string","null"],"maxLength":65536,"description":"Arguments for a Claude plugin skill; null for resources and discovery."},
                "query":{"type":["string","null"],"maxLength":256,"description":"Search text; null when loading or listing all."},
                "cursor":{"type":["integer","null"],"minimum":0,"maximum":65535,"description":"Discovery page; null when loading."}
            },"required":[]}).to_string()
    }
}

pub(super) fn nonblank(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_schema_can_omit_every_inactive_skill_field() -> Result<(), String> {
        let mut schema: serde_json::Value =
            serde_json::from_str(&SkillArguments::provider_schema())
                .map_err(|error| error.to_string())?;
        for keyword in ["anyOf", "oneOf", "allOf"] {
            assert!(schema.get(keyword).is_none());
        }
        schema["required"] = serde_json::json!(["id", "file", "query", "cursor"]);
        for value in [
            serde_json::json!({"id":null,"file":null,"query":"stateengine","cursor":null}),
            serde_json::json!({"id":"stateengine","file":null,"query":null,"cursor":null}),
            serde_json::json!({"id":null,"file":null,"query":null,"cursor":0}),
        ] {
            schema::validate(
                &schema.to_string(),
                &serde_json::to_vec(&value).map_err(|error| error.to_string())?,
            )?;
            let input = BoundedToolInput::new(value.to_string().into_bytes())
                .map_err(|error| error.to_string())?;
            super::super::prepare(Path::new("."), &input)?;
        }
        Ok(())
    }
    #[test]
    fn argument_shapes_have_actionable_exclusivity_or_blank_omission() -> Result<(), String> {
        // The eight actual failed calls: four per Session, including repeated queries.
        for (placeholder, queries) in [
            (
                ".",
                ["stateengine messages resume", "stateengine", "stateengine"],
            ),
            (
                "x",
                ["stateengine messages resume", "stateengine", "stateengine"],
            ),
        ] {
            for query in queries {
                let value = serde_json::json!({"id":placeholder,"file":placeholder,"query":query,"cursor":0});
                let input = BoundedToolInput::new(value.to_string().into_bytes())
                    .map_err(|e| e.to_string())?;
                let error = super::super::prepare(Path::new("."), &input)
                    .err()
                    .ok_or("mixed request accepted")?;
                assert_eq!(error, super::super::EXCLUSIVE);
            }
            let value = serde_json::json!({"id":" ","file":" ","query":"stateengine","cursor":0});
            let input =
                BoundedToolInput::new(value.to_string().into_bytes()).map_err(|e| e.to_string())?;
            assert!(matches!(
                super::super::prepare(Path::new("."), &input)?.target,
                EffectTarget::Skill(super::super::Request::Discover { .. })
            ));
        }
        for value in [
            serde_json::json!({"id":"","file":"","query":"","cursor":null}),
            serde_json::json!({}),
        ] {
            let input =
                BoundedToolInput::new(value.to_string().into_bytes()).map_err(|e| e.to_string())?;
            super::super::prepare(Path::new("."), &input)?;
        }
        Ok(())
    }
}
