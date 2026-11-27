//! U3a-A witness: the frozen IA0 closure is owned by compile-resolved Rust
//! DTO/codecs, with exact field presence and finite resource bounds.

use std::collections::{BTreeSet, HashSet};

use grapt_api::u3a::limits::{
    MAX_METADATA_KEY_BYTES, MAX_METADATA_NODES, MAX_METADATA_SCALAR_BYTES,
};
use grapt_api::u3a::{
    InterruptRequestDto, LocationInfoDto, LocationRefDto, NoContent, PromptRequestDto,
    SessionCreateRequestDto, SessionMessageInfoDto, ShellIdDto, WireId,
    OPTIONAL_NULLABLE_ENCODING_CARRIERS,
};
use grapt_api::{
    decode_request, encode_page, DtoCodecError, U3aLimits, FROZEN_OPENAPI_SHA256,
    FROZEN_SCHEMA_BYTES, FROZEN_SCHEMA_COUNT, NAMED_SCHEMA_CODECS, NAMED_SCHEMA_MANIFEST,
    OPTIONAL_NULLABLE_CARRIERS, U3A_PRODUCTION_LIMITS,
};
use grapt_schema::tri_state_ledger::U3A_OPTIONAL_NULLABLE_LEDGER;
use grapt_schema::DecodeErrorKind;
use serde_json::Value;

const OPENAPI: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/rust-reconstruction/compat/openapi/openapi.json"
));

const OPERATIONS: [(&str, &str); 14] = [
    ("/api/health", "get"),
    ("/api/project", "get"),
    ("/api/project/current", "get"),
    ("/api/provider", "get"),
    ("/api/model", "get"),
    ("/api/model/default", "get"),
    ("/api/agent", "get"),
    ("/api/session", "get"),
    ("/api/session/active", "get"),
    ("/api/session/{sessionID}", "get"),
    ("/api/session/{sessionID}/message", "get"),
    ("/api/session", "post"),
    ("/api/session/{sessionID}/prompt", "post"),
    ("/api/session/{sessionID}/interrupt", "post"),
];

fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error}"),
    }
}

fn required<'a>(value: Option<&'a Value>, context: &str) -> &'a Value {
    match value {
        Some(value) => value,
        None => panic!("missing frozen evidence: {context}"),
    }
}

fn document() -> Value {
    must(serde_json::from_str(OPENAPI), "parse frozen OpenAPI")
}

fn operation<'a>(document: &'a Value, path: &str, method: &str) -> &'a Value {
    let paths = required(document.get("paths"), "paths");
    let path_item = required(paths.get(path), path);
    required(path_item.get(method), method)
}

fn visit_refs(
    document: &Value,
    value: Option<&Value>,
    seen: &mut BTreeSet<String>,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        let prefix = "#/components/schemas/";
        let name = reference
            .strip_prefix(prefix)
            .ok_or_else(|| "reference outside components/schemas".to_owned())?;
        if seen.insert(name.to_owned()) {
            let schema = document
                .pointer(&format!("/components/schemas/{}", escape_pointer(name)))
                .ok_or_else(|| format!("missing referenced schema {name}"))?;
            visit_refs(document, Some(schema), seen)?;
        }
    }
    match value {
        Value::Array(items) => {
            for item in items {
                visit_refs(document, Some(item), seen)?;
            }
        }
        Value::Object(fields) => {
            for child in fields.values() {
                visit_refs(document, Some(child), seen)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn has_null(schema: &Value) -> bool {
    schema
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|branches| {
            branches
                .iter()
                .any(|branch| branch.get("type").and_then(Value::as_str) == Some("null"))
        })
}

fn collect_inline_optional_nullable(schema: &Value, path: &str, rows: &mut BTreeSet<String>) {
    if schema.get("$ref").is_some() {
        return;
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                if branch.get("type").and_then(Value::as_str) != Some("null") {
                    collect_inline_optional_nullable(branch, path, rows);
                }
            }
        }
    }
    if let Some(items) = schema.get("items") {
        collect_inline_optional_nullable(items, &format!("{path}[]"), rows);
    }
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let required_fields: HashSet<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    for (field, property) in properties {
        let field_path = format!("{path}.{field}");
        if !required_fields.contains(field.as_str()) && has_null(property) {
            assert!(
                rows.insert(field_path.clone()),
                "duplicate optional-nullable evidence row {field_path}"
            );
        }
        collect_inline_optional_nullable(property, &field_path, rows);
    }
}

fn frozen_closure(document: &Value) -> BTreeSet<String> {
    let mut closure = BTreeSet::new();
    for (path, method) in OPERATIONS {
        let selected = operation(document, path, method);
        must(
            visit_refs(document, selected.get("parameters"), &mut closure),
            "walk parameters",
        );
        must(
            visit_refs(document, selected.get("requestBody"), &mut closure),
            "walk request body",
        );
        must(
            visit_refs(document, selected.get("responses"), &mut closure),
            "walk responses",
        );
    }
    closure
}

fn frozen_presence_rows(document: &Value, closure: &BTreeSet<String>) -> BTreeSet<String> {
    let schemas = required(
        document.pointer("/components/schemas"),
        "components.schemas",
    );
    let mut rows = BTreeSet::new();
    for name in closure {
        let schema = required(schemas.get(name), name);
        collect_inline_optional_nullable(schema, &format!("schema:{name}"), &mut rows);
    }
    for (path, method) in OPERATIONS {
        let selected = operation(document, path, method);
        if let Some(parameters) = selected.get("parameters").and_then(Value::as_array) {
            for parameter in parameters {
                let location = required(parameter.get("in"), "parameter.in");
                let name = required(parameter.get("name"), "parameter.name");
                let schema = required(parameter.get("schema"), "parameter.schema");
                let location = match location.as_str() {
                    Some(value) => value,
                    None => panic!("parameter.in is not a string"),
                };
                let name = match name.as_str() {
                    Some(value) => value,
                    None => panic!("parameter.name is not a string"),
                };
                let base = format!(
                    "operation:{} {path}.{location}.{name}",
                    method.to_uppercase()
                );
                let required_parameter = parameter
                    .get("required")
                    .and_then(Value::as_bool)
                    .is_some_and(|value| value);
                if !required_parameter && has_null(schema) {
                    assert!(rows.insert(base.clone()), "duplicate presence row {base}");
                }
                collect_inline_optional_nullable(schema, &base, &mut rows);
            }
        }
        if let Some(body_schema) = selected.pointer("/requestBody/content/application~1json/schema")
        {
            let base = format!("operation:{} {path}.body", method.to_uppercase());
            collect_inline_optional_nullable(body_schema, &base, &mut rows);
        }
    }
    rows
}

fn production_limits() -> U3aLimits {
    must(U3aLimits::production(), "construct production U3a limits")
}

#[test]
fn frozen_selection_is_exactly_75_compile_resolved_rust_codecs() {
    assert_eq!(
        FROZEN_OPENAPI_SHA256, "0c3801f00ebc474222dd8bcf026aac5d34c82bf23ef24ac7111f5d776e758f4b",
        "frozen artifact digest pin drifted"
    );

    let document = document();
    let closure = frozen_closure(&document);
    assert_eq!(closure.len(), FROZEN_SCHEMA_COUNT);
    let schemas = required(
        document.pointer("/components/schemas"),
        "components.schemas",
    );
    let compact_bytes = closure.iter().fold(0usize, |total, name| {
        let bytes = must(
            serde_json::to_vec(required(schemas.get(name), name)),
            "compact selected schema",
        );
        match total.checked_add(bytes.len()) {
            Some(sum) => sum,
            None => panic!("selected schema byte census overflowed"),
        }
    });
    assert_eq!(compact_bytes, FROZEN_SCHEMA_BYTES);

    let manifest_names: BTreeSet<&str> = NAMED_SCHEMA_MANIFEST
        .iter()
        .map(|(name, _rust_type)| *name)
        .collect();
    assert_eq!(
        manifest_names.len(),
        FROZEN_SCHEMA_COUNT,
        "duplicate manifest row"
    );
    assert_eq!(
        closure.iter().map(String::as_str).collect::<BTreeSet<_>>(),
        manifest_names,
        "Rust-owned vocabulary differs from the exact frozen closure"
    );
    assert_eq!(NAMED_SCHEMA_CODECS.len(), FROZEN_SCHEMA_COUNT);
    for ((openapi_name, rust_type), codec) in NAMED_SCHEMA_MANIFEST.iter().zip(NAMED_SCHEMA_CODECS)
    {
        assert_eq!(
            codec(),
            *openapi_name,
            "typed codec mismatch for {rust_type}"
        );
        assert!(
            rust_type.ends_with("Dto"),
            "unversioned DTO carrier {rust_type}"
        );
    }
}

#[test]
fn each_of_48_optional_nullable_sites_has_its_own_sealed_ledger_row() {
    let document = document();
    let expected = frozen_presence_rows(&document, &frozen_closure(&document));
    let actual: BTreeSet<&str> = OPTIONAL_NULLABLE_CARRIERS
        .iter()
        .flat_map(|carrier| carrier().iter().copied())
        .filter(|row| !row.is_empty())
        .collect();
    let ledger: BTreeSet<&str> = U3A_OPTIONAL_NULLABLE_LEDGER.iter().copied().collect();
    assert_eq!(expected.len(), 48, "frozen presence census changed");
    assert_eq!(
        actual.len(),
        48,
        "duplicate or missing Rust evidence marker"
    );
    assert_eq!(
        actual, ledger,
        "typed carriers and the sealed ledger diverged"
    );
    assert_eq!(
        expected.iter().map(String::as_str).collect::<BTreeSet<_>>(),
        actual,
        "field-attributable tri-state ledger differs from frozen evidence"
    );

    let vocabulary = concat!(
        include_str!("../src/u3a/vocabulary/catalog.rs"),
        include_str!("../src/u3a/vocabulary/operations.rs"),
        include_str!("../src/u3a/vocabulary/session.rs"),
    );
    assert_eq!(
        vocabulary.matches("optional_nullable(").count(),
        48,
        "a typed field carrier escaped its evidence marker"
    );
    assert!(!vocabulary.contains("serde_json::Value"));
    assert!(!vocabulary.contains(": String"));
}

#[test]
fn all_48_optional_nullable_sites_encode_absent_null_and_value_independently() {
    let mut receipts = Vec::new();
    for carrier in OPTIONAL_NULLABLE_ENCODING_CARRIERS {
        receipts.extend(must(carrier(), "build tri-state encoding receipts"));
    }
    assert_eq!(receipts.len(), 48, "tri-state encoding census changed");
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.row)
            .collect::<BTreeSet<_>>(),
        U3A_OPTIONAL_NULLABLE_LEDGER.iter().copied().collect(),
        "tri-state encoding receipts diverged from the sealed ledger"
    );
    for receipt in receipts {
        let Some(absent) = receipt.absent.as_object() else {
            panic!("{} absent encoding was not an object", receipt.row);
        };
        assert!(
            absent.get(receipt.wire).is_none(),
            "{} absent state emitted {}",
            receipt.row,
            receipt.wire
        );
        assert_eq!(
            receipt.null.get(receipt.wire),
            Some(&Value::Null),
            "{} explicit null was not emitted at {}",
            receipt.row,
            receipt.wire
        );
        assert_eq!(
            receipt.value.get(receipt.wire),
            Some(&Value::Bool(true)),
            "{} populated value was not emitted at {}",
            receipt.row,
            receipt.wire
        );
    }
}

#[test]
fn request_presence_round_trips_absent_null_and_value_without_defaults() {
    let limits = production_limits();
    let absent: SessionCreateRequestDto = must(decode_request(b"{}", limits), "decode absence");
    assert!(absent.id().is_absent());
    assert!(absent.agent().is_absent());
    assert!(absent.model().is_absent());
    assert!(absent.location().is_absent());
    assert_eq!(must(encode_page(&absent, limits), "encode absence"), b"{}");

    let nulls: SessionCreateRequestDto = must(
        decode_request(
            br#"{"id":null,"agent":null,"model":null,"location":null}"#,
            limits,
        ),
        "decode explicit nulls",
    );
    assert!(nulls.id().is_null());
    assert!(nulls.agent().is_null());
    assert!(nulls.model().is_null());
    assert!(nulls.location().is_null());

    let values: SessionCreateRequestDto = must(
        decode_request(
            br#"{"id":"ses_a","agent":"agent-a","model":{"id":"model-a","providerID":"provider-a"},"location":{"directory":"work","workspaceID":"wrk_a"}}"#,
            limits,
        ),
        "decode present values",
    );
    assert!(values.id().as_value().is_some());
    assert!(values.agent().as_value().is_some());
    assert!(values.model().as_value().is_some());
    assert!(values.location().as_value().is_some());

    let unknown = decode_request::<SessionCreateRequestDto>(br#"{"surprise":1}"#, limits);
    assert!(unknown.is_err(), "closed request accepted an unknown field");
}

#[test]
fn required_prompt_text_and_typed_identities_refuse_invalid_wire_shapes() {
    let limits = production_limits();
    assert!(decode_request::<SessionCreateRequestDto>(br#"{"id":""}"#, limits).is_err());
    assert!(decode_request::<PromptRequestDto>(b"{}", limits).is_err());
    assert!(decode_request::<PromptRequestDto>(br#"{"text":null}"#, limits).is_err());
    assert!(decode_request::<PromptRequestDto>(br#"{"text":"x","extra":1}"#, limits).is_err());
    assert!(decode_request::<PromptRequestDto>(br#"{"text":"x","id":""}"#, limits).is_err());

    let prompt: PromptRequestDto = must(
        decode_request(
            br#"{"text":"x","id":null,"delivery":null,"resume":null}"#,
            limits,
        ),
        "decode prompt tri-state fields",
    );
    assert!(prompt.id().is_null());
    assert!(prompt.delivery().is_null());
    assert!(prompt.resume().is_null());

    let interrupt: InterruptRequestDto = must(decode_request(b"{}", limits), "decode interrupt");
    assert!(interrupt.execution_id().is_absent());
    let interrupt: InterruptRequestDto = must(
        decode_request(br#"{"executionID":null}"#, limits),
        "decode interrupt null",
    );
    assert!(interrupt.execution_id().is_null());
}

#[test]
fn all_five_frozen_identity_patterns_refuse_nonmatching_wire_values() {
    let limits = production_limits();
    let location = br#"{"directory":"work","workspaceID":"wrk_good","project":{"id":"project","directory":"work"}}"#;
    let _: LocationInfoDto = must(decode_request(location, limits), "decode valid workspace");
    let exact_workspace = br#"{"directory":"work","workspaceID":"wrk","project":{"id":"project","directory":"work"}}"#;
    let _: LocationInfoDto = must(
        decode_request(exact_workspace, limits),
        "decode exact ^wrk workspace",
    );
    for invalid in ["banana", "ses_wrong"] {
        let body = format!(
            "{{\"directory\":\"work\",\"workspaceID\":\"{invalid}\",\"project\":{{\"id\":\"project\",\"directory\":\"work\"}}}}"
        );
        assert!(
            decode_request::<LocationInfoDto>(body.as_bytes(), limits).is_err(),
            "Location.Info.workspaceID accepted non-^wrk value {invalid}"
        );
    }
    assert!(
        ShellIdDto::try_new("sh_").is_ok(),
        "Session.Message.Shell.shellID refused ^sh_ value"
    );
    assert!(
        ShellIdDto::try_new("shell_wrong").is_err(),
        "Session.Message.Shell.shellID accepted non-^sh_ value"
    );
    assert!(
        WireId::<grapt_types::SessionId>::parse(String::from(" ")).is_none(),
        "decoded single-space session identity was accepted"
    );
    for (value, accepted) in [
        (
            "evt_good",
            WireId::<grapt_types::EventId>::parse(String::from("evt_good")).is_some(),
        ),
        (
            "msg_good",
            WireId::<grapt_types::MessageId>::parse(String::from("msg_good")).is_some(),
        ),
        (
            "ses_good",
            WireId::<grapt_types::SessionId>::parse(String::from("ses_good")).is_some(),
        ),
    ] {
        assert!(accepted, "existing frozen identity pattern refused {value}");
    }
}

#[test]
fn location_ref_workspace_id_enforces_the_frozen_wrk_pattern() {
    let limits = production_limits();
    let exact: LocationRefDto = must(
        decode_request(br#"{"directory":"work","workspaceID":"wrk"}"#, limits),
        "decode exact Location.Ref ^wrk workspace",
    );
    assert!(
        exact.workspace_id().as_present().is_some(),
        "Location.Ref lost its valid workspaceID"
    );
    for invalid in ["banana", "ses_wrong"] {
        let body = format!("{{\"directory\":\"work\",\"workspaceID\":\"{invalid}\"}}");
        assert!(
            decode_request::<LocationRefDto>(body.as_bytes(), limits).is_err(),
            "Location.Ref.workspaceID accepted non-^wrk value {invalid}"
        );
    }
}

#[test]
fn request_and_collection_bounds_refuse_before_backend_work() {
    const REQUEST_OVERHEAD: usize = br#"{"text":""}"#.len();
    let at_bound = format!(
        "{{\"text\":\"{}\"}}",
        "x".repeat(U3A_PRODUCTION_LIMITS.request_dto_bytes - REQUEST_OVERHEAD)
    );
    assert_eq!(at_bound.len(), U3A_PRODUCTION_LIMITS.request_dto_bytes);
    let _: PromptRequestDto = must(
        decode_request(at_bound.as_bytes(), production_limits()),
        "decode exact request-byte boundary",
    );

    let over = vec![b' '; U3A_PRODUCTION_LIMITS.request_dto_bytes + 1];
    match decode_request::<PromptRequestDto>(&over, production_limits()) {
        Err(DtoCodecError::Decode(error)) => assert_eq!(
            error.kind(),
            &DecodeErrorKind::InputTooLarge {
                limit: U3A_PRODUCTION_LIMITS.request_dto_bytes,
            }
        ),
        other => panic!("oversized request failed at the wrong boundary: {other:?}"),
    }

    let files_64 = vec![serde_json::json!({"uri": "file:///x"}); 64];
    let accepted = must(
        serde_json::to_vec(&serde_json::json!({"text": "x", "files": files_64})),
        "encode 64 files",
    );
    let _: PromptRequestDto = must(
        decode_request(&accepted, production_limits()),
        "accept 64 prompt files",
    );
    let files_65 = vec![serde_json::json!({"uri": "file:///x"}); 65];
    let refused = must(
        serde_json::to_vec(&serde_json::json!({"text": "x", "files": files_65})),
        "encode 65 files",
    );
    assert!(
        decode_request::<PromptRequestDto>(&refused, production_limits()).is_err(),
        "65th prompt file escaped the finite collection bound"
    );
}

#[path = "u3a_typed_contract/open_objects.rs"]
mod open_objects;

#[test]
fn response_page_and_limit_configuration_are_bounded() {
    let mut values = U3A_PRODUCTION_LIMITS;
    values.page_bytes = 4;
    let tiny_page = must(U3aLimits::try_new(values), "construct tiny page limit");
    assert!(matches!(
        encode_page(&"abcd", tiny_page),
        Err(DtoCodecError::PageBytes)
    ));

    let mut zero = U3A_PRODUCTION_LIMITS;
    zero.nested_items = 0;
    assert!(U3aLimits::try_new(zero).is_err());

    let mut beyond_host = U3A_PRODUCTION_LIMITS;
    beyond_host.request_dto_bytes += 1;
    assert!(U3aLimits::try_new(beyond_host).is_err());
    assert_eq!(NoContent.encoded(), b"");
}

#[test]
fn message_info_is_a_closed_nine_arm_typed_union() {
    assert_eq!(SessionMessageInfoDto::VARIANT_COUNT, 9);
    let direct = serde_json::from_str::<SessionMessageInfoDto>(
        r#"{"type":"user","id":"msg_a","time":{"created":1},"text":"hello"}"#,
    );
    assert!(direct.is_ok(), "direct tag-first decode failed: {direct:?}");
    let user: SessionMessageInfoDto = must(
        decode_request(
            br#"{"type":"user","id":"msg_a","time":{"created":1},"text":"hello"}"#,
            production_limits(),
        ),
        "decode user message arm",
    );
    assert_eq!(user.variant_name(), "user");

    let assistant: SessionMessageInfoDto = must(
        decode_request(
            br#"{"type":"assistant","id":"msg_b","time":{"created":1},"agent":"agent-a","model":{"id":"model-a","providerID":"provider-a"},"content":[]}"#,
            production_limits(),
        ),
        "decode assistant message arm",
    );
    assert_eq!(assistant.variant_name(), "assistant");

    assert!(
        decode_request::<SessionMessageInfoDto>(
            br#"{"type":"future","id":"msg_c","time":{"created":1}}"#,
            production_limits(),
        )
        .is_err(),
        "unknown message union arm was accepted"
    );
    assert!(
        decode_request::<SessionMessageInfoDto>(
            br#"{"id":"msg_d","type":"user","time":{"created":1},"text":"late tag"}"#,
            production_limits(),
        )
        .is_err(),
        "message union accepted a non-leading discriminator"
    );
}

#[test]
fn compaction_ended_event_matches_the_frozen_event_schema() {
    use grapt_api::u3a::{
        AggregateIdDto, CompactionEndedData, CompactionReason, DogfoodEvent, DurableEvent,
        DurableEventRef, FiniteF64, Utf16Text,
    };
    let Some(session) = WireId::parse("ses_compaction_contract".to_owned()) else {
        panic!("session identity");
    };
    let Some(event_id) = WireId::parse("evt_compaction_contract".to_owned()) else {
        panic!("event identity");
    };
    let event = DogfoodEvent::CompactionEnded(DurableEvent::new(
        event_id,
        must(FiniteF64::try_new(1_789_000_000_000.0), "created"),
        DurableEventRef::new(
            must(
                AggregateIdDto::try_new("ses_compaction_contract"),
                "aggregate",
            ),
            7_u64.into(),
        ),
        CompactionEndedData::new(
            session,
            CompactionReason::Auto,
            "summary text".to_owned(),
            Utf16Text::empty(),
        ),
    ));
    let encoded = must(serde_json::to_value(&event), "encode compaction event");
    let document = document();
    let schema = required(
        document.pointer("/components/schemas/session.compaction.ended"),
        "frozen session.compaction.ended schema",
    );
    let keys = |value: &Value| -> BTreeSet<String> {
        value
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default()
    };
    let names = |value: Option<&Value>| -> BTreeSet<String> {
        value
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    for (encoded, schema, context) in [
        (&encoded, schema, "event"),
        (
            required(encoded.get("data"), "encoded data"),
            required(schema.pointer("/properties/data"), "frozen data schema"),
            "data",
        ),
    ] {
        let present = keys(encoded);
        let allowed = keys(required(schema.get("properties"), context));
        assert!(
            names(schema.get("required")).is_subset(&present),
            "the {context} omits a frozen required field: {present:?}"
        );
        assert!(
            present.is_subset(&allowed),
            "the {context} carries a field the frozen schema forbids: {present:?}"
        );
    }
    assert_eq!(encoded["type"], "session.compaction.ended");
    assert_eq!(encoded["durable"]["version"], 1);
    assert_eq!(encoded["data"]["reason"], "auto");
}

#[test]
fn compaction_ended_recent_value_keeps_the_frozen_schema() {
    use grapt_api::u3a::{
        AggregateIdDto, CompactionEndedData, CompactionReason, DogfoodEvent, DurableEvent,
        DurableEventRef, FiniteF64, Utf16Text,
    };
    let Some(session) = WireId::parse("ses_compaction_recent".to_owned()) else {
        panic!("session identity");
    };
    let Some(event_id) = WireId::parse("evt_compaction_recent".to_owned()) else {
        panic!("event identity");
    };
    let recent =
        "[User]: keep me\n\n[Assistant tool call]: read({\"path\":\"fixture.txt\"})".to_owned();
    let event = DogfoodEvent::CompactionEnded(DurableEvent::new(
        event_id,
        must(FiniteF64::try_new(1_789_000_000_000.0), "created"),
        DurableEventRef::new(
            must(
                AggregateIdDto::try_new("ses_compaction_recent"),
                "aggregate",
            ),
            7_u64.into(),
        ),
        CompactionEndedData::new(
            session,
            CompactionReason::Auto,
            "summary text".to_owned(),
            must(
                Utf16Text::try_new(recent.encode_utf16().collect()),
                "recent value",
            ),
        ),
    ));
    let encoded = must(serde_json::to_value(&event), "encode compaction event");
    let document = document();
    let schema = required(
        document.pointer("/components/schemas/session.compaction.ended"),
        "frozen session.compaction.ended schema",
    );
    let keys = |value: &Value| -> BTreeSet<String> {
        value
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default()
    };
    let names = |value: Option<&Value>| -> BTreeSet<String> {
        value
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    for (encoded, schema, context) in [
        (&encoded, schema, "event"),
        (
            required(encoded.get("data"), "encoded data"),
            required(schema.pointer("/properties/data"), "frozen data schema"),
            "data",
        ),
    ] {
        let present = keys(encoded);
        let allowed = keys(required(schema.get("properties"), context));
        assert!(
            names(schema.get("required")).is_subset(&present),
            "the {context} omits a frozen required field: {present:?}"
        );
        assert!(
            present.is_subset(&allowed),
            "the {context} carries a field the frozen schema forbids: {present:?}"
        );
    }
    assert_eq!(encoded["type"], "session.compaction.ended");
    assert_eq!(encoded["durable"]["version"], 1);
    assert_eq!(encoded["data"]["reason"], "auto");
    assert_eq!(
        encoded["data"]["recent"], recent,
        "the recent value left the frozen schema encoding"
    );
}

#[test]
fn compaction_ended_boundary_recent_escapes_lone_surrogates_verbatim() {
    use grapt_api::u3a::{
        AggregateIdDto, CompactionEndedData, CompactionReason, DogfoodEvent, DurableEvent,
        DurableEventRef, FiniteF64, Utf16Text,
    };
    let Some(session) = WireId::parse("ses_compaction_boundary".to_owned()) else {
        panic!("session identity");
    };
    let Some(event_id) = WireId::parse("evt_compaction_boundary".to_owned()) else {
        panic!("event identity");
    };
    let mut units: Vec<u16> = "[Tool result]: ".encode_utf16().collect();
    units.extend(std::iter::repeat_n(0x0061u16, 1_999));
    units.push(0xD83D);
    units.extend("\n[truncated]".encode_utf16());
    let recent = must(Utf16Text::try_new(units), "boundary recent");
    let event = DogfoodEvent::CompactionEnded(DurableEvent::new(
        event_id,
        must(FiniteF64::try_new(1_789_000_000_000.0), "created"),
        DurableEventRef::new(
            must(
                AggregateIdDto::try_new("ses_compaction_boundary"),
                "aggregate",
            ),
            7_u64.into(),
        ),
        CompactionEndedData::new(
            session,
            CompactionReason::Auto,
            "summary text".to_owned(),
            recent,
        ),
    ));
    let wire = must(serde_json::to_string(&event), "encode boundary event");
    let expected = format!(
        "\"recent\":\"[Tool result]: {}\\ud83d\\n[truncated]\"",
        "a".repeat(1_999)
    );
    assert!(
        wire.contains(&expected),
        "the boundary frame lost the exact unit escape: {wire}"
    );
    assert!(
        !wire.contains("$serde_json::private::RawValue"),
        "the boundary frame fell back to the private token object"
    );
}

#[test]
fn utf16_text_internal_unit_form_round_trips_the_units() {
    use grapt_api::u3a::Utf16Text;
    let mut units: Vec<u16> = "[User]: keep me".encode_utf16().collect();
    units.push(0xD83D);
    units.extend("\n[truncated]".encode_utf16());
    let recent = must(Utf16Text::try_new(units.clone()), "boundary recent");
    let b64 = recent.internal_b64();
    let value = serde_json::json!({ "secode.unit16": b64 });
    let decoded: Utf16Text = must(serde_json::from_value(value), "decode internal form");
    assert_eq!(
        decoded.units(),
        units.as_slice(),
        "the internal unit form changed the units"
    );
    assert_eq!(
        decoded.internal_b64(),
        b64,
        "the internal form is not canonical for the decoded units"
    );
}

#[test]
fn utf16_text_internal_unit_form_round_trips_one_mod_three_lengths() {
    use grapt_api::u3a::Utf16Text;
    // 2_027 units is 4_054 unit bytes: a one-byte final base64 quantum,
    // the padding case the encoder must get right.
    let recent = must(Utf16Text::try_new(vec![0x0061u16; 2_027]), "one mod three");
    let b64 = recent.internal_b64();
    let value = serde_json::json!({ "secode.unit16": b64 });
    let decoded: Utf16Text = must(serde_json::from_value(value), "decode internal form");
    assert_eq!(
        decoded.units(),
        recent.units(),
        "the one-mod-three internal unit form changed the units"
    );
}
#[test]
fn utf16_text_refuses_the_private_token_object_form() {
    use grapt_api::u3a::Utf16Text;
    let value = serde_json::json!({ "$serde_json::private::RawValue": "not raw" });
    let decoded = serde_json::from_value::<Utf16Text>(value);
    assert!(
        decoded.is_err(),
        "the private token object form deserialized as utf16 text"
    );
}

#[test]
fn utf16_text_enforces_the_dto_string_bound() {
    use grapt_api::u3a::Utf16Text;
    let at_bound = Utf16Text::try_new(vec![0x0061u16; 256 * 1024]);
    assert!(at_bound.is_ok(), "the exact dto string bound was refused");
    let over = Utf16Text::try_new(vec![0x0061u16; 256 * 1024 + 1]);
    assert_eq!(
        over,
        Err("utf16 text exceeds its dto string bound"),
        "the over-bound value was not refused with the named error"
    );
}

#[test]
fn compaction_page_recent_uses_the_frozen_string_wire_form_at_the_boundary() {
    use grapt_api::u3a::{SessionMessageCompactionCompletedDto, Utf16Text};
    let mut units: Vec<u16> = "[Tool result]: ".encode_utf16().collect();
    units.extend(std::iter::repeat_n(0x0061u16, 1_999));
    units.push(0xD83D);
    units.extend("\n[truncated]".encode_utf16());
    let b64 = must(Utf16Text::try_new(units), "boundary recent").internal_b64();
    // The internal unit object stays durable-only and decodes into the row.
    let message: SessionMessageCompactionCompletedDto = must(
        serde_json::from_value(serde_json::json!({
            "type": "compaction",
            "status": "completed",
            "id": "msg_page_boundary",
            "time": {"created": 1.0},
            "reason": "auto",
            "summary": "summary text",
            "recent": { "secode.unit16": b64 },
        })),
        "decode boundary page row",
    );
    // The re-serialization is the exact frozen string wire form at the cut.
    let wire = must(serde_json::to_string(&message), "encode boundary page row");
    let mut expected = String::from("\"recent\":\"[Tool result]: ");
    expected.extend(std::iter::repeat_n('a', 1_999));
    expected.push_str("\\ud83d\\n[truncated]\"");
    assert!(
        wire.contains(&expected),
        "the public page row left the frozen string wire form: {wire}"
    );
    // The exact frozen wire form carries the unpaired-surrogate escape the
    // oracle emits at the cut, so no strict JSON parser can re-parse it:
    // the string form is pinned by the fragment above, and the internal
    // object form is pinned absent here.
    assert!(
        !wire.contains("secode.unit16"),
        "the internal unit object leaked into the public page row: {wire}"
    );
}
