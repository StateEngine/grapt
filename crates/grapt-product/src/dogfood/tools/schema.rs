//! Offline validation against the exact published schema, before approval.
//! Static expansion and input work are bounded independently of regex limits.
use serde_json::Value;

const NODES: usize = 4096;
const DEPTH: usize = 32;
const WORK: usize = 1_048_576;
const REGEX_BYTES: usize = 64 * 1024;
const PATTERNS: usize = 16;

pub(super) fn validate_published(
    published: &crate::dogfood::mcp::PublishedTools,
    name: &str,
    input: &[u8],
) -> Result<(), &'static str> {
    if let Some(definition) = published
        .definitions
        .iter()
        .find(|entry| entry.name == name)
    {
        validate(&definition.input_schema, input)
    } else if published.routes.contains_key(name) {
        Err("published tool definition is absent")
    } else {
        // Built-ins use their strict Rust argument decoders; unknown names
        // still reach the existing unknown-tool refusal.
        Ok(())
    }
}

pub(super) fn validate(schema: &str, input: &[u8]) -> Result<(), &'static str> {
    if schema.len().saturating_mul(input.len()) > 16 * 1024 * 1024 {
        return Err("schema/input byte-work bound exceeded");
    }
    let schema = decode(schema.as_bytes())?;
    let input = decode(input)?;
    let mut expansion = Expansion {
        root: &schema,
        nodes: 0,
        patterns: 0,
        refs: Vec::new(),
    };
    expansion.schema(&schema, 0)?;
    let input_nodes = count(&input, 0, &mut 0)?;
    // Covers pairwise comparisons (uniqueItems) as well as schema branches.
    if expansion
        .nodes
        .saturating_mul(input_nodes)
        .saturating_mul(input_nodes)
        > WORK
    {
        return Err("schema validation work bound exceeded");
    }
    let validator = jsonschema::options()
        .offline()
        .with_pattern_options(
            jsonschema::PatternOptions::regex()
                .size_limit(REGEX_BYTES)
                .dfa_size_limit(REGEX_BYTES),
        )
        .build(&schema)
        .map_err(|_| "tool schema is unsupported or invalid")?;
    if validator.is_valid(&input) {
        Ok(())
    } else {
        Err("arguments do not match the tool schema")
    }
}

fn decode(bytes: &[u8]) -> Result<Value, &'static str> {
    if bytes.len() > grapt_codemode::ARGUMENT_BYTES {
        return Err("schema/input byte bound exceeded");
    }
    let limits = grapt_schema::DecodeLimits::new(grapt_codemode::ARGUMENT_BYTES, DEPTH)
        .map_err(|_| "schema decode limits invalid")?;
    let value =
        grapt_schema::decode_json(bytes, limits).map_err(|_| "schema/input JSON invalid")?;
    count(&value, 0, &mut 0)?;
    Ok(value)
}

fn count(value: &Value, depth: usize, visited: &mut usize) -> Result<usize, &'static str> {
    if depth > DEPTH || *visited >= NODES {
        return Err("schema/input structure bound exceeded");
    }
    *visited += 1;
    match value {
        Value::Object(fields) => {
            for child in fields.values() {
                count(child, depth + 1, visited)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                count(child, depth + 1, visited)?;
            }
        }
        _ => {}
    }
    Ok(*visited)
}

struct Expansion<'a> {
    root: &'a Value,
    nodes: usize,
    patterns: usize,
    refs: Vec<&'a str>,
}

impl<'a> Expansion<'a> {
    fn schema(&mut self, value: &'a Value, depth: usize) -> Result<(), &'static str> {
        if depth > DEPTH || self.nodes >= NODES {
            return Err("schema expansion bound exceeded");
        }
        self.nodes += 1;
        if value.is_boolean() {
            return Ok(());
        }
        let object = value
            .as_object()
            .ok_or("schema must be object or boolean")?;
        for (key, child) in object {
            match key.as_str() {
                "$schema" => {
                    let dialect = child.as_str().ok_or("schema dialect invalid")?;
                    if ![
                        "https://json-schema.org/draft/2020-12/schema",
                        "https://json-schema.org/draft/2019-09/schema",
                        "http://json-schema.org/draft-07/schema#",
                        "http://json-schema.org/draft-06/schema#",
                        "http://json-schema.org/draft-04/schema#",
                    ]
                    .contains(&dialect)
                    {
                        return Err("schema dialect unsupported");
                    }
                }
                "$ref" => {
                    let reference = child.as_str().ok_or("schema reference invalid")?;
                    let pointer = reference
                        .strip_prefix('#')
                        .ok_or("external schema reference refused")?;
                    if pointer.contains('%')
                        || (!pointer.is_empty() && !pointer.starts_with('/'))
                        || self.refs.contains(&reference)
                    {
                        return Err("cyclic or unsupported schema reference");
                    }
                    let target = self
                        .root
                        .pointer(pointer)
                        .ok_or("schema reference missing")?;
                    self.refs.push(reference);
                    self.schema(target, depth + 1)?;
                    self.refs.pop();
                }
                "$defs" | "definitions" | "properties" | "patternProperties"
                | "dependentSchemas" => {
                    let children = child.as_object().ok_or("schema map invalid")?;
                    for nested in children.values() {
                        if key == "patternProperties" {
                            self.pattern()?;
                        }
                        self.schema(nested, depth + 1)?;
                    }
                }
                "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                    for nested in child.as_array().ok_or("schema array invalid")? {
                        self.schema(nested, depth + 1)?;
                    }
                }
                "items" if child.is_array() => {
                    for nested in child.as_array().ok_or("schema items invalid")? {
                        self.schema(nested, depth + 1)?;
                    }
                }
                "items"
                | "additionalItems"
                | "additionalProperties"
                | "unevaluatedProperties"
                | "unevaluatedItems"
                | "contains"
                | "propertyNames"
                | "not"
                | "if"
                | "then"
                | "else" => self.schema(child, depth + 1)?,
                "pattern" => self.pattern()?,
                // These assertions are implemented by jsonschema. Annotation
                // values are data, not recursively interpreted schemas.
                "type" | "enum" | "const" | "required" | "dependentRequired" | "minimum"
                | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" | "multipleOf"
                | "minLength" | "maxLength" | "minItems" | "maxItems" | "uniqueItems"
                | "minContains" | "maxContains" | "minProperties" | "maxProperties" | "title"
                | "description" | "default" | "examples" | "$comment" | "readOnly"
                | "writeOnly" | "deprecated" => {}
                // No silent remote rebasing, dynamic recursion, format or
                // content assertion omitted by validator configuration.
                _ => return Err("unsupported schema keyword"),
            }
        }
        Ok(())
    }
    fn pattern(&mut self) -> Result<(), &'static str> {
        if self.patterns >= PATTERNS {
            return Err("schema pattern count exceeded");
        }
        self.patterns += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn acyclic_local_reference_validates_before_dispatch() {
        let schema = json!({"type":"object","required":["count"],"additionalProperties":false,
            "$defs":{"count":{"type":"integer","minimum":1}},"properties":{"count":{"$ref":"#/$defs/count"}}}).to_string();
        assert_eq!(validate(&schema, br#"{"count":2}"#), Ok(()));
        for input in [br#"{"count":"2"}"#.as_slice(), br#"{}"#, br#"{"count":0}"#] {
            assert!(validate(&schema, input).is_err());
        }
    }

    #[test]
    fn cyclic_external_dynamic_and_backtracking_patterns_refuse() {
        for schema in [
            json!({"$ref":"#"}),
            json!({"$ref":"file:///secret"}),
            json!({"$dynamicRef":"#x"}),
            json!({"pattern":r"(?<=x)y"}),
        ] {
            assert!(validate(&schema.to_string(), br#""xy""#).is_err());
        }
        let schema =
            json!({"allOf":vec![json!({"type":"array","uniqueItems":true});128]}).to_string();
        assert!(validate(&schema, json!(vec![0; 128]).to_string().as_bytes()).is_err());
    }

    #[test]
    fn adversarial_schema_expansion_regex_and_pairwise_work_are_bounded() {
        let mut definitions = serde_json::Map::new();
        definitions.insert("s0".into(), json!(true));
        for level in 1..=4 {
            let reference = json!({"$ref":format!("#/$defs/s{}", level - 1)});
            definitions.insert(format!("s{level}"), json!({"allOf":vec![reference;16]}));
        }
        let expansion = json!({"$defs":definitions,"$ref":"#/$defs/s4"});
        assert_eq!(
            validate(&expansion.to_string(), b"null"),
            Err("schema expansion bound exceeded")
        );
        let patterns = json!({"allOf":vec![json!({"pattern":"a"});17]});
        assert_eq!(
            validate(&patterns.to_string(), br#""a""#),
            Err("schema pattern count exceeded")
        );
        assert_eq!(
            validate(r#"{"pattern":"(a|b){100000}"}"#, br#""a""#),
            Err("tool schema is unsupported or invalid")
        );
        let near_match = serde_json::Value::String(format!("{}!", "a".repeat(4096)));
        assert_eq!(
            validate(
                r#"{"pattern":"^(a+)+$"}"#,
                near_match.to_string().as_bytes()
            ),
            Err("arguments do not match the tool schema")
        );
        let schema = r#"{"type":"array","uniqueItems":true}"#;
        let values: Vec<_> = (0..64).collect();
        assert_eq!(
            validate(schema, json!(values).to_string().as_bytes()),
            Ok(())
        );
        let mut duplicate = values;
        duplicate.push(0);
        assert_eq!(
            validate(schema, json!(duplicate).to_string().as_bytes()),
            Err("arguments do not match the tool schema")
        );
        assert_eq!(
            validate(schema, json!(vec![0; 2048]).to_string().as_bytes()),
            Err("schema validation work bound exceeded")
        );
    }
}

#[cfg(test)]
mod anthropic_original {
    use super::*;

    #[test]
    fn atlas_original_schema_still_rejects_ambiguous_and_missing_modes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let catalog: Value = serde_json::from_str(include_str!(
            "../../../tests/support/stateengine-core-tools.json"
        ))?;
        let tools = catalog["tools"].as_array().ok_or("fixture tools")?;
        let atlas = tools
            .iter()
            .find(|tool| tool["name"] == "stateengine_atlas_query")
            .ok_or("Atlas tool")?;
        let original = atlas["inputSchema"].to_string();
        assert!(validate(&original, br#"{"project_id":"fixture","query":"decision"}"#).is_ok());
        assert!(validate(
            &original,
            br#"{"project_id":"fixture","queries":[{"id":"one","query":"decision"}]}"#
        )
        .is_ok());
        for invalid in [
            br#"{"project_id":"fixture"}"#.as_slice(),
            br#"{"project_id":"fixture","query":"decision","queries":[{"id":"one","query":"decision"}]}"#,
            br#"{"project_id":"fixture","query":"decision","unexpected":true}"#,
        ] { assert!(validate(&original, invalid).is_err()); }
        Ok(())
    }
}

#[cfg(test)]
mod published_input {
    use super::*;
    #[test]
    fn a_plugin_without_an_mcp_route_uses_the_original_composite_schema() {
        let mut published = crate::dogfood::mcp::PublishedTools::default();
        published.definitions.push(grapt_provider::ToolDefinition {
            input_format: grapt_provider::ToolInputFormat::Json,
            name: "plugin_lookup".into(), description: None,
            input_schema: r#"{"type":"object","properties":{"query":{"type":"string"},"id":{"type":"integer"}},"additionalProperties":false,"oneOf":[{"required":["query"]},{"required":["id"]}]}"#.into(),
        });
        assert!(published.routes.is_empty());
        assert!(validate_published(&published, "plugin_lookup", br#"{"query":"record"}"#).is_ok());
        assert!(validate_published(&published, "plugin_lookup", br#"{"id":7}"#).is_ok());
        for invalid in [
            br#"{}"#.as_slice(),
            br#"{"id":7,"query":"record"}"#,
            br#"{"query":7}"#,
        ] {
            assert!(validate_published(&published, "plugin_lookup", invalid).is_err());
        }
    }
}
