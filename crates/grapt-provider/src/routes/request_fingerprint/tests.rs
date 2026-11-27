use super::*;
use serde_json::json;
fn fingerprint(body: &Value) -> Result<RequestFingerprint, String> {
    RequestFingerprint::of(body, &["route", "account-version"], body.to_string().len())
        .ok_or_else(|| "request fingerprint unavailable".to_owned())
}
fn request() -> Value {
    json!({"model":"test-model", "tools":[{"name":"read","parameters":{"type":"object"}}],
        "prompt_cache_key":"session", "instructions":"fixed", "reasoning":{"effort":"medium"},
        "input":[{"role":"user","content":"first"},{"role":"assistant","content":"second"}]})
}
#[test]
fn first_changed_component_and_input_index_are_from_the_wire() -> Result<(), String> {
    let body = request();
    let prior = fingerprint(&body)?;
    for (key, value, expected) in [
        ("model", json!("new-model"), RequestDelta::TargetChanged),
        (
            "prompt_cache_key",
            json!("new-key"),
            RequestDelta::CacheKeyChanged,
        ),
        ("tools", json!([]), RequestDelta::ToolsChanged),
        (
            "instructions",
            json!("changed"),
            RequestDelta::InstructionsChanged,
        ),
        (
            "reasoning",
            json!({"effort":"high"}),
            RequestDelta::OptionsChanged,
        ),
        (
            "new_future_option",
            json!(true),
            RequestDelta::OptionsChanged,
        ),
    ] {
        let mut changed = body.clone();
        changed[key] = value;
        assert_eq!(fingerprint(&changed)?.compare(&prior), expected);
    }
    let mut changed = body.clone();
    changed["input"][1]["content"] = json!("revised");
    assert_eq!(
        fingerprint(&changed)?.compare(&prior),
        RequestDelta::InputChanged { index: 1 }
    );
    changed["input"] = json!([{"role":"user","content":"first"}]);
    assert_eq!(
        fingerprint(&changed)?.compare(&prior),
        RequestDelta::InputShortened { index: 1 }
    );
    assert_eq!(
        prior.compare(&fingerprint(&changed)?),
        RequestDelta::AppendOnly
    );
    assert_eq!(prior.compare(&prior), RequestDelta::Identical);
    let other = RequestFingerprint::of(
        &body,
        &["other-route", "account-version"],
        body.to_string().len(),
    )
    .ok_or("fingerprint")?;
    assert_eq!(other.compare(&prior), RequestDelta::TargetChanged);
    Ok(())
}
#[test]
fn comparison_window_never_certifies_an_unseen_prefix() -> Result<(), String> {
    let mut body = request();
    body["input"] = Value::Array(
        (0..MAX_ITEMS + 2)
            .map(|index| json!({"content":index}))
            .collect(),
    );
    let prior = fingerprint(&body)?;
    let mut changed = body.clone();
    changed["input"][MAX_ITEMS + 1] = json!({"content":"unseen"});
    assert_eq!(
        fingerprint(&changed)?.compare(&prior),
        RequestDelta::ComparisonBound
    );
    let items = changed["input"].as_array_mut().ok_or("input")?;
    items.push(json!({"content":"appended"}));
    assert_eq!(
        fingerprint(&changed)?.compare(&prior),
        RequestDelta::ComparisonBound
    );
    assert_eq!(prior.compare(&prior), RequestDelta::Identical);
    body["input"]
        .as_array_mut()
        .ok_or("input")?
        .truncate(MAX_ITEMS);
    assert_eq!(
        prior.compare(&fingerprint(&body)?),
        RequestDelta::AppendOnly
    );
    assert_eq!(
        fingerprint(&body)?.compare(&prior),
        RequestDelta::InputShortened { index: MAX_ITEMS }
    );
    Ok(())
}
#[test]
fn body_bound_and_redacted_debug_do_not_change_a_request() -> Result<(), String> {
    let body = request();
    let before = body.clone();
    let fingerprint = fingerprint(&body)?;
    assert_eq!(body, before);
    assert_eq!(format!("{fingerprint:?}"), "RequestFingerprint { .. }");
    assert!(RequestFingerprint::of(&body, &["route"], MAX_BYTES + 1).is_none());
    assert!(RequestFingerprint::of(&json!({"input":false}), &["route"], 4).is_none());
    assert!(hash(&json!("x".repeat(MAX_BYTES))).is_none());
    assert_eq!(fingerprint.body_bytes, body.to_string().len());
    Ok(())
}
#[test]
fn anthropic_and_compatible_wire_shapes_participate() -> Result<(), String> {
    for body in [
        json!({"model":"m","messages":[{"role":"user","content":[{"text":"a","type":"text"}]}],"system":[{"text":"system"}]}),
        json!({"model":"m","messages":[{"role":"system","content":"system"},{"role":"user","content":"a"}]}),
    ] {
        let prior = fingerprint(&body)?;
        let mut changed = body.clone();
        changed["messages"]
            .as_array_mut()
            .ok_or("messages")?
            .push(json!({"role":"assistant","content":"answer"}));
        assert_eq!(
            fingerprint(&changed)?.compare(&prior),
            RequestDelta::AppendOnly
        );
        changed["system"] = json!([{"text":"new system"}]);
        assert_eq!(
            fingerprint(&changed)?.compare(&prior),
            RequestDelta::InstructionsChanged
        );
    }
    Ok(())
}

#[test]
fn absent_fields_are_distinct_from_explicit_null() -> Result<(), String> {
    let mut body = request();
    let object = body.as_object_mut().ok_or("object")?;
    object.remove("prompt_cache_key");
    object.remove("tools");
    object.remove("instructions");
    let prior = fingerprint(&body)?;
    assert_eq!(prior.tools_bytes, 0);
    assert_eq!(prior.instructions_bytes, 0);
    for (key, expected) in [
        ("prompt_cache_key", RequestDelta::CacheKeyChanged),
        ("tools", RequestDelta::ToolsChanged),
        ("instructions", RequestDelta::InstructionsChanged),
    ] {
        let mut changed = body.clone();
        changed[key] = Value::Null;
        assert_eq!(fingerprint(&changed)?.compare(&prior), expected);
    }
    Ok(())
}

#[test]
fn system_message_bytes_are_counted_without_misclassifying_a_tail_update() -> Result<(), String> {
    let mut body = request();
    body.as_object_mut().ok_or("object")?.remove("instructions");
    body["input"] =
        json!([{"role":"system","content":"pinned instructions"},{"role":"user","content":"task"}]);
    let previous = fingerprint(&body)?;
    assert!(previous.instructions_bytes > 0);
    body["input"]
        .as_array_mut()
        .ok_or("input")?
        .push(json!({"role":"system","content":"new instructions at the tail"}));
    let current = fingerprint(&body)?;
    assert!(current.instructions_bytes > previous.instructions_bytes);
    assert_eq!(current.compare(&previous), RequestDelta::AppendOnly);
    let durable = current.durable().ok_or("current fingerprint")?;
    let prior = previous.durable().ok_or("prior fingerprint")?;
    let comparison = durable.compare(&prior);
    assert!(comparison.append_only && comparison.changed_components.is_empty());
    body["input"][0]["content"] = json!("rewritten instructions");
    assert_eq!(
        fingerprint(&body)?.compare(&previous),
        RequestDelta::InputChanged { index: 0 }
    );
    Ok(())
}

#[test]
fn cache_headers_change_routing_even_when_the_body_is_identical() -> Result<(), String> {
    let body = request();
    let prior = fingerprint(&body)?
        .with_routing(Some("session-one"), None)
        .ok_or("routing")?;
    for (session, turn) in [
        (Some("session-two"), None),
        (Some("session-one"), Some("opaque-route")),
        (None, None),
    ] {
        let changed = fingerprint(&body)?
            .with_routing(session, turn)
            .ok_or("routing")?;
        assert_eq!(changed.compare(&prior), RequestDelta::RoutingChanged);
        assert!(!format!("{changed:?}").contains("opaque-route"));
    }
    let unchanged = fingerprint(&body)?
        .with_routing(Some("session-one"), None)
        .ok_or("routing")?;
    assert_eq!(unchanged.compare(&prior), RequestDelta::Identical);
    Ok(())
}

#[test]
fn durable_fingerprint_tracks_tools_system_instructions_and_exact_append_boundary(
) -> Result<(), String> {
    let body = json!({"model":"m","tools":[{"name":"shell"}],
        "messages":[{"role":"developer","content":"secret instruction"},{"role":"user","content":"private input"}]});
    let original = fingerprint(&body)?.durable().ok_or("durable fingerprint")?;
    let mut changed = body.clone();
    changed["tools"] = json!([{"name":"read"}]);
    let tools = fingerprint(&changed)?
        .durable()
        .ok_or("tools fingerprint")?;
    assert_ne!(tools.components().tools, original.components().tools);
    assert_eq!(
        tools.components().instructions,
        original.components().instructions
    );
    changed = body.clone();
    changed["messages"][0]["content"] = json!("different private instruction");
    let instructions = fingerprint(&changed)?
        .durable()
        .ok_or("instruction fingerprint")?;
    assert_eq!(
        instructions.components().instructions,
        original.components().instructions
    );
    assert_ne!(instructions.components().input, original.components().input);
    assert_eq!(instructions.stable_items(&original), 0);
    assert_eq!(
        instructions.compare(&original).changed_components,
        vec!["input"]
    );
    changed = body;
    changed["messages"]
        .as_array_mut()
        .ok_or("messages")?
        .push(json!({"role":"assistant","content":"answer"}));
    let append = fingerprint(&changed)?
        .durable()
        .ok_or("append fingerprint")?;
    assert_eq!(append.stable_items(&original), 2);
    assert_eq!(
        append.components().instructions,
        original.components().instructions
    );
    assert_eq!(format!("{append:?}"), "RequestCacheFingerprint { .. }");
    Ok(())
}

#[test]
fn appending_a_system_only_input_preserves_its_existing_prefix() -> Result<(), String> {
    let mut body = json!({"model":"m", "input":[{"role":"developer","content":"first"}]});
    let prior = fingerprint(&body)?;
    body["input"]
        .as_array_mut()
        .ok_or("input")?
        .push(json!({"role":"system","content":"second"}));
    let current = fingerprint(&body)?;
    assert_eq!(current.compare(&prior), RequestDelta::AppendOnly);
    let comparison = current
        .durable()
        .ok_or("current")?
        .compare(&prior.durable().ok_or("prior")?);
    assert!(comparison.append_only && comparison.changed_components.is_empty());
    Ok(())
}
