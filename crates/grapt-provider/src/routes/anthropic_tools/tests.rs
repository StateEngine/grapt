use super::*;

fn tool(schema: Value) -> ToolDefinition {
    ToolDefinition {
        input_format: crate::ToolInputFormat::Json,
        name: "fixture".into(),
        description: Some("Original tool description.".into()),
        input_schema: schema.to_string(),
    }
}

#[test]
fn every_combinator_projects_optional_branch_fields_and_retains_the_rule() {
    for keyword in COMBINATORS {
        let schema = json!({keyword:[
            {"type":"object","properties":{"left":{"type":"string"}},"required":["left"],"additionalProperties":false},
            {"type":"object","properties":{"right":{"type":"integer"}},"required":["right"],"additionalProperties":false}
        ]});
        let definition = tool(schema);
        let projected = project(&definition).unwrap_or_else(|error| panic!("{error}"));
        let flat = &projected["input_schema"];
        assert_eq!(flat["type"], "object");
        assert_eq!(flat["additionalProperties"], false);
        assert_eq!(flat["properties"]["left"]["type"], "string");
        assert_eq!(flat["properties"]["right"]["type"], "integer");
        assert!(flat.get("required").is_none());
        for key in COMBINATORS {
            assert!(flat.get(key).is_none());
        }
        let description = projected["description"].as_str().unwrap_or("");
        assert!(description.starts_with("Original tool description."));
        assert!(description.contains(&definition.input_schema));
    }
}

#[test]
fn an_open_alternative_keeps_permissive_property_values() {
    let projected = project(&tool(json!({"oneOf":[
        {"properties":{"mode":{"const":"first"}},"additionalProperties":false},
        {"properties":{"mode":{"const":"second"}}}
    ]})))
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(projected["input_schema"]["additionalProperties"], true);
    assert_eq!(projected["input_schema"]["properties"]["mode"], json!({}));
}

#[test]
fn flat_schemas_keep_their_original_bytes_and_nested_composites() {
    let original = json!({"type":"object","properties":{"input":{"anyOf":[{"type":"string"},{"type":"integer"}]}},"required":["input"],"additionalProperties":false});
    let projected = project(&tool(original.clone())).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(projected["input_schema"], original);
    assert_eq!(projected["description"], "Original tool description.");
}

#[test]
fn local_references_merge_branch_fields_without_erasing_definitions() {
    let projected = project(&tool(json!({"$defs":{"branch":{"properties":{"id":{"type":"string"}},"additionalProperties":false}},"allOf":[{"$ref":"#/$defs/branch"}]})))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        projected["input_schema"]["properties"]["id"]["type"],
        "string"
    );
    assert!(projected["input_schema"].get("$defs").is_some());
    assert_eq!(projected["input_schema"]["additionalProperties"], false);
}

#[test]
fn cyclic_external_malformed_and_oversized_composites_refuse_explicitly() {
    for schema in [
        json!({"oneOf":[{"$ref":"#"}]}),
        json!({"oneOf":[{"$ref":"https://example.invalid/schema"}]}),
        json!({"oneOf":[]}),
        json!({"anyOf":[{"properties":false}]}),
        json!({"oneOf":[{}],"description":"x".repeat(MAX_SCHEMA_BYTES)}),
    ] {
        assert!(project(&tool(schema)).is_err());
    }
}

#[test]
fn many_property_alternatives_keep_a_flat_bounded_union() {
    let branches: Vec<_> = (0..200)
        .map(|index| json!({"properties":{"mode":{"const":index}},"additionalProperties":false}))
        .collect();
    let projected =
        project(&tool(json!({"oneOf":branches}))).unwrap_or_else(|error| panic!("{error}"));
    let alternatives = projected["input_schema"]["properties"]["mode"]["anyOf"]
        .as_array()
        .unwrap_or_else(|| panic!("property alternatives"));
    assert_eq!(alternatives.len(), 200);
    assert!(alternatives
        .iter()
        .all(|value| value.get("const").is_some()));
}

#[test]
fn an_intersection_closes_even_when_the_root_allows_additional_properties() {
    let projected = project(&tool(json!({"type":"object","additionalProperties":true,
        "allOf":[{"properties":{"key":{"type":"string"}},"additionalProperties":false}]
    })))
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(projected["input_schema"]["additionalProperties"], false);
}

#[test]
fn branch_patterns_keep_dynamic_keys_in_the_wire_envelope() {
    let projected = project(&tool(json!({"oneOf":[{"type":"object",
        "patternProperties":{"^a":{"type":"string"}},"additionalProperties":false}]})))
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(projected["input_schema"]["additionalProperties"], true);
    assert!(projected["description"]
        .as_str()
        .unwrap_or("")
        .contains("patternProperties"));
}

#[test]
fn a_property_absent_from_an_open_alternative_does_not_gain_a_type_constraint() {
    let projected = project(&tool(json!({"oneOf":[
        {"properties":{"value":{"type":"string"}},"additionalProperties":false},
        {"properties":{"other":{"type":"integer"}},"required":["other"]}
    ]})))
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(projected["input_schema"]["additionalProperties"], true);
    assert_eq!(projected["input_schema"]["properties"]["value"], json!({}));
}

#[test]
fn root_constraints_remain_in_force_across_open_branches() {
    let projected = project(&tool(json!({"type":"object",
        "properties":{"query":{"type":"string"},"id":{"type":"integer"}},
        "additionalProperties":false,"oneOf":[{"required":["query"]},{"required":["id"]}]
    })))
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(projected["input_schema"]["additionalProperties"], false);
    assert_eq!(
        projected["input_schema"]["properties"]["query"]["type"],
        "string"
    );
    assert_eq!(
        projected["input_schema"]["properties"]["id"]["type"],
        "integer"
    );
}
