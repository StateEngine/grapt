use std::collections::BTreeMap;
use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};

use grapt_api::u3a::codec::{
    Base64Dto, BoundedMap, BoundedString, BoundedVec, FiniteOrSentinelF64, OpenObject, WireId,
};
use grapt_api::{
    decode_request, encode_page, DtoCodecError, U3aLimits, U3aLimitsError, U3A_PRODUCTION_LIMITS,
};
use grapt_schema::DecodeErrorKind;
use grapt_types::SessionId;
use serde::de::IgnoredAny;
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn request_bound_fires_before_target_deserialization() -> TestResult {
    let mut values = U3A_PRODUCTION_LIMITS;
    values.request_dto_bytes = 4;
    let limits = U3aLimits::try_new(values)?;
    let result = decode_request::<BoundedString<32>>(br#""1234""#, limits);
    match result {
        Err(DtoCodecError::Decode(error)) => {
            assert_eq!(error.kind(), &DecodeErrorKind::InputTooLarge { limit: 4 });
        }
        other => return Err(format!("expected pre-serde size refusal, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn strings_vectors_and_maps_enforce_before_excess_value_allocation() -> TestResult {
    let exact: BoundedString<4> = serde_json::from_str(r#""🦀""#)?;
    assert_eq!(exact.as_str(), "🦀");
    assert!(serde_json::from_str::<BoundedString<4>>(r#""🦀🦀""#).is_err());
    assert!(serde_json::from_str::<BoundedVec<BoundedString<4>, 2>>(r#"["a","b","c"]"#).is_err());

    static VALUES_DECODED: AtomicUsize = AtomicUsize::new(0);
    #[derive(Debug)]
    struct Counted;
    impl<'de> Deserialize<'de> for Counted {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            VALUES_DECODED.fetch_add(1, Ordering::SeqCst);
            IgnoredAny::deserialize(deserializer).map(|_| Self)
        }
    }

    VALUES_DECODED.store(0, Ordering::SeqCst);
    let duplicate =
        serde_json::from_str::<BoundedMap<BoundedString<4>, Counted, 2>>(r#"{"a":1,"a":2}"#);
    assert!(duplicate.is_err());
    assert_eq!(VALUES_DECODED.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn typed_id_and_base64_boundaries_reject_invalid_wire_shapes() -> TestResult {
    let session: WireId<SessionId> = serde_json::from_str(r#""ses_a""#)?;
    assert_eq!(serde_json::to_string(&session)?, r#""ses_a""#);
    assert!(serde_json::from_str::<WireId<SessionId>>(r#""not-a-session""#).is_err());

    for valid in [r#""""#, r#""TQ==""#, r#""TWE=""#, r#""TWFu""#] {
        let encoded: Base64Dto = serde_json::from_str(valid)?;
        assert_eq!(serde_json::to_string(&encoded)?, valid);
    }
    for invalid in [
        r#""TQ=""#,
        r#""TQ===""#,
        r#""TR==""#,
        r#""TWF=""#,
        r#""TW Fu""#,
    ] {
        assert!(serde_json::from_str::<Base64Dto>(invalid).is_err());
    }
    Ok(())
}

#[test]
fn finite_or_sentinel_uses_exact_type_and_value_dispatch() -> TestResult {
    let cases = [
        ("1.5", "1.5"),
        (r#""NaN""#, r#""NaN""#),
        (r#""Infinity""#, r#""Infinity""#),
        (r#""-Infinity""#, r#""-Infinity""#),
    ];
    for (wire, canonical) in cases {
        let value: FiniteOrSentinelF64 = serde_json::from_str(wire)?;
        assert_eq!(serde_json::to_string(&value)?, canonical);
    }
    for rejected in [r#""nan""#, r#""+Infinity""#, r#""1.5""#, "null"] {
        assert!(serde_json::from_str::<FiniteOrSentinelF64>(rejected).is_err());
    }
    Ok(())
}

#[test]
fn open_object_seed_preserves_json_without_untagged_buffering() -> TestResult {
    let value: OpenObject = serde_json::from_str(r#"{"a":[1,true,null,"x"],"b":{"c":2}}"#)?;
    let encoded = serde_json::to_string(&value)?;
    assert_eq!(encoded, r#"{"a":[1,true,null,"x"],"b":{"c":2}}"#);
    assert!(serde_json::from_str::<OpenObject>(r#"{"a":1,"a":2}"#).is_err());
    Ok(())
}

#[test]
fn open_object_depth_node_key_and_item_budgets_are_shared() {
    let too_deep = r#"{"a":{"a":{"a":{"a":{"a":{"a":{"a":{"a":0}}}}}}}}"#;
    assert!(serde_json::from_str::<OpenObject>(too_deep).is_err());

    let many_keys = (0..65)
        .map(|index| format!(r#""k{index}":null"#))
        .collect::<Vec<_>>()
        .join(",");
    assert!(serde_json::from_str::<OpenObject>(&format!("{{{many_keys}}}")).is_err());

    let many_items = std::iter::repeat_n("null", 257)
        .collect::<Vec<_>>()
        .join(",");
    assert!(serde_json::from_str::<OpenObject>(&format!(r#"{{"a":[{many_items}]}}"#)).is_err());
}

#[test]
fn open_object_scalar_and_encoded_byte_budgets_are_shared() {
    let scalar = "x".repeat(5_000);
    let aggregate = format!(r#"{{"a":"{scalar}","b":"{scalar}"}}"#);
    assert!(serde_json::from_str::<OpenObject>(&aggregate).is_err());

    let escaped = "\\u0000".repeat(3_000);
    let encoded = format!(r#"{{"a":"{escaped}"}}"#);
    assert!(serde_json::from_str::<OpenObject>(&encoded).is_err());
}

#[test]
fn page_encoding_refuses_before_full_materialization() -> TestResult {
    static ELEMENTS_SERIALIZED: AtomicUsize = AtomicUsize::new(0);
    struct CountedElement;
    impl Serialize for CountedElement {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            ELEMENTS_SERIALIZED.fetch_add(1, Ordering::SeqCst);
            serializer.serialize_str("1234")
        }
    }
    struct ManyElements;
    impl Serialize for ManyElements {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut sequence = serializer.serialize_seq(Some(1_000))?;
            for _ in 0..1_000 {
                sequence.serialize_element(&CountedElement)?;
            }
            sequence.end()
        }
    }

    let mut values = U3A_PRODUCTION_LIMITS;
    values.page_bytes = 5;
    let limits = U3aLimits::try_new(values)?;
    ELEMENTS_SERIALIZED.store(0, Ordering::SeqCst);
    assert!(matches!(
        encode_page(&ManyElements, limits),
        Err(DtoCodecError::PageBytes)
    ));
    assert_eq!(ELEMENTS_SERIALIZED.load(Ordering::SeqCst), 1);
    assert_eq!(encode_page(&"123", limits)?, br#""123""#);
    Ok(())
}

#[test]
fn limit_receipt_is_finite_explicit_and_has_no_default_path() -> TestResult {
    let production = U3aLimits::production()?;
    assert_eq!(production.receipt(), U3A_PRODUCTION_LIMITS);

    let mut zero = U3A_PRODUCTION_LIMITS;
    zero.nested_items = 0;
    assert_eq!(
        U3aLimits::try_new(zero),
        Err(U3aLimitsError::Zero {
            field: "nested_items"
        })
    );

    let mut host_escape = U3A_PRODUCTION_LIMITS;
    host_escape.request_dto_bytes += 1;
    assert_eq!(
        U3aLimits::try_new(host_escape),
        Err(U3aLimitsError::RequestExceedsHost)
    );
    Ok(())
}

#[test]
fn bounded_map_constructor_refuses_oversized_owned_values() {
    let values = BTreeMap::from([("a", 1), ("b", 2)]);
    assert!(BoundedMap::<_, _, 1>::try_new(values).is_err());
}
