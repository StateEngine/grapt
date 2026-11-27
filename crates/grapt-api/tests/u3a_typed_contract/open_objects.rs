//! Distinct collection budgets for open metadata and admitted tool inputs.
use super::*;

#[test]
fn admitted_native_search_sources_survive_tool_history_without_relaxing_metadata() {
    let input = serde_json::json!({
        "type": "search", "queries": ["fixture search"],
        "sources": (0..1024).map(|n| serde_json::json!({
            "type": "url", "url": format!("https://example.test/source/{n}")
        })).collect::<Vec<_>>()
    });
    let decoded: grapt_api::u3a::ToolInputObject = must(
        serde_json::from_value(input.clone()),
        "admitted native search history input",
    );
    assert_eq!(
        must(serde_json::to_value(decoded), "tool input encoding"),
        input
    );
    assert!(
        serde_json::from_value::<grapt_api::u3a::OpenObject>(input).is_err(),
        "ordinary metadata keeps its existing smaller budget"
    );
    let over = serde_json::json!({"items": vec![true; 2049]});
    assert!(serde_json::from_value::<grapt_api::u3a::ToolInputObject>(over).is_err());
}

#[test]
fn open_metadata_has_finite_depth_key_node_scalar_and_byte_bounds() {
    let limits = production_limits();

    let mut nested = "0".to_owned();
    for _ in 0..9 {
        nested = format!("{{\"a\":{nested}}}");
    }
    let body = format!("{{\"text\":\"x\",\"metadata\":{nested}}}");
    assert!(decode_request::<PromptRequestDto>(body.as_bytes(), limits).is_err());

    let mut too_many_keys = serde_json::Map::new();
    for index in 0..65 {
        too_many_keys.insert(format!("k{index}"), Value::Bool(true));
    }
    let body = must(
        serde_json::to_vec(&serde_json::json!({"text": "x", "metadata": too_many_keys})),
        "encode metadata key mutant",
    );
    assert!(decode_request::<PromptRequestDto>(&body, limits).is_err());

    let too_long_key = "k".repeat(MAX_METADATA_KEY_BYTES + 1);
    let mut long_key = serde_json::Map::new();
    long_key.insert(too_long_key, Value::Bool(true));
    let body = must(
        serde_json::to_vec(&serde_json::json!({"text": "x", "metadata": long_key})),
        "encode metadata key-byte mutant",
    );
    assert!(decode_request::<PromptRequestDto>(&body, limits).is_err());

    let nodes = vec![Value::Bool(true); MAX_METADATA_NODES + 1];
    let body = must(
        serde_json::to_vec(&serde_json::json!({"text": "x", "metadata": {"nodes": nodes}})),
        "encode metadata node mutant",
    );
    assert!(decode_request::<PromptRequestDto>(&body, limits).is_err());

    let scalar = "x".repeat(MAX_METADATA_SCALAR_BYTES + 1);
    let body = must(
        serde_json::to_vec(&serde_json::json!({"text": "x", "metadata": {"scalar": scalar}})),
        "encode metadata scalar mutant",
    );
    assert!(decode_request::<PromptRequestDto>(&body, limits).is_err());

    let mut encoded_only = serde_json::Map::new();
    for index in 0..64 {
        let key = format!("{index:02}{}", "k".repeat(MAX_METADATA_KEY_BYTES - 2));
        encoded_only.insert(key, Value::Bool(true));
    }
    let body = must(
        serde_json::to_vec(&serde_json::json!({"text": "x", "metadata": encoded_only})),
        "encode metadata byte mutant",
    );
    assert!(decode_request::<PromptRequestDto>(&body, limits).is_err());
}
