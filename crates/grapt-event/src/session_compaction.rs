//! The strict canonical payload contract for `session.compaction.ended.2`.
//!
//! # What this owns
//!
//! The Rust-native compaction descriptor's wire form: all ELEVEN ratified
//! coordinates of [`CompactionDescriptor`], encoded through this crate's
//! existing [`crate::canonical`] writer so byte and hash determinism are
//! inherited rather than re-invented, and decoded by a hand-rolled fail-closed
//! reader in the style of the instruction crate's `strict_object`.
//!
//! v1 of the same boundary is the frozen oracle payload
//! (`{sessionID, reason, text, recent}`) and is NOT this contract. Decode keys
//! on the exact `(name, version)` pair, so the two shapes can never answer for
//! each other.
//!
//! # Fail-closed, in four categories
//!
//! Every coordinate is REQUIRED. There is no `serde`, no `Option` standing in
//! for a missing field, and no default. A payload is refused when a field is
//! missing, when it is explicitly `null`, when it has the wrong JSON type, or
//! when it carries a field this contract does not name. The last category is
//! what stops a future writer from smuggling an unreviewed coordinate through
//! a decoder that would otherwise ignore it.
//!
//! The one optional VALUE in the contract — a summary's model variant — is
//! explicitly `Optional<T>` in the crate-wide sense: absent means absent, and
//! an explicit `null` is refused. That keeps "no variant is bound" and "a
//! variant was recorded as nothing" from collapsing into one state.
//!
//! # References only, structurally
//!
//! Retained history, the world-state baseline and the next request's context
//! are [`ArtifactRef`]s: an id, an exact length and a digest. This module
//! defines no field anywhere that could carry inline bytes or text, so §4.2's
//! "does not inline duplicate copies of retained history" is not a rule this
//! codec follows — it is a shape it cannot express. That is why the law has a
//! type-carried probe rather than a mutation row: there is no line to weaken.

use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, CompactionDescriptor, CompactionDescriptorParts,
    CompactionId, CompactionSummary, CompactionTrigger, ContextEpoch, ContextWindowId,
    ModelCatalogVersion, ModelId, ModelVariantId, NextRequestRef, ProviderId, RetainedHistory,
    RetentionPolicyVersion, SessionId, StepNumber, SummaryCompatibility,
};
use serde_json::{Map, Value};

/// Why a `session.compaction.ended.2` payload is not a descriptor.
///
/// The categories are separate because they are separate defects: a missing
/// field is a writer that never had the coordinate, an explicit null is a
/// writer that had it and erased it, a wrong type is a writer that meant
/// something else, and an unknown field is a writer this contract has not been
/// reviewed against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    /// The payload is not a JSON object at all.
    NotAnObject { at: &'static str },
    /// A required field is absent.
    MissingField { at: &'static str, field: String },
    /// A field is present as an explicit `null`.
    NullField { at: &'static str, field: String },
    /// A field is present with the wrong JSON type or an invalid value.
    InvalidField {
        at: &'static str,
        field: String,
        expected: &'static str,
    },
    /// A field this contract does not name.
    UnknownField { at: &'static str, field: String },
    /// A closed vocabulary received a value outside it.
    UnknownVariant {
        at: &'static str,
        field: String,
        value: String,
    },
    /// The coordinates decoded, but the descriptor's own constructor refused
    /// them — a no-progress epoch pair, for instance. Carried rather than
    /// flattened so a structurally valid payload that is not a legal
    /// descriptor stays distinguishable from a malformed one.
    RefusedByConstructor {
        failure: grapt_types::ContextFailure,
    },
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PayloadError::NotAnObject { at } => write!(f, "{at} is not a JSON object"),
            PayloadError::MissingField { at, field } => {
                write!(f, "{at} is missing required field `{field}`")
            }
            PayloadError::NullField { at, field } => {
                write!(
                    f,
                    "{at} field `{field}` is null; this contract has no null values"
                )
            }
            PayloadError::InvalidField {
                at,
                field,
                expected,
            } => write!(f, "{at} field `{field}` must be {expected}"),
            PayloadError::UnknownField { at, field } => {
                write!(f, "{at} carries unknown field `{field}`")
            }
            PayloadError::UnknownVariant { at, field, value } => {
                write!(f, "{at} field `{field}` has unknown value `{value}`")
            }
            PayloadError::RefusedByConstructor { failure } => {
                write!(
                    f,
                    "the decoded coordinates are not a legal descriptor: {failure:?}"
                )
            }
        }
    }
}

impl std::error::Error for PayloadError {}

const AT_ROOT: &str = "session.compaction.ended.2";
const AT_SUMMARY: &str = "session.compaction.ended.2 summary";
const AT_COMPAT: &str = "session.compaction.ended.2 summary compatibility";
const AT_RETAINED: &str = "session.compaction.ended.2 retained";
const AT_NEXT: &str = "session.compaction.ended.2 nextRequest";
const AT_ARTIFACT: &str = "session.compaction.ended.2 artifact reference";

const ROOT_FIELDS: &[&str] = &[
    "compactionID",
    "sourceSession",
    "sourceEpoch",
    "targetWindow",
    "targetEpoch",
    "trigger",
    "retentionPolicy",
    "summary",
    "retained",
    "baseline",
    "nextRequest",
];

/// Encode a descriptor into its canonical payload value.
///
/// Total: every descriptor encodes, because every coordinate is required in
/// the type as well as the wire form. [`canonical_bytes`] is the byte-exact
/// projection of this value.
pub fn encode(descriptor: &CompactionDescriptor) -> Value {
    let mut root = Map::new();
    root.insert(
        "compactionID".into(),
        Value::String(descriptor.id().as_str().to_owned()),
    );
    root.insert(
        "sourceSession".into(),
        Value::String(descriptor.source_session().as_str().to_owned()),
    );
    root.insert(
        "sourceEpoch".into(),
        Value::Number(descriptor.source_epoch().value().into()),
    );
    root.insert(
        "targetWindow".into(),
        Value::String(descriptor.target_window().as_str().to_owned()),
    );
    root.insert(
        "targetEpoch".into(),
        Value::Number(descriptor.target_epoch().value().into()),
    );
    root.insert(
        "trigger".into(),
        Value::String(trigger_str(descriptor.trigger()).to_owned()),
    );
    root.insert(
        "retentionPolicy".into(),
        Value::String(descriptor.retention_policy().as_str().to_owned()),
    );
    root.insert("summary".into(), encode_summary(descriptor.summary()));
    root.insert("retained".into(), encode_retained(descriptor.retained()));
    root.insert("baseline".into(), encode_artifact(descriptor.baseline()));
    root.insert(
        "nextRequest".into(),
        encode_next_request(descriptor.next_request()),
    );
    Value::Object(root)
}

/// The exact canonical bytes of a descriptor's payload.
pub fn canonical_bytes(descriptor: &CompactionDescriptor) -> Vec<u8> {
    crate::canonical::canonical_string(&encode(descriptor)).into_bytes()
}

/// Decode a payload back into a descriptor, refusing anything that is not
/// exactly this contract.
pub fn decode(value: &Value) -> Result<CompactionDescriptor, PayloadError> {
    let root = object(value, AT_ROOT)?;
    known_fields_only(root, AT_ROOT, ROOT_FIELDS)?;

    let parts = CompactionDescriptorParts {
        id: CompactionId::new(string_field(root, AT_ROOT, "compactionID")?)
            .ok_or_else(|| invalid(AT_ROOT, "compactionID", "a non-empty compaction id"))?,
        // A prefixed identity: `parse` is its validating constructor, so a
        // foreign prefix is refused here rather than stored as a session id.
        source_session: string_field(root, AT_ROOT, "sourceSession")?
            .parse::<SessionId>()
            .map_err(|_| invalid(AT_ROOT, "sourceSession", "a valid `ses_*` session id"))?,
        source_epoch: ContextEpoch::new(u64_field(root, AT_ROOT, "sourceEpoch")?),
        target_window: ContextWindowId::new(string_field(root, AT_ROOT, "targetWindow")?)
            .ok_or_else(|| invalid(AT_ROOT, "targetWindow", "a non-empty context window id"))?,
        target_epoch: ContextEpoch::new(u64_field(root, AT_ROOT, "targetEpoch")?),
        trigger: decode_trigger(string_field(root, AT_ROOT, "trigger")?)?,
        retention_policy: RetentionPolicyVersion::new(string_field(
            root,
            AT_ROOT,
            "retentionPolicy",
        )?)
        .ok_or_else(|| invalid(AT_ROOT, "retentionPolicy", "a non-empty policy version"))?,
        summary: decode_summary(require(root, AT_ROOT, "summary")?)?,
        retained: decode_retained(require(root, AT_ROOT, "retained")?)?,
        baseline: decode_artifact(require(root, AT_ROOT, "baseline")?)?,
        next_request: decode_next_request(require(root, AT_ROOT, "nextRequest")?)?,
    };

    // The constructor is the ONLY way a descriptor exists, so a payload whose
    // coordinates are individually well-formed but jointly illegal — a target
    // epoch that does not advance — is refused here rather than assembled.
    CompactionDescriptor::new(parts)
        .map_err(|failure| PayloadError::RefusedByConstructor { failure })
}

fn trigger_str(trigger: CompactionTrigger) -> &'static str {
    match trigger {
        CompactionTrigger::ContextOverflow => "contextOverflow",
        CompactionTrigger::PolicyThreshold => "policyThreshold",
        CompactionTrigger::UserRequested => "userRequested",
        CompactionTrigger::EmergencyHeadroom => "emergencyHeadroom",
        CompactionTrigger::StructuralBound => "structuralBound",
    }
}

fn decode_trigger(raw: &str) -> Result<CompactionTrigger, PayloadError> {
    match raw {
        "contextOverflow" => Ok(CompactionTrigger::ContextOverflow),
        "policyThreshold" => Ok(CompactionTrigger::PolicyThreshold),
        "userRequested" => Ok(CompactionTrigger::UserRequested),
        "emergencyHeadroom" => Ok(CompactionTrigger::EmergencyHeadroom),
        "structuralBound" => Ok(CompactionTrigger::StructuralBound),
        other => Err(PayloadError::UnknownVariant {
            at: AT_ROOT,
            field: "trigger".into(),
            value: other.to_owned(),
        }),
    }
}

fn encode_artifact(artifact: &ArtifactRef) -> Value {
    let mut object = Map::new();
    object.insert(
        "id".into(),
        Value::String(artifact.id().as_str().to_owned()),
    );
    object.insert("length".into(), Value::Number(artifact.length().into()));
    object.insert(
        "digest".into(),
        Value::String(artifact.digest().as_str().to_owned()),
    );
    Value::Object(object)
}

fn decode_artifact(value: &Value) -> Result<ArtifactRef, PayloadError> {
    let object = object(value, AT_ARTIFACT)?;
    known_fields_only(object, AT_ARTIFACT, &["id", "length", "digest"])?;
    let id = ArtifactId::new(string_field(object, AT_ARTIFACT, "id")?)
        .ok_or_else(|| invalid(AT_ARTIFACT, "id", "a non-empty artifact id"))?;
    let length = u64_field(object, AT_ARTIFACT, "length")?;
    let digest = ArtifactDigest::new(string_field(object, AT_ARTIFACT, "digest")?)
        .ok_or_else(|| invalid(AT_ARTIFACT, "digest", "a non-empty digest"))?;
    Ok(ArtifactRef::new(id, length, digest))
}

fn encode_summary(summary: &CompactionSummary) -> Value {
    let mut object = Map::new();
    match summary {
        CompactionSummary::ClientReadable { artifact } => {
            object.insert("kind".into(), Value::String("clientReadable".into()));
            object.insert("artifact".into(), encode_artifact(artifact));
        }
        CompactionSummary::ProviderBound {
            artifact,
            compatibility,
        } => {
            object.insert("kind".into(), Value::String("providerBound".into()));
            object.insert("artifact".into(), encode_artifact(artifact));
            object.insert("compatibility".into(), encode_compatibility(compatibility));
        }
    }
    Value::Object(object)
}

fn decode_summary(value: &Value) -> Result<CompactionSummary, PayloadError> {
    let object = object(value, AT_SUMMARY)?;
    match string_field(object, AT_SUMMARY, "kind")? {
        "clientReadable" => {
            known_fields_only(object, AT_SUMMARY, &["kind", "artifact"])?;
            Ok(CompactionSummary::ClientReadable {
                artifact: decode_artifact(require(object, AT_SUMMARY, "artifact")?)?,
            })
        }
        "providerBound" => {
            known_fields_only(object, AT_SUMMARY, &["kind", "artifact", "compatibility"])?;
            Ok(CompactionSummary::ProviderBound {
                artifact: decode_artifact(require(object, AT_SUMMARY, "artifact")?)?,
                compatibility: decode_compatibility(require(object, AT_SUMMARY, "compatibility")?)?,
            })
        }
        other => Err(PayloadError::UnknownVariant {
            at: AT_SUMMARY,
            field: "kind".into(),
            value: other.to_owned(),
        }),
    }
}

fn encode_compatibility(compatibility: &SummaryCompatibility) -> Value {
    let mut object = Map::new();
    object.insert(
        "provider".into(),
        Value::String(compatibility.provider().as_str().to_owned()),
    );
    object.insert(
        "model".into(),
        Value::String(compatibility.model().as_str().to_owned()),
    );
    // Absent means absent. The key is OMITTED rather than written as null,
    // because this contract has no null values and "recorded as nothing" is
    // not a state a bound summary can be in.
    if let Some(variant) = compatibility.variant() {
        object.insert("variant".into(), Value::String(variant.as_str().to_owned()));
    }
    object.insert(
        "catalogVersion".into(),
        Value::String(compatibility.catalog_version().as_str().to_owned()),
    );
    Value::Object(object)
}

/// Reconstitute inert compatibility data from validated persisted fields.
///
/// This is the decode route named in the authority ruling. It reaches
/// [`SummaryCompatibility::from_validated_parts`], which conveys NO catalog
/// authority: nothing here constructs a `ModelRef`, consults a catalog, or
/// claims the stored bytes were catalog-vouched. It cannot — this crate has no
/// path to either, and a `ModelRef`'s only public constructor takes an entry
/// that only a loaded catalog mints.
fn decode_compatibility(value: &Value) -> Result<SummaryCompatibility, PayloadError> {
    let object = object(value, AT_COMPAT)?;
    known_fields_only(
        object,
        AT_COMPAT,
        &["provider", "model", "variant", "catalogVersion"],
    )?;
    let provider = ProviderId::new(string_field(object, AT_COMPAT, "provider")?)
        .ok_or_else(|| invalid(AT_COMPAT, "provider", "a non-empty provider id"))?;
    let model = ModelId::new(string_field(object, AT_COMPAT, "model")?)
        .ok_or_else(|| invalid(AT_COMPAT, "model", "a non-empty model id"))?;
    // `Optional<T>`: missing is Absent, and an explicit null is refused by
    // `string_field`'s null category rather than read as "no variant".
    let variant = match object.get("variant") {
        None => None,
        Some(_) => Some(
            ModelVariantId::new(string_field(object, AT_COMPAT, "variant")?)
                .ok_or_else(|| invalid(AT_COMPAT, "variant", "a non-empty variant id"))?,
        ),
    };
    let catalog_version =
        ModelCatalogVersion::new(string_field(object, AT_COMPAT, "catalogVersion")?)
            .ok_or_else(|| invalid(AT_COMPAT, "catalogVersion", "a non-empty catalog version"))?;
    Ok(SummaryCompatibility::from_validated_parts(
        provider,
        model,
        variant,
        catalog_version,
    ))
}

fn encode_retained(retained: &RetainedHistory) -> Value {
    let mut object = Map::new();
    match retained {
        RetainedHistory::References { artifacts } => {
            object.insert("kind".into(), Value::String("references".into()));
            object.insert(
                "artifacts".into(),
                Value::Array(artifacts.iter().map(encode_artifact).collect()),
            );
        }
        RetainedHistory::DeterministicSelection { descriptor } => {
            object.insert(
                "kind".into(),
                Value::String("deterministicSelection".into()),
            );
            object.insert("descriptor".into(), encode_artifact(descriptor));
        }
    }
    Value::Object(object)
}

fn decode_retained(value: &Value) -> Result<RetainedHistory, PayloadError> {
    let object = object(value, AT_RETAINED)?;
    match string_field(object, AT_RETAINED, "kind")? {
        "references" => {
            known_fields_only(object, AT_RETAINED, &["kind", "artifacts"])?;
            let raw = require(object, AT_RETAINED, "artifacts")?;
            let Value::Array(items) = raw else {
                return Err(invalid(AT_RETAINED, "artifacts", "an array of references"));
            };
            let mut artifacts = Vec::with_capacity(items.len());
            for item in items {
                artifacts.push(decode_artifact(item)?);
            }
            Ok(RetainedHistory::References { artifacts })
        }
        "deterministicSelection" => {
            known_fields_only(object, AT_RETAINED, &["kind", "descriptor"])?;
            Ok(RetainedHistory::DeterministicSelection {
                descriptor: decode_artifact(require(object, AT_RETAINED, "descriptor")?)?,
            })
        }
        other => Err(PayloadError::UnknownVariant {
            at: AT_RETAINED,
            field: "kind".into(),
            value: other.to_owned(),
        }),
    }
}

fn encode_next_request(next: &NextRequestRef) -> Value {
    let mut object = Map::new();
    object.insert("step".into(), Value::Number(next.step().value().into()));
    object.insert("context".into(), encode_artifact(next.context()));
    Value::Object(object)
}

fn decode_next_request(value: &Value) -> Result<NextRequestRef, PayloadError> {
    let object = object(value, AT_NEXT)?;
    // Both dimensions required, per dimension. Probe 46's law carried to the
    // wire: a next-request reference that names a step but no context, or a
    // context but no step, cannot reassemble anything.
    known_fields_only(object, AT_NEXT, &["step", "context"])?;
    let raw_step = u64_field(object, AT_NEXT, "step")?;
    let step = u32::try_from(raw_step)
        .ok()
        .and_then(StepNumber::new)
        .ok_or_else(|| invalid(AT_NEXT, "step", "a positive step number"))?;
    let context = decode_artifact(require(object, AT_NEXT, "context")?)?;
    Ok(NextRequestRef::new(step, context))
}

fn object<'a>(value: &'a Value, at: &'static str) -> Result<&'a Map<String, Value>, PayloadError> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(PayloadError::NotAnObject { at }),
    }
}

fn known_fields_only(
    object: &Map<String, Value>,
    at: &'static str,
    known: &[&str],
) -> Result<(), PayloadError> {
    match object.keys().find(|key| !known.contains(&key.as_str())) {
        Some(unknown) => Err(PayloadError::UnknownField {
            at,
            field: unknown.clone(),
        }),
        None => Ok(()),
    }
}

fn require<'a>(
    object: &'a Map<String, Value>,
    at: &'static str,
    field: &str,
) -> Result<&'a Value, PayloadError> {
    match object.get(field) {
        None => Err(PayloadError::MissingField {
            at,
            field: field.to_owned(),
        }),
        Some(Value::Null) => Err(PayloadError::NullField {
            at,
            field: field.to_owned(),
        }),
        Some(value) => Ok(value),
    }
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    at: &'static str,
    field: &str,
) -> Result<&'a str, PayloadError> {
    require(object, at, field)?
        .as_str()
        .ok_or_else(|| invalid(at, field, "a string"))
}

fn u64_field(
    object: &Map<String, Value>,
    at: &'static str,
    field: &str,
) -> Result<u64, PayloadError> {
    require(object, at, field)?
        .as_u64()
        .ok_or_else(|| invalid(at, field, "a non-negative integer"))
}

fn invalid(at: &'static str, field: &str, expected: &'static str) -> PayloadError {
    PayloadError::InvalidField {
        at,
        field: field.to_owned(),
        expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn artifact(id: &str) -> Option<ArtifactRef> {
        Some(ArtifactRef::new(
            ArtifactId::new(id)?,
            7,
            ArtifactDigest::new("digest")?,
        ))
    }

    fn compatibility(variant: Option<&str>) -> Option<SummaryCompatibility> {
        Some(SummaryCompatibility::from_validated_parts(
            ProviderId::new("anthropic")?,
            ModelId::new("claude-opus-5")?,
            match variant {
                Some(v) => Some(ModelVariantId::new(v)?),
                None => None,
            },
            ModelCatalogVersion::new("release-1")?,
        ))
    }

    fn parts(summary: CompactionSummary) -> Option<CompactionDescriptorParts> {
        Some(CompactionDescriptorParts {
            id: CompactionId::new("cmp-1")?,
            source_session: "ses_033601c26ffe".parse().ok()?,
            source_epoch: ContextEpoch::new(1),
            target_window: ContextWindowId::new("cwn-1")?,
            target_epoch: ContextEpoch::new(2),
            trigger: CompactionTrigger::ContextOverflow,
            retention_policy: RetentionPolicyVersion::new("ret-1")?,
            summary,
            retained: RetainedHistory::References {
                artifacts: vec![artifact("hist-1")?, artifact("hist-2")?],
            },
            baseline: artifact("base-1")?,
            next_request: NextRequestRef::new(StepNumber::new(3)?, artifact("req-1")?),
        })
    }

    fn neutral() -> Option<CompactionSummary> {
        Some(CompactionSummary::ClientReadable {
            artifact: artifact("sum-neutral")?,
        })
    }

    fn descriptor(summary: CompactionSummary) -> Option<CompactionDescriptor> {
        CompactionDescriptor::new(parts(summary)?).ok()
    }

    fn payload(summary: CompactionSummary) -> Option<Value> {
        Some(encode(&descriptor(summary)?))
    }

    #[test]
    fn both_summary_arms_round_trip_totally() -> TestResult {
        // Total equality, not field-spotting: a codec checked field by field
        // silently drops whatever the check forgot, and a descriptor that
        // loses a coordinate across durability is the exact defect 4.2 is
        // about.
        for summary in [
            neutral().ok_or("neutral")?,
            CompactionSummary::ProviderBound {
                artifact: artifact("sum-bound").ok_or("artifact")?,
                compatibility: compatibility(Some("thinking")).ok_or("compatibility")?,
            },
            CompactionSummary::ProviderBound {
                artifact: artifact("sum-bound").ok_or("artifact")?,
                compatibility: compatibility(None).ok_or("compatibility")?,
            },
        ] {
            let original = descriptor(summary).ok_or("descriptor")?;
            let decoded = decode(&encode(&original))?;
            assert_eq!(decoded, original, "descriptor must round trip exactly");
            // And the bytes are stable, so a stored payload re-encodes
            // identically rather than merely decoding to an equal value.
            assert_eq!(canonical_bytes(&decoded), canonical_bytes(&original));
        }
        Ok(())
    }

    #[test]
    fn a_deterministic_selection_round_trips_as_a_reference_too() -> TestResult {
        let mut raw = parts(neutral().ok_or("neutral")?).ok_or("parts")?;
        raw.retained = RetainedHistory::DeterministicSelection {
            descriptor: artifact("sel-1").ok_or("artifact")?,
        };
        let original = CompactionDescriptor::new(raw).map_err(|e| format!("{e:?}"))?;
        assert_eq!(decode(&encode(&original))?, original);
        Ok(())
    }

    #[test]
    fn swr_c1_structural_bound_trigger_round_trips_as_public_data() -> TestResult {
        let mut raw = parts(neutral().ok_or("neutral")?).ok_or("parts")?;
        raw.trigger = CompactionTrigger::StructuralBound;
        let original = CompactionDescriptor::new(raw).map_err(|error| format!("{error:?}"))?;
        let encoded = encode(&original);
        assert_eq!(
            encoded.get("trigger"),
            Some(&Value::from("structuralBound"))
        );
        assert_eq!(decode(&encoded)?, original);
        Ok(())
    }

    #[test]
    fn every_required_root_coordinate_refuses_when_missing() -> TestResult {
        // Table-driven over the ELEVEN coordinates: a hand-listed subset is
        // how one coordinate ends up optional by accident.
        for field in ROOT_FIELDS {
            let mut value = payload(neutral().ok_or("neutral")?).ok_or("payload")?;
            value
                .as_object_mut()
                .ok_or("object")?
                .remove(*field)
                .ok_or_else(|| format!("{field} was not in the encoded payload"))?;
            assert_eq!(
                decode(&value),
                Err(PayloadError::MissingField {
                    at: AT_ROOT,
                    field: (*field).to_owned(),
                }),
                "removing {field} must refuse"
            );
        }
        Ok(())
    }

    #[test]
    fn an_explicit_null_is_refused_separately_from_a_missing_field() -> TestResult {
        // Null and missing are different writer defects and must not merge:
        // one never had the coordinate, the other erased it.
        for field in ROOT_FIELDS {
            let mut value = payload(neutral().ok_or("neutral")?).ok_or("payload")?;
            value
                .as_object_mut()
                .ok_or("object")?
                .insert((*field).to_owned(), Value::Null);
            assert_eq!(
                decode(&value),
                Err(PayloadError::NullField {
                    at: AT_ROOT,
                    field: (*field).to_owned(),
                }),
                "a null {field} must refuse as null, not as missing"
            );
        }
        Ok(())
    }

    #[test]
    fn a_wrong_type_and_an_unknown_field_each_refuse_in_their_own_category() -> TestResult {
        let mut wrong = payload(neutral().ok_or("neutral")?).ok_or("payload")?;
        wrong
            .as_object_mut()
            .ok_or("object")?
            .insert("sourceEpoch".into(), Value::String("1".into()));
        assert_eq!(
            decode(&wrong),
            Err(PayloadError::InvalidField {
                at: AT_ROOT,
                field: "sourceEpoch".into(),
                expected: "a non-negative integer",
            })
        );

        let mut unknown = payload(neutral().ok_or("neutral")?).ok_or("payload")?;
        unknown
            .as_object_mut()
            .ok_or("object")?
            .insert("smuggled".into(), Value::Bool(true));
        assert_eq!(
            decode(&unknown),
            Err(PayloadError::UnknownField {
                at: AT_ROOT,
                field: "smuggled".into(),
            }),
            "an unreviewed coordinate must not ride along ignored"
        );
        Ok(())
    }

    #[test]
    fn closed_vocabularies_refuse_values_outside_them() -> TestResult {
        let mut trigger = payload(neutral().ok_or("neutral")?).ok_or("payload")?;
        trigger
            .as_object_mut()
            .ok_or("object")?
            .insert("trigger".into(), Value::String("becauseIFeltLikeIt".into()));
        assert!(matches!(
            decode(&trigger),
            Err(PayloadError::UnknownVariant { field, .. }) if field == "trigger"
        ));

        let mut summary = payload(neutral().ok_or("neutral")?).ok_or("payload")?;
        summary
            .as_object_mut()
            .ok_or("object")?
            .get_mut("summary")
            .and_then(Value::as_object_mut)
            .ok_or("summary")?
            .insert("kind".into(), Value::String("providerEncrypted".into()));
        assert!(matches!(
            decode(&summary),
            Err(PayloadError::UnknownVariant { at, .. }) if at == AT_SUMMARY
        ));
        Ok(())
    }

    #[test]
    fn an_unknown_retained_kind_is_refused_by_its_own_closed_vocabulary() -> TestResult {
        // `retained` is the THIRD closed vocabulary in this payload, and it was
        // the one left unwitnessed: the neighbouring test covers `trigger` and
        // the summary arm, so a decoder that fell through on an unknown
        // retained kind would have passed every codec test here.
        //
        // It is a separate test rather than a third clause of that one so the
        // two laws stay separately attributable — a mutation to this arm must
        // red THIS witness while the summary control stays correctly green.
        //
        // The refusal is fail-closed durability, not tidiness. `retained`
        // decides how history is carried, and an unknown kind that decoded as
        // an empty `References` would silently report that a compaction
        // retained NOTHING — the descriptor would commit, the fork lineage
        // behind it would look empty, and the loss would read as a fact rather
        // than a refusal. `inlineHistory` is used as the unknown value because
        // it names the exact shape §4.2 forbids.
        let mut retained = payload(neutral().ok_or("neutral")?).ok_or("payload")?;
        retained
            .as_object_mut()
            .ok_or("object")?
            .get_mut("retained")
            .and_then(Value::as_object_mut)
            .ok_or("retained")?
            .insert("kind".into(), Value::String("inlineHistory".into()));

        let refusal = decode(&retained);
        assert!(
            matches!(
                &refusal,
                Err(PayloadError::UnknownVariant { at, field, value })
                    if *at == AT_RETAINED && field == "kind" && value == "inlineHistory"
            ),
            "an unknown retained kind must refuse as UnknownVariant at the \
             retained coordinate, naming the value it refused: {refusal:?}"
        );
        Ok(())
    }

    #[test]
    fn the_next_request_reference_refuses_per_dimension() -> TestResult {
        // Probe 46's law carried to the wire, and per DIMENSION: a reference
        // naming a step but no context, or a context but no step, reassembles
        // nothing. One combined check would let either half rot.
        for dimension in ["step", "context"] {
            let mut value = payload(neutral().ok_or("neutral")?).ok_or("payload")?;
            value
                .as_object_mut()
                .ok_or("object")?
                .get_mut("nextRequest")
                .and_then(Value::as_object_mut)
                .ok_or("nextRequest")?
                .remove(dimension)
                .ok_or("dimension was not encoded")?;
            assert_eq!(
                decode(&value),
                Err(PayloadError::MissingField {
                    at: AT_NEXT,
                    field: dimension.to_owned(),
                })
            );
        }

        // Step zero is not a step. `StepNumber` refuses it, and the codec must
        // surface that rather than coercing to one.
        let mut zero = payload(neutral().ok_or("neutral")?).ok_or("payload")?;
        zero.as_object_mut()
            .ok_or("object")?
            .get_mut("nextRequest")
            .and_then(Value::as_object_mut)
            .ok_or("nextRequest")?
            .insert("step".into(), Value::Number(0.into()));
        assert!(matches!(
            decode(&zero),
            Err(PayloadError::InvalidField { field, .. }) if field == "step"
        ));
        Ok(())
    }

    #[test]
    fn each_compatibility_coordinate_survives_and_is_load_bearing() -> TestResult {
        let bound = CompactionSummary::ProviderBound {
            artifact: artifact("sum-bound").ok_or("artifact")?,
            compatibility: compatibility(Some("thinking")).ok_or("compatibility")?,
        };
        let encoded = payload(bound).ok_or("payload")?;

        // Each coordinate is required. Dropping any one refuses, so none of
        // the three can quietly default.
        for field in ["provider", "model", "catalogVersion"] {
            let mut value = encoded.clone();
            value
                .as_object_mut()
                .ok_or("object")?
                .get_mut("summary")
                .and_then(Value::as_object_mut)
                .ok_or("summary")?
                .get_mut("compatibility")
                .and_then(Value::as_object_mut)
                .ok_or("compatibility")?
                .remove(field)
                .ok_or("field was not encoded")?;
            assert_eq!(
                decode(&value),
                Err(PayloadError::MissingField {
                    at: AT_COMPAT,
                    field: field.to_owned(),
                })
            );
        }

        // The variant is Optional: ABSENT is legal and survives as absent,
        // while an explicit null is refused. Collapsing those two would make
        // "no variant is bound" and "a variant was recorded as nothing" the
        // same state.
        let absent = decode(
            &payload(CompactionSummary::ProviderBound {
                artifact: artifact("sum-bound").ok_or("artifact")?,
                compatibility: compatibility(None).ok_or("compatibility")?,
            })
            .ok_or("payload")?,
        )?;
        match absent.summary() {
            CompactionSummary::ProviderBound { compatibility, .. } => {
                assert_eq!(compatibility.variant(), None);
            }
            other => panic!("expected a provider-bound summary, got {other:?}"),
        }

        let mut null_variant = encoded.clone();
        null_variant
            .as_object_mut()
            .ok_or("object")?
            .get_mut("summary")
            .and_then(Value::as_object_mut)
            .ok_or("summary")?
            .get_mut("compatibility")
            .and_then(Value::as_object_mut)
            .ok_or("compatibility")?
            .insert("variant".into(), Value::Null);
        assert_eq!(
            decode(&null_variant),
            Err(PayloadError::NullField {
                at: AT_COMPAT,
                field: "variant".into(),
            })
        );
        Ok(())
    }

    #[test]
    fn a_structurally_valid_payload_that_is_not_a_legal_descriptor_is_refused() -> TestResult {
        // The constructor is the only way a descriptor exists, so a payload
        // whose fields are each well-formed but whose epochs do not advance
        // must not assemble. Without this the codec would be a second, weaker
        // door into the same type.
        let mut value = payload(neutral().ok_or("neutral")?).ok_or("payload")?;
        value
            .as_object_mut()
            .ok_or("object")?
            .insert("targetEpoch".into(), Value::Number(1.into()));
        assert_eq!(
            decode(&value),
            Err(PayloadError::RefusedByConstructor {
                failure: grapt_types::ContextFailure::CompactionNoProgress,
            })
        );
        Ok(())
    }
}
