//! The accepted surface, the refused surface, and the structured path.
//!
//! The accepted half is a PRESERVE claim measured from the oracle, which parses
//! with `jsonc-parser` under `{ allowTrailingComma: true }`
//! (the oracle's `config.ts:229`): JSON plus both comment forms plus one
//! trailing comma. The refused half is where this decoder is deliberately
//! stricter, and each refusal gets its own named witness because each is a
//! recorded departure rather than a shared "it is strict" claim.
//!
//! There are FOUR departures, not three. The surrogate refusals below were filed
//! under ordinary strictness and that was wrong: `jsonc-parser` 3.3.1 accepts
//! every syntactically valid `\uXXXX` and appends `String.fromCharCode` with no
//! pairing check, so refusing an isolated surrogate is a compatibility
//! correction. See its own section heading below.

use crate::support;

use grapt_config::{jsonc, ConfigValue, JsoncErrorKind};
use support::{bounds, custom_bounds};

fn decode(body: &str) -> Result<grapt_config::Spanned<ConfigValue>, grapt_config::JsoncError> {
    jsonc::decode(body.as_bytes(), bounds())
}

fn refusal(body: &str) -> JsoncErrorKind {
    match decode(body) {
        Ok(_) => panic!("expected {body:?} to be refused"),
        Err(error) => error.kind().clone(),
    }
}

// ---------------------------------------------------------------------------
// Accepted — the oracle's surface
// ---------------------------------------------------------------------------

#[test]
fn line_and_block_comments_are_accepted_anywhere_trivia_is_legal() {
    let body = "// lead\r{ /* mid */ \"a\" : 1 // tail\n , \"b\": 2 // end\r\n}\n/* after */";
    let value = match decode(body) {
        Ok(value) => value,
        Err(error) => panic!("comments must be accepted: {error}"),
    };
    assert!(value.value().defines("a"));
    assert!(value.value().defines("b"));
}

#[test]
fn one_trailing_comma_is_accepted_in_an_object_and_in_an_array() {
    match decode("{\"a\": [1, 2,],}") {
        Ok(value) => {
            let Some(a) = value.value().get("a") else {
                panic!("key `a` missing");
            };
            let Some(items) = a.value().as_array() else {
                panic!("`a` is {}", a.value().kind());
            };
            assert_eq!(items.len(), 2);
        }
        Err(error) => panic!("a trailing comma must be accepted: {error}"),
    }
}

#[test]
fn every_json_escape_decodes_including_a_surrogate_pair() {
    let body = r#"{"s":"q\" b\\ s\/ \b\f\n\r\t uA pair😀"}"#;
    let value = match decode(body) {
        Ok(value) => value,
        Err(error) => panic!("escapes must decode: {error}"),
    };
    let Some(entry) = value.value().get("s") else {
        panic!("key `s` missing");
    };
    let Some(text) = entry.value().as_str() else {
        panic!("`s` is {}", entry.value().kind());
    };
    assert_eq!(text, "q\" b\\ s/ \u{0008}\u{000C}\n\r\t uA pair\u{1F600}");
}

#[test]
fn an_explicit_null_is_a_present_value_and_not_an_absence() {
    let value = match decode("{\"a\": null}") {
        Ok(value) => value,
        Err(error) => panic!("null must decode: {error}"),
    };
    assert!(
        value.value().defines("a"),
        "an explicitly nulled key is PRESENT; master's presence categories make missing and explicit null distinct states"
    );
    assert!(!value.value().defines("b"));
    match value.value().get("a").map(|v| v.value()) {
        Some(ConfigValue::Null) => {}
        other => panic!("expected Null, got {other:?}"),
    }
}

#[test]
fn object_key_order_is_the_document_order() {
    let value = match decode("{\"z\":1,\"a\":2,\"m\":3}") {
        Ok(value) => value,
        Err(error) => panic!("decode: {error}"),
    };
    let Some(entries) = value.value().as_object() else {
        panic!("root is {}", value.value().kind());
    };
    let keys: Vec<&str> = entries.iter().map(|(k, _)| k.value().as_str()).collect();
    assert_eq!(
        keys,
        vec!["z", "a", "m"],
        "the oracle decodes with propertyOrder: \"original\", so order is observable behaviour"
    );
}

#[test]
fn a_number_keeps_its_exact_literal_and_offers_no_lossy_accessor_by_accident() {
    let value = match decode("{\"big\": 9007199254740993, \"real\": 1.5e-3}") {
        Ok(value) => value,
        Err(error) => panic!("decode: {error}"),
    };
    let Some(big) = value.value().get("big").and_then(|v| v.value().as_number()) else {
        panic!("`big` is not a number");
    };
    assert_eq!(big.literal(), "9007199254740993");
    assert_eq!(big.as_i64(), Some(9007199254740993));
    // The f64 round trip is exactly the precision loss the literal exists to
    // avoid, and the accessor that performs it says `lossy` in its name.
    assert_ne!(
        big.as_f64_lossy().map(|f| f as i64),
        Some(9007199254740993),
        "if f64 ever round-trips this literal the witness is testing nothing"
    );

    let Some(real) = value
        .value()
        .get("real")
        .and_then(|v| v.value().as_number())
    else {
        panic!("`real` is not a number");
    };
    assert_eq!(real.literal(), "1.5e-3");
    assert_eq!(real.as_i64(), None, "a fraction is not an exact i64");
}

#[test]
fn a_span_names_the_exact_bytes_a_value_occupied() {
    let body = "{\n  \"model\": \"opus\"\n}";
    let value = match decode(body) {
        Ok(value) => value,
        Err(error) => panic!("decode: {error}"),
    };
    let Some(entry) = value.value().get("model") else {
        panic!("key `model` missing");
    };
    let span = entry.span();
    assert_eq!(&body[span.start()..span.end()], "\"opus\"");
}

// ---------------------------------------------------------------------------
// Refused — the three deliberate departures from the oracle
// ---------------------------------------------------------------------------

/// `jsonc-parser` keeps the last duplicate silently, so the author is never told
/// that the first line does nothing. A document that says two different things
/// about one key has no single meaning.
#[test]
fn a_duplicate_object_key_is_refused_rather_than_last_wins() {
    match refusal(r#"{"model":"a","model":"b"}"#) {
        // The key is carried as `Redacted` and is deliberately not comparable to
        // a string here: it may itself be secret-bearing. The kind plus the
        // offset is what a caller gets, and it is enough.
        JsoncErrorKind::DuplicateKey { .. } => {}
        other => panic!("expected DuplicateKey, got {other:?}"),
    }
}

#[test]
fn the_empty_object_key_is_refused() {
    assert!(matches!(refusal(r#"{"":1}"#), JsoncErrorKind::EmptyKey));
}

#[test]
fn trailing_content_after_the_root_value_is_refused() {
    assert!(matches!(
        refusal("{\"a\":1} {\"b\":2}"),
        JsoncErrorKind::TrailingContent
    ));
}

// ---------------------------------------------------------------------------
// Refused — ordinary strictness
// ---------------------------------------------------------------------------

#[test]
fn malformed_documents_are_refused_with_the_reason_that_is_actually_true() {
    type Predicate = fn(&JsoncErrorKind) -> bool;
    let cases: Vec<(&str, Predicate)> = vec![
        ("", |k| matches!(k, JsoncErrorKind::Empty)),
        ("// only a comment", |k| matches!(k, JsoncErrorKind::Empty)),
        ("{", |k| matches!(k, JsoncErrorKind::UnexpectedEnd)),
        ("{\"a\"}", |k| matches!(k, JsoncErrorKind::Expected { .. })),
        ("{\"a\":}", |k| matches!(k, JsoncErrorKind::Expected { .. })),
        ("{'a':1}", |k| matches!(k, JsoncErrorKind::Expected { .. })),
        ("{\"a\":1", |k| matches!(k, JsoncErrorKind::UnexpectedEnd)),
        ("/* unterminated", |k| {
            matches!(k, JsoncErrorKind::UnterminatedComment)
        }),
        ("{\"a\": \"unterminated}", |k| {
            matches!(k, JsoncErrorKind::UnterminatedString)
        }),
        (r#"{"a":"\q"}"#, |k| {
            matches!(k, JsoncErrorKind::InvalidEscape { .. })
        }),
        (r#"{"a":"\uZZZZ"}"#, |k| {
            matches!(k, JsoncErrorKind::InvalidUnicodeEscape)
        }),
        (r#"{"a":"\uD83D"}"#, |k| {
            matches!(k, JsoncErrorKind::LoneSurrogate)
        }),
        (r#"{"a":"\uDE00"}"#, |k| {
            matches!(k, JsoncErrorKind::LoneSurrogate)
        }),
        ("{\"a\":\"raw\ttab\"}", |k| {
            matches!(k, JsoncErrorKind::UnescapedControl { .. })
        }),
        ("{\"a\":01}", |k| {
            matches!(k, JsoncErrorKind::InvalidNumber { .. })
        }),
        ("{\"a\":1.}", |k| {
            matches!(k, JsoncErrorKind::InvalidNumber { .. })
        }),
        ("{\"a\":1e}", |k| {
            matches!(k, JsoncErrorKind::InvalidNumber { .. })
        }),
        ("{\"a\":+1}", |k| {
            matches!(k, JsoncErrorKind::Expected { .. })
        }),
        ("{\"a\":1 / 2}", |k| {
            matches!(k, JsoncErrorKind::Expected { .. })
        }),
    ];
    for (body, predicate) in cases {
        let kind = refusal(body);
        assert!(
            predicate(&kind),
            "{body:?} was refused for the wrong reason: {kind:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Refused — the FOURTH departure: an isolated UTF-16 surrogate code unit
//
// OPERATIONAL NON-PARITY, disclosed rather than presented as strictness. The
// oracle accepts these. Rust `str` is UTF-8 and cannot represent an isolated
// surrogate at all, so the alternatives were a lossy U+FFFD substitution or a
// typed refusal. Substitution would let a truncated credential-bearing value
// decode into something that looks fine, so the refusal is deliberate.
// ---------------------------------------------------------------------------

/// Three arms reject a surrogate and each needs a witness that DRIVES it: an
/// unpaired HIGH surrogate, a high surrogate followed by something that is not
/// a low surrogate, and a bare LOW surrogate. One witness covering only the
/// first left the other two arms unfalsified, which the IGC battery caught by
/// scoring a mutant on the third arm GREEN.
#[test]
fn an_unpaired_high_surrogate_never_becomes_a_replacement_character() {
    assert!(matches!(
        refusal(r#"{"a":"\uD800x"}"#),
        JsoncErrorKind::LoneSurrogate
    ));
}

#[test]
fn a_high_surrogate_followed_by_a_non_low_surrogate_is_refused() {
    assert!(matches!(
        refusal(r#"{"a":"\uD800\u0041"}"#),
        JsoncErrorKind::LoneSurrogate
    ));
}

#[test]
fn a_bare_low_surrogate_never_becomes_a_replacement_character() {
    assert!(matches!(
        refusal(r#"{"a":"\uDE00"}"#),
        JsoncErrorKind::LoneSurrogate
    ));
}

#[test]
fn invalid_utf8_is_refused_at_the_byte_boundary() {
    let raw = vec![b'{', b'"', 0xFF, b'"', b':', b'1', b'}'];
    match jsonc::decode(&raw, bounds()) {
        Ok(_) => panic!("invalid UTF-8 must be refused"),
        Err(error) => assert!(matches!(error.kind(), JsoncErrorKind::NotUtf8)),
    }
}

// ---------------------------------------------------------------------------
// The structured path
// ---------------------------------------------------------------------------

#[test]
fn a_failure_names_the_path_it_happened_at_not_only_what_happened() {
    let body = r#"{"providers": {"anthropic": {"models": [ {"id": 01} ]}}}"#;
    match decode(body) {
        Ok(_) => panic!("expected a refusal"),
        Err(error) => {
            assert!(matches!(error.kind(), JsoncErrorKind::InvalidNumber { .. }));
            assert_eq!(
                error.path().render(),
                "$.#0.#0.#0[0].#0",
                "master requires a serde_path_to_error-class structured path; the segments are key ORDINALS because a key name is authored content that must not cross a public Display"
            );
            assert_eq!(&body[error.offset()..error.offset() + 2], "01");
        }
    }
}

#[test]
fn the_root_path_renders_as_a_bare_root() {
    match decode("[") {
        Ok(_) => panic!("expected a refusal"),
        Err(error) => assert_eq!(error.path().render(), "$"),
    }
}

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

#[test]
fn the_byte_bound_is_checked_before_decoding_and_names_both_numbers() {
    let bounds = custom_bounds(8, 4, 4, 8, 100);
    let body = "{\"aaaaaaaaaa\": 1}";
    match jsonc::decode(body.as_bytes(), bounds) {
        Ok(_) => panic!("expected a byte-bound refusal"),
        Err(error) => match error.kind() {
            JsoncErrorKind::DocumentTooLarge { actual, bound } => {
                assert_eq!((*actual, *bound), (body.len(), 8))
            }
            other => panic!("expected DocumentTooLarge, got {other:?}"),
        },
    }
}

/// The scoreable half of the depth law: a document that merely EXCEEDS the
/// bound is refused, and a mutant that removes the check turns this into a
/// successful decode that the assertion below catches.
///
/// It is separate from the hostile-depth witness on purpose. Deleting the check
/// makes a 10_000-deep document overflow the stack and abort the test process,
/// which a mutation harness scores as "no test ran" rather than as a red
/// witness — a law whose only witness dies that way is unwitnessed. So the
/// mutation row points HERE, and the hostile case below keeps the property that
/// actually matters to a reader.
#[test]
fn the_depth_bound_refuses_a_document_that_merely_exceeds_it() {
    let bounds = custom_bounds(64 * 1024, 4, 4, 8, 100_000);
    let body = format!("{}{}", "[".repeat(200), "]".repeat(200));
    match jsonc::decode(body.as_bytes(), bounds) {
        Ok(_) => panic!("expected a depth refusal"),
        Err(error) => match error.kind() {
            JsoncErrorKind::DepthExceeded { bound } => assert_eq!(*bound, 8),
            other => panic!("expected DepthExceeded, got {other:?}"),
        },
    }
}

/// Checked BEFORE descending, so a hostile document refuses instead of
/// exhausting the stack. `10_000` opening brackets would overflow a recursive
/// descent that checked afterwards; this witness passing at all is the evidence
/// that the check runs first.
#[test]
fn a_hostile_depth_refuses_before_the_stack_overflows() {
    let bounds = custom_bounds(64 * 1024, 4, 4, 8, 100_000);
    let body = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    match jsonc::decode(body.as_bytes(), bounds) {
        Ok(_) => panic!("expected a depth refusal"),
        Err(error) => match error.kind() {
            JsoncErrorKind::DepthExceeded { bound } => assert_eq!(*bound, 8),
            other => panic!("expected DepthExceeded, got {other:?}"),
        },
    }
}

/// Byte length does not bound node count: `[[[…]]]` is cheap in bytes and
/// expensive in nodes, which is why the two bounds are separate.
#[test]
fn the_node_bound_refuses_a_document_that_is_small_in_bytes_and_large_in_nodes() {
    let bounds = custom_bounds(64 * 1024, 4, 4, 64, 10);
    let body = format!("[{}]", vec!["1"; 200].join(","));
    match jsonc::decode(body.as_bytes(), bounds) {
        Ok(_) => panic!("expected a node refusal"),
        Err(error) => match error.kind() {
            JsoncErrorKind::NodeCountExceeded { bound } => assert_eq!(*bound, 10),
            other => panic!("expected NodeCountExceeded, got {other:?}"),
        },
    }
}

#[test]
fn a_bound_of_zero_and_a_bound_above_the_ceiling_are_both_refused_by_name() {
    use grapt_config::{BoundRefusal, MaxValueDepth};
    match MaxValueDepth::try_new(0) {
        Err(BoundRefusal::Zero { bound }) => assert_eq!(bound, "max value depth"),
        other => panic!("expected Zero, got {other:?}"),
    }
    match MaxValueDepth::try_new(MaxValueDepth::CEILING + 1) {
        Err(BoundRefusal::AboveCeiling { value, ceiling, .. }) => {
            assert_eq!(
                (value, ceiling),
                (MaxValueDepth::CEILING + 1, MaxValueDepth::CEILING)
            )
        }
        other => panic!("expected AboveCeiling, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Duplicate detection is COMPLETE and not quadratic
// ---------------------------------------------------------------------------

/// The review found the first implementation rescanned every prior entry for
/// each new key: 99,999 distinct short keys stays inside both the node ceiling
/// and the byte cap while performing 4,999,850,001 comparisons, a CPU denial
/// path inside the declared bounds.
///
/// The replacement is a `BTreeSet` — not a `HashSet`, whose `RandomState` is
/// ambient randomness this workspace forbids. This witness is structural rather
/// than timed: it proves the membership check covers the WHOLE object, which is
/// what a windowed "bounded" fix would break while still looking fast.
#[test]
fn a_duplicate_is_caught_however_far_it_sits_from_its_twin() {
    let mut body = String::from("{\"k0\":0");
    for i in 1..500 {
        body.push_str(&format!(",\"k{i}\":{i}"));
    }
    body.push_str(",\"k0\":1}");
    assert!(matches!(
        refusal(&body),
        JsoncErrorKind::DuplicateKey { .. }
    ));
}

/// A broad object decodes without quadratic work. With the rescan this is
/// 200,000,000 comparisons; with the set it is a walk. The assertion is on the
/// RESULT — every key present, in document order — so the witness fails for a
/// wrong answer rather than only for being slow.
#[test]
fn a_broad_object_decodes_and_keeps_every_key_in_document_order() {
    const KEYS: usize = 20_000;
    let mut body = String::from("{");
    for i in 0..KEYS {
        if i > 0 {
            body.push(',');
        }
        body.push_str(&format!("\"k{i}\":{i}"));
    }
    body.push('}');
    let bounds = custom_bounds(4 * 1024 * 1024, 4, 4, 8, 100_000);
    let value = match jsonc::decode(body.as_bytes(), bounds) {
        Ok(value) => value,
        Err(error) => panic!("a broad object must decode: {error}"),
    };
    let Some(entries) = value.value().as_object() else {
        panic!("root is {}", value.value().kind());
    };
    assert_eq!(entries.len(), KEYS);
    assert_eq!(entries[0].0.value().as_str(), "k0");
    assert_eq!(entries[KEYS - 1].0.value().as_str(), "k19999");
}

// ---------------------------------------------------------------------------
// The node bound is charged BEFORE the node is built
// ---------------------------------------------------------------------------

/// Order-sensitive, and that is the whole point. The review found the charge ran
/// AFTER each scalar and after a container's `Vec` was fully built, so the source
/// claim that a document cannot allocate past the limit and then be rejected was
/// false.
///
/// The offset is the evidence: charged first, the refusal points at the START of
/// the value that exceeded the bound; charged afterwards, the cursor has already
/// consumed that value and the offset lands past it.
#[test]
fn the_node_charge_lands_before_the_offending_value_is_consumed() {
    // root + three scalars = 4 nodes; a bound of 3 refuses on the third scalar.
    let body = "{\"a\":1,\"b\":2,\"c\":3}";
    let bounds = custom_bounds(1024, 4, 4, 8, 3);
    match jsonc::decode(body.as_bytes(), bounds) {
        Ok(_) => panic!("expected a node refusal"),
        Err(error) => {
            assert!(matches!(
                error.kind(),
                JsoncErrorKind::NodeCountExceeded { .. }
            ));
            assert_eq!(
                &body[error.offset()..error.offset() + 1],
                "3",
                "the refusal must point at the value that exceeded the bound, not past it"
            );
        }
    }
}

/// The container charge is also before construction: an object that is itself
/// the node over the limit refuses at its own opening brace.
#[test]
fn a_container_is_charged_at_its_opening_brace() {
    // root + one scalar = 2 nodes; a bound of 2 refuses on the inner object.
    let body = "{\"a\":1,\"b\":{}}";
    let bounds = custom_bounds(1024, 4, 4, 8, 2);
    match jsonc::decode(body.as_bytes(), bounds) {
        Ok(_) => panic!("expected a node refusal"),
        Err(error) => assert_eq!(&body[error.offset()..error.offset() + 1], "{"),
    }
}
