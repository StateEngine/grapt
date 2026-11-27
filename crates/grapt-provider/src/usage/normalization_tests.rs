//! Normalization witnesses for the route equations, the provenance ledger,
//! the stamped protocol family, and the language-neutral corpus.
use super::*;
use grapt_types::usage::UsageProvenance;

const CORPUS_BYTES: usize = 64 * 1_024;
const CORPUS_ACTIONS: usize = 32;

fn corpus_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/provider-usage-normalization-v1")
}

fn bounded_fixture(path: &std::path::Path) -> String {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => panic!("required fixture {} is unreadable: {error}", path.display()),
    };
    assert!(
        bytes.len() <= CORPUS_BYTES,
        "fixture {} exceeds the {CORPUS_BYTES}-byte bound",
        path.display()
    );
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => panic!("fixture {} is not UTF-8", path.display()),
    }
}

fn leaf(field: &UsageField) -> Value {
    let Some(measurement) = field.measurement() else {
        return Value::Null;
    };
    let mut out = Map::new();
    out.insert("count".to_owned(), Value::from(measurement.count().value()));
    match measurement.provenance() {
        UsageProvenance::ProviderReported => {
            out.insert("provenance".to_owned(), Value::from("provider-reported"));
        }
        UsageProvenance::ProviderDerived { rule } => {
            out.insert("provenance".to_owned(), Value::from("provider-derived"));
            let rule = match rule {
                UsageDerivation::InputFromPromptAndToolUse => "input-from-prompt-and-tool-use",
                UsageDerivation::OutputFromTotalLessInput => "output-from-total-less-input",
                UsageDerivation::FreshFromInclusiveInput => "fresh-from-inclusive",
                UsageDerivation::InclusiveInputFromPartition => "inclusive-from-partition",
            };
            out.insert("rule".to_owned(), Value::from(rule));
        }
        UsageProvenance::HarnessEstimated { policy } => {
            out.insert("provenance".to_owned(), Value::from("harness-estimated"));
            out.insert("policy".to_owned(), Value::from(policy.as_str()));
        }
    }
    Value::Object(out)
}

fn accepted(usage: &TokenUsage) -> Value {
    serde_json::json!({
        "outcome": "accepted",
        "input_total": leaf(usage.input_total()),
        "input_fresh": leaf(usage.input_fresh()),
        "output": leaf(usage.output()),
        "reasoning": leaf(usage.reasoning()),
        "cache_read": leaf(usage.cache_read()),
        "cache_write": leaf(usage.cache_write()),
        "total": leaf(usage.total()),
    })
}

fn refusal(error: &UsageNormalizationError) -> Value {
    let reason = match error.reason() {
        UsageNormalizationReason::Null => "null",
        UsageNormalizationReason::WrongType => "wrong-type",
        UsageNormalizationReason::OutOfRange(_) => "out-of-range",
        UsageNormalizationReason::PartitionExceedsTotal => "partition-exceeds-total",
        UsageNormalizationReason::ReportedTotalMismatch => "reported-total-mismatch",
        UsageNormalizationReason::ConflictingFrames => "conflicting-frames",
    };
    serde_json::json!({
        "outcome": "refused",
        "route": error.route().label(),
        "field": error.field().label(),
        "reason": reason,
    })
}

fn normalize_corpus_action(
    route: &str,
    wire: &Value,
) -> Result<Option<TokenUsage>, UsageNormalizationError> {
    match route {
        "responses" => normalize_responses_usage(wire, UsageRoute::OpenAiResponses),
        "codex" => normalize_responses_usage(wire, UsageRoute::ChatGptCodex),
        "anthropic" => normalize_anthropic_usage(wire),
        "compatible" => normalize_compatible_usage(wire),
        other => panic!("corpus names unknown route {other}"),
    }
}

#[test]
fn route_equations_are_distinct_and_provenance_is_per_leaf() {
    let responses = normalize_responses_usage(
        &serde_json::json!({"usage": {
            "input_tokens": 11,
            "input_tokens_details": {"cached_tokens": 2, "cache_write_tokens": 1},
            "output_tokens": 4,
            "output_tokens_details": {"reasoning_tokens": 3},
            "total_tokens": 15
        }}),
        UsageRoute::OpenAiResponses,
    )
    .unwrap_or_else(|error| panic!("responses: {error}"))
    .unwrap_or_else(|| panic!("responses usage absent"));
    assert_eq!(responses.input_total().value(), Some(11));
    assert_eq!(responses.input_fresh().value(), Some(8));
    assert!(matches!(
        responses.input_fresh().provenance(),
        Some(UsageProvenance::ProviderDerived {
            rule: UsageDerivation::FreshFromInclusiveInput
        })
    ));

    let anthropic = normalize_anthropic_usage(&serde_json::json!({"usage": {
        "input_tokens": 8,
        "cache_read_input_tokens": 2,
        "cache_creation_input_tokens": 1,
        "output_tokens": 4
    }}))
    .unwrap_or_else(|error| panic!("anthropic: {error}"))
    .unwrap_or_else(|| panic!("anthropic usage absent"));
    assert_eq!(anthropic.input_fresh().value(), Some(8));
    assert_eq!(anthropic.input_total().value(), Some(11));

    let compatible = normalize_compatible_usage(&serde_json::json!({"usage": {
        "prompt_tokens": 11,
        "prompt_tokens_details": {"cached_tokens": 2, "cache_write_tokens": 1},
        "completion_tokens": 4,
        "completion_tokens_details": {"reasoning_tokens": 3},
        "total_tokens": 15
    }}))
    .unwrap_or_else(|error| panic!("compatible: {error}"))
    .unwrap_or_else(|| panic!("compatible usage absent"));
    assert_eq!(compatible.input_total().value(), Some(11));
    assert_eq!(compatible.input_fresh().value(), Some(8));
    assert_eq!(compatible.cache_write().value(), Some(1));
}

#[test]
fn absence_zero_malformed_bounds_and_partition_failure_are_distinct() {
    let partial = normalize_responses_usage(
        &serde_json::json!({"usage": {"input_tokens": 10}}),
        UsageRoute::ChatGptCodex,
    )
    .unwrap_or_else(|error| panic!("partial: {error}"))
    .unwrap_or_else(|| panic!("partial usage absent"));
    assert_eq!(partial.input_total().value(), Some(10));
    assert_eq!(partial.input_fresh().value(), None);

    let zero = normalize_compatible_usage(&serde_json::json!({"usage": {
        "prompt_tokens": 0,
        "prompt_tokens_details": {"cached_tokens": 0},
        "completion_tokens": 0,
        "total_tokens": 0
    }}))
    .unwrap_or_else(|error| panic!("zero: {error}"))
    .unwrap_or_else(|| panic!("zero usage absent"));
    assert_eq!(zero.input_fresh().value(), Some(0));

    for value in [
        serde_json::json!({"usage": {"prompt_tokens": -1}}),
        serde_json::json!({"usage": {"prompt_tokens": 1.5}}),
        serde_json::json!({"usage": {"prompt_tokens": "1"}}),
        serde_json::json!({"usage": {"prompt_tokens": 9007199254740993_u64}}),
    ] {
        assert!(
            normalize_compatible_usage(&value).is_err(),
            "accepted {value}"
        );
    }

    let partition = normalize_responses_usage(
        &serde_json::json!({"usage": {
            "input_tokens": 10,
            "input_tokens_details": {"cached_tokens": 7, "cache_write_tokens": 4}
        }}),
        UsageRoute::OpenAiResponses,
    )
    .err()
    .unwrap_or_else(|| panic!("over-partition accepted"));
    assert_eq!(
        partition.reason(),
        &UsageNormalizationReason::PartitionExceedsTotal
    );
}

/// The fresh-input expectation of one stamped route case: absent stays
/// absent, a derived value carries its exact rule.
enum FreshExpect {
    Absent,
    Value { value: u64, rule: UsageDerivation },
}

/// One route, its wire shape, and the family its protocol guarantees.
struct RouteStamp {
    name: &'static str,
    family: UsageProtocolFamily,
    normalize: fn(&Value) -> Result<Option<TokenUsage>, UsageNormalizationError>,
    wire: Value,
    fresh: Option<FreshExpect>,
}

/// Every route stamps the family its protocol guarantees: the partition
/// routes and the inclusive routes stamp without inventing a missing
/// counter, and Cohere's closed wire makes fresh exact from the zero-cache
/// derivation.
#[test]
fn every_route_stamps_the_family_its_protocol_guarantees() {
    let responses = |wire: &Value| normalize_responses_usage(wire, UsageRoute::OpenAiResponses);
    let codex = |wire: &Value| normalize_responses_usage(wire, UsageRoute::ChatGptCodex);
    let routes = [
        RouteStamp {
            name: "cohere",
            family: UsageProtocolFamily::InclusiveInputNoCache,
            normalize: normalize_cohere_usage,
            wire: serde_json::json!({"usage": {"tokens": {"input_tokens": 11, "output_tokens": 4}}}),
            fresh: Some(FreshExpect::Value {
                value: 11,
                rule: UsageDerivation::FreshFromInclusiveInput,
            }),
        },
        RouteStamp {
            name: "gemini without cache detail",
            family: UsageProtocolFamily::InclusiveInput,
            normalize: normalize_gemini_usage,
            wire: serde_json::json!({"usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 4, "totalTokenCount": 15}}),
            fresh: Some(FreshExpect::Absent),
        },
        RouteStamp {
            name: "responses without cache detail",
            family: UsageProtocolFamily::InclusiveInput,
            normalize: responses,
            wire: serde_json::json!({"usage": {"input_tokens": 11, "output_tokens": 4, "total_tokens": 15}}),
            fresh: Some(FreshExpect::Absent),
        },
        RouteStamp {
            name: "responses with full cache detail",
            family: UsageProtocolFamily::InclusiveInput,
            normalize: responses,
            wire: serde_json::json!({"usage": {"input_tokens": 11, "input_tokens_details": {"cached_tokens": 2, "cache_write_tokens": 1}, "output_tokens": 4, "total_tokens": 15}}),
            fresh: Some(FreshExpect::Value {
                value: 8,
                rule: UsageDerivation::FreshFromInclusiveInput,
            }),
        },
        RouteStamp {
            name: "anthropic",
            family: UsageProtocolFamily::FreshInputPartition,
            normalize: normalize_anthropic_usage,
            wire: serde_json::json!({"usage": {"input_tokens": 8, "output_tokens": 4}}),
            fresh: None,
        },
        RouteStamp {
            name: "bedrock",
            family: UsageProtocolFamily::FreshInputPartition,
            normalize: normalize_bedrock_usage,
            wire: serde_json::json!({"usage": {"inputTokens": 8, "outputTokens": 4}}),
            fresh: None,
        },
        RouteStamp {
            name: "compatible",
            family: UsageProtocolFamily::InclusiveInput,
            normalize: normalize_compatible_usage,
            wire: serde_json::json!({"usage": {"prompt_tokens": 11, "completion_tokens": 4, "total_tokens": 15}}),
            fresh: None,
        },
        RouteStamp {
            name: "codex",
            family: UsageProtocolFamily::InclusiveInput,
            normalize: codex,
            wire: serde_json::json!({"usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}),
            fresh: None,
        },
    ];
    for route in &routes {
        assert_route(route);
    }
}

/// Assert one route's stamp and fresh-input behavior.
fn assert_route(route: &RouteStamp) {
    let usage = (route.normalize)(&route.wire)
        .unwrap_or_else(|error| panic!("{}: {error}", route.name))
        .unwrap_or_else(|| panic!("{}: usage absent", route.name));
    assert_eq!(
        usage.protocol(),
        Some(route.family),
        "{}: family",
        route.name
    );
    match route.fresh {
        None => {}
        Some(FreshExpect::Absent) => {
            assert!(
                usage.input_fresh().is_absent(),
                "{}: fresh absent",
                route.name
            )
        }
        Some(FreshExpect::Value { value, rule }) => {
            assert_eq!(
                usage.input_fresh().value(),
                Some(value),
                "{}: fresh value",
                route.name
            );
            assert!(
                matches!(
                    usage.input_fresh().provenance(),
                    Some(UsageProvenance::ProviderDerived {
                        rule: held
                    }) if *held == rule
                ),
                "{}: fresh rule",
                route.name
            );
        }
    }
}

/// A family conflict never refuses: the stamp drops, the merge keeps a
/// sticky typed indication, and a later frame can neither restore the stamp
/// nor erase the conflict.
#[test]
fn a_merge_family_conflict_drops_the_stamp_and_keeps_the_indication() {
    let first = normalize_anthropic_usage(&serde_json::json!({"usage": {
        "input_tokens": 8,
        "output_tokens": 4
    }}))
    .unwrap_or_else(|error| panic!("anthropic: {error}"))
    .unwrap_or_else(|| panic!("anthropic usage absent"));
    let other = normalize_gemini_usage(&serde_json::json!({"usageMetadata": {
        "promptTokenCount": 11,
        "candidatesTokenCount": 4,
        "totalTokenCount": 15
    }}))
    .unwrap_or_else(|error| panic!("gemini: {error}"))
    .unwrap_or_else(|| panic!("gemini usage absent"));
    let merge = |a: &TokenUsage, b: &TokenUsage| {
        a.merge(b, UsageRoute::AnthropicMessages)
            .unwrap_or_else(|error| panic!("a merge must not refuse: {error}"))
    };
    let merged = merge(&first, &other);
    assert!(
        merged.protocol().is_none(),
        "the conflicting stamp must drop to unstamped"
    );
    assert!(
        merged.protocol_conflict(),
        "the conflict must stay indicated"
    );

    // Sticky: a later frame cannot restore the stamp or erase the conflict.
    let third = merge(&merged, &first);
    assert!(
        third.protocol().is_none(),
        "a later frame must not restore the stamp"
    );
    assert!(
        third.protocol_conflict(),
        "a later frame must not erase the conflict"
    );

    // No conflict: a same-family merge keeps the stamp and no indication.
    let same = merge(&first, &first);
    assert_eq!(
        same.protocol(),
        Some(UsageProtocolFamily::FreshInputPartition)
    );
    assert!(
        !same.protocol_conflict(),
        "a same-family merge has no conflict"
    );
}

#[test]
fn language_neutral_normalization_corpus_executes_every_route() {
    let root = corpus_root();
    let manifest_text = bounded_fixture(&root.join("manifest.json"));
    let manifest: Value = match serde_json::from_str(&manifest_text) {
        Ok(manifest) => manifest,
        Err(error) => panic!("normalization manifest is invalid JSON: {error}"),
    };
    assert_eq!(manifest["id"], "provider-usage-normalization-v1");
    assert_eq!(manifest["law_pack"]["revision"], 9);
    assert_eq!(
        manifest["law_pack"]["sources"][2]["sha256"],
        "31d4ae9af4229939225374efb9c721e752bc0fc163615e3f188acebed051b032"
    );
    assert_eq!(
        manifest["law_pack"]["sources"][3]["sha256"],
        "6c202f89e1e8bdc1de7304f22539ba043a4032f90376434841718f9b5624cdf6"
    );

    let actions = bounded_fixture(&root.join("actions.jsonl"));
    let mut accepted_count = 0_u64;
    let mut absent_count = 0_u64;
    let mut refused_count = 0_u64;
    let mut routes = std::collections::BTreeMap::<String, u64>::new();
    let mut action_count = 0_usize;
    for (index, line) in actions.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        action_count += 1;
        assert!(
            action_count <= CORPUS_ACTIONS,
            "normalization corpus exceeds {CORPUS_ACTIONS} actions"
        );
        let action: Value = match serde_json::from_str(line) {
            Ok(action) => action,
            Err(error) => panic!("action line {} is invalid JSON: {error}", index + 1),
        };
        let id = action["id"]
            .as_str()
            .unwrap_or_else(|| panic!("action line {} has no string id", index + 1));
        let route = action["route"]
            .as_str()
            .unwrap_or_else(|| panic!("action {id} has no string route"));
        let route_count = routes.entry(route.to_owned()).or_insert(0);
        *route_count += 1;
        let actual = match normalize_corpus_action(route, &action["wire"]) {
            Ok(Some(usage)) => {
                accepted_count += 1;
                accepted(&usage)
            }
            Ok(None) => {
                absent_count += 1;
                serde_json::json!({"outcome": "absent"})
            }
            Err(error) => {
                refused_count += 1;
                refusal(&error)
            }
        };
        assert_eq!(
            actual, action["expect"],
            "normalization action {id} produced the wrong truth"
        );
    }
    assert!(action_count > 0, "normalization corpus is empty");

    let expected_state_text = bounded_fixture(&root.join("expected/state.json"));
    let expected_state: Value = match serde_json::from_str(&expected_state_text) {
        Ok(state) => state,
        Err(error) => panic!("normalization expected state is invalid JSON: {error}"),
    };
    let actual_state = serde_json::json!({
        "accepted": accepted_count,
        "absent": absent_count,
        "refused": refused_count,
        "routes": routes,
    });
    assert_eq!(actual_state, expected_state);

    let durable = bounded_fixture(&root.join("expected/durable.jsonl"));
    assert!(
        durable.is_empty(),
        "provider normalization unexpectedly claims a durable write"
    );
    let waivers_text = bounded_fixture(&root.join("expected/waivers.json"));
    let waivers: Value = match serde_json::from_str(&waivers_text) {
        Ok(waivers) => waivers,
        Err(error) => panic!("normalization waivers are invalid JSON: {error}"),
    };
    assert_eq!(waivers, serde_json::json!([]), "fixture carries a waiver");
}
