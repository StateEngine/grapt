//! Frozen missing/null/value matrix, canonical serialization matrix, bounded
//! and redacted failure paths, and the resurrection mutants each one kills.

use grapt_schema::{
    decode_json, deserialize_from_str, DecodeError, DecodeErrorKind, DecodeLimits, Nullable,
    Optional, Required,
};
use grapt_types::SessionId;
use serde::de::{Error as DeError, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

fn limits() -> DecodeLimits {
    DecodeLimits::new(4_096, 16).unwrap_or_else(|error| panic!("test limits: {error}"))
}

fn require_rejection<T: std::fmt::Debug>(
    result: Result<T, DecodeError>,
    witness: &str,
) -> DecodeError {
    match result {
        Err(error) => error,
        Ok(value) => panic!("{witness}; decoded {value:?}"),
    }
}

fn require_decode<T>(result: Result<T, DecodeError>, witness: &str) -> T {
    result.unwrap_or_else(|error| panic!("{witness}: {error}"))
}

fn require_serialize_rejection(
    result: Result<String, serde_json::Error>,
    witness: &str,
) -> serde_json::Error {
    match result {
        Err(error) => error,
        Ok(encoded) => panic!("{witness}; encoded {encoded}"),
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct CategoryDocument {
    required: Required<String>,
    #[serde(skip_serializing_if = "Optional::is_absent")]
    optional: Optional<String>,
    nullable: Nullable<String>,
}

const CATEGORY_FIELDS: &[&str] = &["required", "optional", "nullable"];

/// Observes which keys the wire map actually carried.
///
/// The ratified codec section forbids `serde(default)`, and no category
/// implements [`Default`], so absence is observed here rather than repaired.
impl<'de> Deserialize<'de> for CategoryDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(CategoryVisitor)
    }
}

struct CategoryVisitor;

impl<'de> Visitor<'de> for CategoryVisitor {
    type Value = CategoryDocument;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a category document object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut required = None;
        let mut optional = None;
        let mut nullable = None;
        while let Some(key) = map.next_key::<String>()? {
            let duplicated = match key.as_str() {
                "required" => required.replace(map.next_value()?).is_some(),
                "optional" => optional
                    .replace(map.next_value::<Optional<String>>()?)
                    .is_some(),
                "nullable" => nullable.replace(map.next_value()?).is_some(),
                unknown => return Err(A::Error::unknown_field(unknown, CATEGORY_FIELDS)),
            };
            if duplicated {
                return Err(A::Error::custom(format!("duplicate field `{key}`")));
            }
        }
        Ok(CategoryDocument {
            required: required.ok_or_else(|| A::Error::missing_field("required"))?,
            nullable: nullable.ok_or_else(|| A::Error::missing_field("nullable"))?,
            optional: optional.unwrap_or_else(Optional::absent),
        })
    }
}

fn document(fragment: &str) -> Vec<u8> {
    format!(r#"{{"required":"r","nullable":"n"{fragment}}}"#).into_bytes()
}

#[test]
fn required_rejects_both_missing_and_explicit_null() {
    let valid = require_decode(
        decode_json::<CategoryDocument>(&document(""), limits()),
        "a present required value decodes",
    );
    assert_eq!(valid.required.get(), "r");

    let missing = require_rejection(
        decode_json::<CategoryDocument>(br#"{"nullable":"n"}"#, limits()),
        "a missing required field must reject",
    );
    assert_eq!(missing.kind(), &DecodeErrorKind::Data);
    assert_eq!(missing.path(), "$.required");

    let null = require_rejection(
        decode_json::<CategoryDocument>(br#"{"required":null,"nullable":"n"}"#, limits()),
        "an explicit null required field must reject",
    );
    assert_eq!(null.kind(), &DecodeErrorKind::Data);
    // The key came from the document, so the default policy redacts it. The
    // MISSING case above keeps its name because an absent field cannot have
    // been supplied by the input.
    assert_eq!(null.path(), "$.<redacted>");
}

#[test]
fn optional_maps_only_missing_to_absent_and_rejects_explicit_null() {
    let absent = require_decode(
        decode_json::<CategoryDocument>(&document(""), limits()),
        "an omitted optional field decodes",
    );
    assert!(absent.optional.is_absent());

    let present = require_decode(
        decode_json::<CategoryDocument>(&document(r#","optional":"o""#), limits()),
        "a present optional field decodes",
    );
    assert_eq!(present.optional.as_present().map(String::as_str), Some("o"));

    let null = require_rejection(
        decode_json::<CategoryDocument>(&document(r#","optional":null"#), limits()),
        "an explicit null optional field must reject",
    );
    assert_eq!(null.kind(), &DecodeErrorKind::Data);
    assert_eq!(null.path(), "$.<redacted>");
}

#[test]
fn nullable_requires_presence_and_maps_null_to_null() {
    let value = require_decode(
        decode_json::<CategoryDocument>(&document(""), limits()),
        "a present nullable field decodes",
    );
    assert_eq!(value.nullable.as_value().map(String::as_str), Some("n"));

    let null = require_decode(
        decode_json::<CategoryDocument>(br#"{"required":"r","nullable":null}"#, limits()),
        "an explicit null nullable field decodes",
    );
    assert!(null.nullable.is_null());

    let missing = require_rejection(
        decode_json::<CategoryDocument>(br#"{"required":"r"}"#, limits()),
        "a missing nullable field must reject",
    );
    assert_eq!(missing.kind(), &DecodeErrorKind::Data);
    assert_eq!(missing.path(), "$.nullable");
}

#[test]
fn canonical_serialization_omits_only_absent_and_emits_null_only_for_nullable_categories() {
    const ABSENT: &[u8] = br#"{"required":"r","nullable":"n"}"#;
    let decoded = require_decode(
        decode_json::<CategoryDocument>(ABSENT, limits()),
        "absent-state document decodes",
    );
    let encoded = serde_json::to_vec(&decoded)
        .unwrap_or_else(|error| panic!("absent-state document encodes: {error}"));
    assert_eq!(encoded, ABSENT, "absent categories are omitted, never null");

    const NULLS: &[u8] = br#"{"required":"r","nullable":null}"#;
    let decoded = require_decode(
        decode_json::<CategoryDocument>(NULLS, limits()),
        "null-state document decodes",
    );
    let encoded = serde_json::to_vec(&decoded)
        .unwrap_or_else(|error| panic!("null-state document encodes: {error}"));
    assert_eq!(encoded, NULLS, "nullable categories emit their null");

    const VALUES: &[u8] = br#"{"required":"r","optional":"o","nullable":"n"}"#;
    let decoded = require_decode(
        decode_json::<CategoryDocument>(VALUES, limits()),
        "value-state document decodes",
    );
    let encoded = serde_json::to_vec(&decoded)
        .unwrap_or_else(|error| panic!("value-state document encodes: {error}"));
    assert_eq!(encoded, VALUES);
}

#[derive(Debug, Serialize)]
struct UnskippedOptional {
    optional: Optional<String>,
}

#[test]
fn serializing_an_absent_field_fails_loudly_instead_of_emitting_null() {
    // Mutation: dropping `skip_serializing_if` from an absent-capable field
    // would otherwise turn omission into an indistinguishable explicit null.
    let error = require_serialize_rejection(
        serde_json::to_string(&UnskippedOptional {
            optional: Optional::absent(),
        }),
        "an unskipped absent Optional must not serialize",
    );
    assert!(error.to_string().contains("must be omitted"));
}

#[test]
fn explicit_null_resurrection_mutant_stays_dead_for_both_null_rejecting_categories() {
    // Mutation: replacing `Required<T>`/`Optional<T>` with `Option<T>` plus
    // `serde(default)` makes each of these decode successfully.
    let required = require_rejection(
        decode_json::<CategoryDocument>(br#"{"required":null,"nullable":"n"}"#, limits()),
        "required null resurrection mutant must remain dead",
    );
    assert_eq!(required.path(), "$.<redacted>");
    let optional = require_rejection(
        decode_json::<CategoryDocument>(&document(r#","optional":null"#), limits()),
        "optional null resurrection mutant must remain dead",
    );
    assert_eq!(optional.path(), "$.<redacted>");
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UndefaultedOptionalDocument {
    optional: Optional<String>,
}

#[test]
fn a_derived_container_cannot_represent_absence_at_all() {
    // The ratified codec section forbids `serde(default)`, and no category
    // implements `Default`, so a derived container has no way to spell
    // omission. It must reject rather than guess, which is what forces a
    // container with an absent-capable field to observe presence explicitly.
    let error = require_rejection(
        decode_json::<UndefaultedOptionalDocument>(br#"{}"#, limits()),
        "an omitted field without serde(default) must reject",
    );
    assert_eq!(error.kind(), &DecodeErrorKind::Data);
    assert_eq!(error.path(), "$.optional");

    let present = require_decode(
        decode_json::<UndefaultedOptionalDocument>(br#"{"optional":"o"}"#, limits()),
        "a present value still decodes",
    );
    assert_eq!(present.optional.as_present().map(String::as_str), Some("o"));
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityDocument {
    #[serde(deserialize_with = "deserialize_from_str")]
    session_id: SessionId,
}

#[test]
fn invalid_opaque_newtype_uses_the_reusable_strict_boundary() {
    let valid = require_decode(
        decode_json::<IdentityDocument>(br#"{"session_id":"ses_valid"}"#, limits()),
        "valid identity should decode",
    );
    assert_eq!(valid.session_id.as_str(), "ses_valid");

    let error = require_rejection(
        decode_json::<IdentityDocument>(br#"{"session_id":"msg_wrong"}"#, limits()),
        "wrong identity kind must reject",
    );
    assert_eq!(error.kind(), &DecodeErrorKind::Data);
    assert_eq!(error.path(), "$.<redacted>");
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Lifecycle {
    Pending,
    Settled,
}

#[derive(Debug, Deserialize)]
struct LifecycleDocument {
    lifecycle: Lifecycle,
}

#[test]
fn unknown_lifecycle_enum_is_rejected_without_echoing_the_value() {
    let valid = require_decode(
        decode_json::<LifecycleDocument>(br#"{"lifecycle":"pending"}"#, limits()),
        "known lifecycle should decode",
    );
    assert!(matches!(valid.lifecycle, Lifecycle::Pending));
    let _ = Lifecycle::Settled;

    const SENTINEL: &str = "SECRET_UNKNOWN_LIFECYCLE";
    let input = format!(r#"{{"lifecycle":"{SENTINEL}"}}"#);
    let error = require_rejection(
        decode_json::<LifecycleDocument>(input.as_bytes(), limits()),
        "unknown lifecycle must reject",
    );
    assert_eq!(error.kind(), &DecodeErrorKind::Data);
    assert_eq!(error.path(), "$.<redacted>");
    assert!(!format!("{error}").contains(SENTINEL));
    assert!(!format!("{error:?}").contains(SENTINEL));
}

#[derive(Debug, Deserialize)]
struct CollectionDocument {
    outer: CollectionOuter,
}

#[derive(Debug, Deserialize)]
struct CollectionOuter {
    items: Vec<Nested>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Nested {
    enabled: bool,
}

#[test]
fn malformed_nested_array_object_reports_the_exact_path() {
    let valid = require_decode(
        decode_json::<CollectionDocument>(br#"{"outer":{"items":[{"enabled":true}]}}"#, limits()),
        "nested collection should decode",
    );
    assert!(valid.outer.items[0].enabled);
    let error = require_rejection(
        decode_json::<CollectionDocument>(
            br#"{"outer":{"items":[{"enabled":true},{"enabled":"yes"}]}}"#,
            limits(),
        ),
        "wrong nested type must reject",
    );
    assert_eq!(error.kind(), &DecodeErrorKind::Data);
    // Depth, ordering and the sequence INDEX survive: they come from the
    // target's own shape, not from a key the document chose.
    assert_eq!(error.path(), "$.<redacted>.<redacted>[1].<redacted>");
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct FrozenRepresentative {
    enabled: bool,
    count: u32,
    labels: Vec<String>,
    nested: FrozenNested,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct FrozenNested {
    mode: String,
}

#[test]
fn frozen_representative_json_round_trips_without_coercion() {
    const FROZEN: &[u8] =
        br#"{"enabled":true,"count":2,"labels":["alpha","beta"],"nested":{"mode":"strict"}}"#;
    let decoded = require_decode(
        decode_json::<FrozenRepresentative>(FROZEN, limits()),
        "frozen example should decode",
    );
    let encoded = serde_json::to_vec(&decoded)
        .unwrap_or_else(|error| panic!("frozen example should encode: {error}"));
    assert_eq!(encoded, FROZEN);

    let error = require_rejection(
        decode_json::<FrozenRepresentative>(
            br#"{"enabled":"true","count":2,"labels":[],"nested":{"mode":"strict"}}"#,
            limits(),
        ),
        "string boolean must not coerce",
    );
    assert_eq!(error.path(), "$.<redacted>");
}

#[test]
fn secret_value_is_absent_from_display_debug_and_error_state() {
    const SENTINEL: &str = "SECRET_SENTINEL_DO_NOT_ECHO";
    let input = format!(r#"{{"outer":{{"items":[{{"enabled":"{SENTINEL}"}}]}}}}"#);
    let error = require_rejection(
        decode_json::<CollectionDocument>(input.as_bytes(), limits()),
        "secret string is not a boolean",
    );
    assert!(!error.path().contains(SENTINEL));
    assert!(!format!("{error}").contains(SENTINEL));
    assert!(!format!("{error:?}").contains(SENTINEL));
    assert!(std::error::Error::source(&error).is_none());
}

#[derive(Debug, Deserialize)]
struct DynamicPathDocument {
    values: BTreeMap<String, bool>,
}

/// S1 — the DEFAULT boundary does not echo a document-derived key at all.
///
/// # This assertion is the inversion of a previously green one
///
/// It used to read `assert!(error.path().starts_with("$.values.kkk"))`. That
/// POSITIVELY PINNED the leak: the suite required the first 64 characters of a
/// map key — content the document chose — to appear in every public rendering,
/// and a change that stopped echoing it failed the suite. The bound was real and
/// the key was truncated, but a truncated secret is a narrowed secret, and 64
/// characters is the whole of most of them.
///
/// The old test's other half is kept, because it was witnessing a different and
/// still-live law: `path()` is an accessor no rendering bound protects, so it
/// must be bounded at CONSTRUCTION rather than at `Display`.
#[test]
fn the_default_boundary_never_echoes_a_document_derived_key() {
    let valid = require_decode(
        decode_json::<DynamicPathDocument>(br#"{"values":{"short":true}}"#, limits()),
        "valid dynamic path should decode",
    );
    assert_eq!(valid.values.get("short"), Some(&true));

    let key = "k".repeat(512);
    let input = format!(r#"{{"values":{{"{key}":"not-a-boolean"}}}}"#);
    let roomy_limits =
        DecodeLimits::new(2_048, 8).unwrap_or_else(|error| panic!("test render limits: {error}"));
    let error = require_rejection(
        decode_json::<DynamicPathDocument>(input.as_bytes(), roomy_limits),
        "dynamic value type must reject",
    );

    // Not merely "the WHOLE key is absent" — a 64-character prefix would satisfy
    // that and is what the old assertion required. The exact rendered path is
    // pinned, so any echo at all fails.
    assert_eq!(error.path(), "$.<redacted>.<redacted>");
    assert!(
        !error.path().contains("kkk"),
        "no prefix of the key may survive either"
    );
    // Mutation: returning the raw joined serde path from `path()` republishes
    // the whole key through an accessor that no rendering bound protects.
    assert!(!error.path().contains(&key));
    assert!(error.path().chars().count() < 128);
    assert!(format!("{error}").chars().count() < 200);
    assert!(format!("{error:?}").chars().count() < 260);
}

/// S1 — an ENUM VARIANT the document supplied is redacted on the same channel.
///
/// Separate from the map-key witness above because they are separate arms of
/// `render_path`. Deleting only the `Segment::Enum` arm's redaction leaves the
/// map-key test green, so one test could not have attributed both.
#[test]
fn the_default_boundary_never_echoes_a_document_derived_enum_variant() {
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Tagged {
        Known { inner: bool },
    }

    #[derive(Debug, Deserialize)]
    struct TaggedDocument {
        tagged: Tagged,
    }

    let valid = require_decode(
        decode_json::<TaggedDocument>(br#"{"tagged":{"known":{"inner":true}}}"#, limits()),
        "a known variant decodes",
    );
    assert!(matches!(valid.tagged, Tagged::Known { inner: true }));

    let error = require_rejection(
        decode_json::<TaggedDocument>(br#"{"tagged":{"known":{"inner":"yes"}}}"#, limits()),
        "a wrongly typed variant field must reject",
    );
    assert_eq!(error.kind(), &DecodeErrorKind::Data);
    assert!(
        !error.path().contains("known"),
        "the variant the document selected must not be echoed: {}",
        error.path()
    );
    assert!(!error.path().contains("tagged"));
    assert!(!error.path().contains("inner"));
}

/// S1 — the opt-in, which is the ONLY way a named segment is revealed.
///
/// Its previous spelling, `with_path_segment_chars(0)`, was an opt-OUT of a
/// revealing default. A boundary that had never considered its key space leaked
/// by omission; now it redacts by omission and must say so to reveal.
#[test]
fn revealing_a_named_segment_requires_an_explicit_opt_in() {
    const SENTINEL: &str = "tenant-secret-key";
    let input = format!(r#"{{"values":{{"{SENTINEL}":"not-a-boolean"}}}}"#);

    let default = DecodeLimits::new(2_048, 8)
        .unwrap_or_else(|error| panic!("test redaction limits: {error}"));
    let error = require_rejection(
        decode_json::<DynamicPathDocument>(input.as_bytes(), default),
        "dynamic value type must reject",
    );
    assert_eq!(error.path(), "$.<redacted>.<redacted>");
    assert!(!format!("{error}").contains(SENTINEL));
    assert!(!format!("{error:?}").contains(SENTINEL));

    // The same input under an explicit opt-in DOES echo it. Without this half the
    // test above would also pass against a decoder that had lost the ability to
    // render a path at all, and the opt-in would be dead code.
    let opted_in = DecodeLimits::new(2_048, 8)
        .unwrap_or_else(|error| panic!("test reveal limits: {error}"))
        .revealing_named_path_segments(64);
    let error = require_rejection(
        decode_json::<DynamicPathDocument>(input.as_bytes(), opted_in),
        "dynamic value type must reject",
    );
    assert_eq!(error.path(), format!("$.values.{SENTINEL}"));
}

/// S1 — a zero budget is redaction, not a degenerate reveal.
///
/// Its own test rather than a third clause of the one above, so a mutation that
/// breaks the zero case and a mutation that breaks the default are separately
/// attributable. Sharing a test would have given both the same witness and left
/// the harness unable to say which law had failed.
#[test]
fn a_zero_reveal_budget_resolves_to_redaction() {
    const SENTINEL: &str = "tenant-secret-key";
    let input = format!(r#"{{"values":{{"{SENTINEL}":"not-a-boolean"}}}}"#);
    let zero = DecodeLimits::new(2_048, 8)
        .unwrap_or_else(|error| panic!("test zero limits: {error}"))
        .revealing_named_path_segments(0);
    let error = require_rejection(
        decode_json::<DynamicPathDocument>(input.as_bytes(), zero),
        "dynamic value type must reject",
    );
    assert_eq!(error.path(), "$.<redacted>.<redacted>");
    assert!(!format!("{error:?}").contains(SENTINEL));
}

/// S1 — an OPTED-IN segment is still truncated to the budget it advertises.
///
/// The per-segment bound survived the redaction default; it just moved behind
/// the opt-in. Without this witness the bound is unreachable from any test —
/// the default never renders a name, and the previous witness for it asserted a
/// 64-character echo that no longer happens.
#[test]
fn an_opted_in_segment_is_truncated_to_its_advertised_budget() {
    let key = "k".repeat(100);
    let input = format!(r#"{{"values":{{"{key}":"not-a-boolean"}}}}"#);
    let opted_in = DecodeLimits::new(2_048, 8)
        .unwrap_or_else(|error| panic!("test reveal limits: {error}"))
        .revealing_named_path_segments(20);
    let error = require_rejection(
        decode_json::<DynamicPathDocument>(input.as_bytes(), opted_in),
        "dynamic value type must reject",
    );
    // Exactly 20 characters INCLUDING the marker, so the budget the caller
    // advertised is the budget rendered.
    let expected = format!("$.values.{}...", "k".repeat(17));
    assert_eq!(error.path(), expected);
    assert!(!error.path().contains(&key));
}

/// S2 — serde's line and column are absent from every public surface.
///
/// The column is an offset into the rejected document, so it reports the exact
/// encoded length of everything before the failure on that line. Two inputs that
/// differ only in the length of a secret produce different columns, which is a
/// length oracle emitted on every failure.
#[test]
fn no_public_surface_reports_a_position_in_the_rejected_input() {
    // Two documents identical except for the length of the value preceding the
    // failure. Under the old error they produced columns 30 and 46.
    let short = br#"{"outer":{"items":[{"enabled":true},{"enabled":"a"}]}}"#.to_vec();
    let long = br#"{"outer":{"items":[{"enabled":true},{"enabled":"aaaaaaaaaaaaaaaa"}]}}"#.to_vec();

    let short = require_rejection(
        decode_json::<CollectionDocument>(&short, limits()),
        "short document must reject",
    );
    let long = require_rejection(
        decode_json::<CollectionDocument>(&long, limits()),
        "long document must reject",
    );

    // Identical renderings for inputs of different length: nothing observable
    // varies with how much content preceded the failure.
    assert_eq!(format!("{short}"), format!("{long}"));
    assert_eq!(format!("{short:?}"), format!("{long:?}"));
    assert_eq!(short, long);

    // …and no positional vocabulary survives in the rendering at all.
    for rendered in [format!("{short}"), format!("{short:?}")] {
        assert!(!rendered.contains("line"), "{rendered}");
        assert!(!rendered.contains("column"), "{rendered}");
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DerivedNullableDocument {
    nullable: Nullable<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DerivedRequiredDocument {
    required: Required<String>,
}

#[test]
fn a_derived_container_reports_a_missing_required_field_at_its_own_path() {
    // The two-stage decode asks for a newtype struct first, so an omission
    // fails with serde's own missing-field error and its exact path. A
    // category that asked for an option directly would instead surface its own
    // null-rejection at the containing object, naming no field.
    let missing = require_rejection(
        decode_json::<DerivedRequiredDocument>(br#"{}"#, limits()),
        "a missing required field must reject in a derived container",
    );
    assert_eq!(missing.kind(), &DecodeErrorKind::Data);
    assert_eq!(missing.path(), "$.required");

    let null = require_rejection(
        decode_json::<DerivedRequiredDocument>(br#"{"required":null}"#, limits()),
        "an explicit null required field must reject",
    );
    assert_eq!(null.path(), "$.<redacted>");

    let present = require_decode(
        decode_json::<DerivedRequiredDocument>(br#"{"required":"r"}"#, limits()),
        "a present required value still decodes",
    );
    assert_eq!(present.required.get(), "r");
}

#[test]
fn a_derived_container_still_separates_a_missing_nullable_from_an_explicit_null() {
    // A container that observes presence itself never reaches serde's
    // missing-field path, so this derived container is what keeps the two-stage
    // decode honest. serde reports an absent field through a deserializer that
    // answers only `deserialize_option` with `visit_none` — the very same call
    // an explicit `null` makes. A category that asked for an option directly
    // would decode this omission as `Null`.
    let missing = require_rejection(
        decode_json::<DerivedNullableDocument>(br#"{}"#, limits()),
        "a missing nullable field must reject in a derived container",
    );
    assert_eq!(missing.kind(), &DecodeErrorKind::Data);
    assert_eq!(missing.path(), "$.nullable");

    let null = require_decode(
        decode_json::<DerivedNullableDocument>(br#"{"nullable":null}"#, limits()),
        "an explicit null still decodes",
    );
    assert!(null.nullable.is_null());
}

#[test]
fn a_repeated_wire_key_is_refused_rather_than_silently_last_wins() {
    // Observing presence by walking the map means a key seen twice is a real
    // ambiguity about what the sender meant, not a last-write-wins default.
    let error = require_rejection(
        decode_json::<CategoryDocument>(
            br#"{"required":"first","required":"second","nullable":"n"}"#,
            limits(),
        ),
        "a repeated key must reject",
    );
    assert_eq!(error.kind(), &DecodeErrorKind::Data);
}

#[test]
fn a_rendered_path_stays_within_its_advertised_total_bound() {
    // Mutation: taking the whole 256-character budget and then appending the
    // truncation marker renders 259 characters while advertising 256.
    let key = "k".repeat(500);
    let input = format!(r#"{{"values":{{"{key}":"not-a-boolean"}}}}"#);
    let roomy = DecodeLimits::new(8_192, 8)
        .unwrap_or_else(|error| panic!("test render limits: {error}"))
        .revealing_named_path_segments(400);
    let error = require_rejection(
        decode_json::<DynamicPathDocument>(input.as_bytes(), roomy),
        "dynamic value type must reject",
    );
    let rendered = error.path().chars().count();
    assert!(
        rendered <= 256,
        "path rendered {rendered} characters, advertised bound is 256"
    );
    assert!(error.path().ends_with("..."), "an overrun path is marked");
}

static DESERIALIZE_CALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct CountedValue;

impl<'de> Deserialize<'de> for CountedValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        DESERIALIZE_CALLS.fetch_add(1, Ordering::SeqCst);
        serde_json::Value::deserialize(deserializer).map(|_| Self)
    }
}

#[test]
fn input_size_and_depth_are_rejected_by_preflight_bounds() {
    DESERIALIZE_CALLS.store(0, Ordering::SeqCst);
    let size_limits =
        DecodeLimits::new(8, 16).unwrap_or_else(|error| panic!("test size limits: {error}"));
    let error = require_rejection(
        decode_json::<CountedValue>(br#"{"long":true}"#, size_limits),
        "oversized input must reject before serde",
    );
    // S3, and the second inversion of a previously green assertion. This read
    // `InputTooLarge { limit: 8, actual: 13 }` and thereby REQUIRED the decoder
    // to report the exact byte length of the rejected input. The configured
    // limit is the caller's own number and is safe; the observed length is a
    // measurement of the input, and a caller who can vary a wrapper around one
    // secret narrows its length by reading it back.
    assert_eq!(error.kind(), &DecodeErrorKind::InputTooLarge { limit: 8 });
    assert_eq!(DESERIALIZE_CALLS.load(Ordering::SeqCst), 0);

    let depth_limits =
        DecodeLimits::new(128, 2).unwrap_or_else(|error| panic!("test depth limits: {error}"));
    let error = require_rejection(
        decode_json::<CountedValue>(br#"{"a":[{"b":true}]}"#, depth_limits),
        "overly nested input must reject before serde",
    );
    assert_eq!(error.kind(), &DecodeErrorKind::NestingTooDeep { limit: 2 });
    assert_eq!(DESERIALIZE_CALLS.load(Ordering::SeqCst), 0);
}

/// S3 — two oversized inputs of DIFFERENT length are indistinguishable.
///
/// The assertion above pins the variant's shape, which a mutation restoring
/// `actual` would fail to compile rather than fail to pass — a compile error is
/// not a witness. This one fails as a TEST: restore the field and the two
/// renderings diverge with the input lengths.
#[test]
fn an_oversized_input_does_not_report_its_own_length() {
    let size_limits =
        DecodeLimits::new(8, 16).unwrap_or_else(|error| panic!("test size limits: {error}"));
    let short = require_rejection(
        decode_json::<CountedValue>(br#"{"a":true}"#, size_limits),
        "oversized input must reject",
    );
    let long = require_rejection(
        decode_json::<CountedValue>(br#"{"aaaaaaaaaaaaaaaaaaaaaaaaaaaa":true}"#, size_limits),
        "oversized input must reject",
    );
    assert_eq!(
        short, long,
        "the length of the rejected input must not vary"
    );
    assert_eq!(format!("{short:?}"), format!("{long:?}"));
    assert!(!format!("{short:?}").contains("10"));
    assert!(!format!("{long:?}").contains("37"));
    // The CONFIGURED limit is retained: it is the caller's own number and is
    // what makes the failure actionable, so this is not a blanket blanking.
    assert!(format!("{short:?}").contains('8'));
}

#[test]
fn syntax_and_trailing_data_are_closed_failures() {
    let syntax = require_rejection(
        decode_json::<serde_json::Value>(br#"{"open":]"#, limits()),
        "malformed JSON must reject",
    );
    assert_eq!(syntax.kind(), &DecodeErrorKind::Syntax);
    assert_eq!(syntax.path(), "$.<redacted>");

    let trailing = require_rejection(
        decode_json::<serde_json::Value>(br#"{} {}"#, limits()),
        "a second JSON value must reject",
    );
    assert_eq!(trailing.kind(), &DecodeErrorKind::TrailingData);
    assert_eq!(trailing.path(), "$");
}
