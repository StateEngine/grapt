//! Witnesses for the answer validator.
//!
//! Split out of `answer.rs` so neither file crosses the workspace's per-module
//! ceiling. These exercise the parent module's own items.

use super::*;
use serde_json::json;

fn fields() -> Value {
    json!([
        {"key": "name", "type": "string", "required": true, "minLength": 2, "maxLength": 4},
        {"key": "count", "type": "integer", "minimum": 1, "maximum": 3},
        {"key": "agree", "type": "boolean"},
    ])
}

#[test]
fn a_valid_answer_is_admitted() {
    let answer = json!({"name": "abc", "count": 2, "agree": true});
    assert_eq!(validate_answer(&fields(), &answer), None);
}

/// The message is the only carrier of the rejected field, so every refusal
/// this module can produce has to end with the field's own key.
#[test]
fn every_refusal_names_the_field_it_rejected() {
    let cases = [
        (json!({"nope": "x"}), "nope"),
        (json!({"count": 2, "agree": true}), "name"),
        (json!({"name": "a", "count": 2, "agree": true}), "name"),
        (json!({"name": "abcde", "count": 2, "agree": true}), "name"),
        (json!({"name": 7, "count": 2, "agree": true}), "name"),
        (json!({"name": "abc", "count": 9, "agree": true}), "count"),
        (json!({"name": "abc", "count": 0, "agree": true}), "count"),
        (json!({"name": "abc", "count": 1.5, "agree": true}), "count"),
        (json!({"name": "abc", "count": 2, "agree": "yes"}), "agree"),
    ];
    for (answer, key) in cases {
        let message = match validate_answer(&fields(), &answer) {
            Some(message) => message,
            None => panic!("{answer} was admitted but must be refused"),
        };
        assert!(
            message.ends_with(&format!(": {key}")),
            "{message:?} does not name field {key}"
        );
    }
}

/// A hidden field is unanswerable, and hiding it also falsifies every
/// condition that references it — the cascade the frozen validator has.
#[test]
fn an_inactive_field_is_refused_rather_than_ignored() {
    let fields = json!([
        {"key": "kind", "type": "string", "options": [{"value": "a", "label": "A"}, {"value": "b", "label": "B"}]},
        {"key": "detail", "type": "string", "when": [{"key": "kind", "op": "eq", "value": "a"}]},
    ]);
    assert_eq!(
        validate_answer(&fields, &json!({"kind": "a", "detail": "x"})),
        None
    );
    let refused = validate_answer(&fields, &json!({"kind": "b", "detail": "x"}));
    assert_eq!(
        refused,
        Some(String::from("Form field is not active: detail"))
    );
    // `kind` unanswered makes the condition false, so `detail` is inactive
    // even though nothing contradicted it.
    assert_eq!(
        validate_answer(&fields, &json!({"detail": "x"})),
        Some(String::from("Form field is not active: detail"))
    );
}

#[test]
fn a_required_but_inactive_field_is_not_demanded() {
    let fields = json!([
        {"key": "kind", "type": "boolean"},
        {"key": "detail", "type": "string", "required": true,
         "when": [{"key": "kind", "op": "eq", "value": true}]},
    ]);
    assert_eq!(validate_answer(&fields, &json!({"kind": false})), None);
    assert_eq!(
        validate_answer(&fields, &json!({"kind": true})),
        Some(String::from("Missing required form field: detail"))
    );
}

#[test]
fn an_external_field_admits_only_an_acknowledgement() {
    let fields = json!([{"key": "tos", "type": "external", "url": "https://example.test/tos"}]);
    assert_eq!(validate_answer(&fields, &json!({"tos": true})), None);
    for refused in [json!({}), json!({"tos": false}), json!({"tos": "yes"})] {
        assert_eq!(
            validate_answer(&fields, &refused),
            Some(String::from(
                "External form field must be acknowledged: tos"
            ))
        );
    }
}

#[test]
fn a_closed_option_set_refuses_an_undeclared_choice_unless_custom() {
    let closed = json!([{"key": "pick", "type": "string",
        "options": [{"value": "a", "label": "A"}]}]);
    assert_eq!(validate_answer(&closed, &json!({"pick": "a"})), None);
    assert_eq!(
        validate_answer(&closed, &json!({"pick": "z"})),
        Some(String::from("Invalid option for form field: pick"))
    );
    let open = json!([{"key": "pick", "type": "string", "custom": true,
        "options": [{"value": "a", "label": "A"}]}]);
    assert_eq!(validate_answer(&open, &json!({"pick": "z"})), None);
}

#[test]
fn a_multiselect_bounds_its_items_and_its_options() {
    let fields = json!([{"key": "tags", "type": "multiselect", "minItems": 1, "maxItems": 2,
        "options": [{"value": "a", "label": "A"}, {"value": "b", "label": "B"}]}]);
    assert_eq!(validate_answer(&fields, &json!({"tags": ["a"]})), None);
    assert_eq!(
        validate_answer(&fields, &json!({"tags": []})),
        Some(String::from("Too few selections for form field: tags"))
    );
    assert_eq!(
        validate_answer(&fields, &json!({"tags": ["a", "b", "a"]})),
        Some(String::from("Too many selections for form field: tags"))
    );
    assert_eq!(
        validate_answer(&fields, &json!({"tags": ["z"]})),
        Some(String::from("Invalid option for form field: tags"))
    );
    assert_eq!(
        validate_answer(&fields, &json!({"tags": "a"})),
        Some(String::from("Expected string array for form field: tags"))
    );
}

#[test]
fn an_email_and_a_date_are_checked_by_shape_and_by_calendar() {
    let fields = json!([
        {"key": "mail", "type": "string", "format": "email"},
        {"key": "when", "type": "string", "format": "date"},
    ]);
    assert_eq!(
        validate_answer(&fields, &json!({"mail": "a@b.co", "when": "2026-02-28"})),
        None
    );
    assert_eq!(
        validate_answer(&fields, &json!({"mail": "a@b.co", "when": "2024-02-29"})),
        None,
        "a leap day in a leap year is a real date"
    );
    for (bad, key) in [
        (json!({"mail": "a b@c.co", "when": "2026-01-01"}), "mail"),
        (json!({"mail": "ab.co", "when": "2026-01-01"}), "mail"),
        (json!({"mail": "a@bco", "when": "2026-01-01"}), "mail"),
        (json!({"mail": "a@b.co", "when": "2026-02-30"}), "when"),
        (json!({"mail": "a@b.co", "when": "2026-13-01"}), "when"),
        (json!({"mail": "a@b.co", "when": "2025-02-29"}), "when"),
        (json!({"mail": "a@b.co", "when": "20260101"}), "when"),
    ] {
        let message = match validate_answer(&fields, &bad) {
            Some(message) => message,
            None => panic!("{bad} was admitted but must be refused"),
        };
        assert!(
            message.ends_with(&format!(": {key}")),
            "{message:?} does not name field {key}"
        );
    }
}

/// A string length is counted the way the frozen validator counts it, so a
/// non-ASCII answer inside the bound is not refused for being long.
#[test]
fn a_length_bound_counts_the_same_units_the_frozen_check_counts() {
    let fields = json!([{"key": "text", "type": "string", "maxLength": 3}]);
    assert_eq!(validate_answer(&fields, &json!({"text": "éàü"})), None);
    assert_eq!(
        validate_answer(&fields, &json!({"text": "éàüz"})),
        Some(String::from("Form field is too long: text"))
    );
}

/// A `when` may only look BACKWARD. A forward reference would mint a field
/// whose condition can never be satisfied, so the form could never be
/// answered — an authoring mistake the caller must be told about at create
/// time rather than discovering at reply time.
#[test]
fn a_condition_may_only_reference_an_earlier_field() {
    let backward = json!([
        {"key": "kind", "type": "boolean"},
        {"key": "detail", "type": "string", "when": [{"key": "kind", "op": "eq", "value": true}]},
    ]);
    assert_eq!(validate_fields(&backward), None);

    let forward = json!([
        {"key": "detail", "type": "string", "when": [{"key": "kind", "op": "eq", "value": true}]},
        {"key": "kind", "type": "boolean"},
    ]);
    assert_eq!(
        validate_fields(&forward),
        Some(String::from(
            "Form field condition must reference an earlier field: detail -> kind"
        ))
    );

    let itself = json!([
        {"key": "loop", "type": "boolean", "when": [{"key": "loop", "op": "eq", "value": true}]},
    ]);
    assert!(
        validate_fields(&itself).is_some(),
        "a self-reference is not an earlier field"
    );
}

#[test]
fn a_condition_value_must_fit_the_field_it_references() {
    let wrong_type = json!([
        {"key": "kind", "type": "boolean"},
        {"key": "detail", "type": "string", "when": [{"key": "kind", "op": "eq", "value": "yes"}]},
    ]);
    assert_eq!(
        validate_fields(&wrong_type),
        Some(String::from(
            "Form field condition value must be a boolean: detail -> kind"
        ))
    );

    let closed = json!([
        {"key": "kind", "type": "string", "options": [{"value": "a", "label": "A"}]},
        {"key": "detail", "type": "string", "when": [{"key": "kind", "op": "eq", "value": "z"}]},
    ]);
    assert_eq!(
        validate_fields(&closed),
        Some(String::from(
            "Form field condition value must be one of the field's options: detail -> kind"
        ))
    );

    let open = json!([
        {"key": "kind", "type": "string", "custom": true, "options": [{"value": "a", "label": "A"}]},
        {"key": "detail", "type": "string", "when": [{"key": "kind", "op": "eq", "value": "z"}]},
    ]);
    assert_eq!(validate_fields(&open), None);
}

#[test]
fn a_specification_refuses_an_empty_set_and_a_duplicate_key() {
    assert_eq!(
        validate_fields(&json!([])),
        Some(String::from("Form must have at least one field"))
    );
    assert_eq!(
        validate_fields(&json!([
            {"key": "a", "type": "boolean"},
            {"key": "a", "type": "string"},
        ])),
        Some(String::from("Duplicate form field key: a"))
    );
}

/// `format: "uri"` is a WHATWG URL parse, not a shape test, so a relative
/// reference is refused where a schemed absolute URL is admitted.
#[test]
fn a_uri_is_parsed_rather_than_shape_matched() {
    let fields = json!([{"key": "link", "type": "string", "format": "uri"}]);
    for good in [
        "https://example.test/x?y=1#z",
        "mailto:someone@example.test",
        "file:///tmp/x",
        "urn:isbn:0451450523",
    ] {
        assert_eq!(
            validate_answer(&fields, &json!({"link": good})),
            None,
            "{good} is an absolute URL and must be admitted"
        );
    }
    for bad in ["/just/a/path", "example.test", "", "http://[::1"] {
        assert_eq!(
            validate_answer(&fields, &json!({"link": bad})),
            Some(String::from("Expected URI for form field: link")),
            "{bad} is not an absolute URL and must be refused"
        );
    }
}

#[test]
fn a_date_time_accepts_the_mandated_format_and_its_optional_parts() {
    let fields = json!([{"key": "at", "type": "string", "format": "date-time"}]);
    for good in [
        "2026",
        "2026-09",
        "2026-09-06",
        "2026-09-06T23:59",
        "2026-09-06T23:59:59",
        "2026-09-06T23:59:59.123",
        "2026-09-06T23:59:59Z",
        "2026-09-06T23:59:59.123Z",
        "2026-09-06T23:59:59+05:30",
        "2026-09-06T23:59:59.5-08:00",
        "2024-02-29",
        // The oracle does NOT calendar-validate `date-time`: it rolls these
        // over and accepts them, so refusing them was a compatibility bug.
        "2026-02-30",
        "2025-02-29",
        "2026-09-31",
        // The LEGACY readings, which an ISO-only check wrongly refused.
        "2026-9-6",
        "2026-09-06 23:59",
        "2026/09/06",
        "March 5, 2026",
        "Mar 5 2026",
        "Thu, 01 Jan 2026 00:00:00 GMT",
        "Thu Jan 01 2026",
        "+002026-09-06",
        "Jan 1 2026 PST",
    ] {
        assert_eq!(
            validate_answer(&fields, &json!({"at": good})),
            None,
            "{good} is in the mandated format and must be admitted"
        );
    }
    for bad in [
        // Refused by the oracle too, and re-verified against it: month 13,
        // day 00, an hour past end-of-day, a minute out of range, a bare
        // `T`, a compact basic-format date, and trailing junk.
        "2026-13-01",
        "2026-09-00",
        "2026-09-06T24:00:01",
        "2026-09-06T23:60",
        "2026-09-06T",
        "20260906",
        "not a date",
        "",
    ] {
        assert_eq!(
            validate_answer(&fields, &json!({"at": bad})),
            Some(String::from("Expected date-time for form field: at")),
            "{bad} is refused by the oracle and must be refused here"
        );
    }
}

/// A pattern that compiles decides the answer; a pattern that does not
/// compile is a DIFFERENT refusal, because the two say different things
/// about whose mistake it was.
#[test]
fn a_pattern_decides_the_answer_and_an_uncompilable_one_is_its_own_refusal() {
    let fields = json!([{"key": "code", "type": "string", "pattern": "^[0-9]+$"}]);
    assert_eq!(validate_answer(&fields, &json!({"code": "12345"})), None);
    assert_eq!(
        validate_answer(&fields, &json!({"code": "not-a-number"})),
        Some(String::from("Form field does not match pattern: code"))
    );
    let broken = json!([{"key": "code", "type": "string", "pattern": "^[0-9"}]);
    assert_eq!(
        validate_answer(&broken, &json!({"code": "1"})),
        Some(String::from("Form field has invalid pattern: code"))
    );
}

/// DIFFERENTIAL: every row is a MEASURED `new RegExp(p).test(v)` result from
/// The oracle's JavaScript runtime 1.3.14, asserted against this engine.
///
/// These are the cases a general Rust regex engine gets WRONG, which is why
/// compile-only comparison was not evidence: without the `u` flag JavaScript
/// is not a Unicode-scalar engine. `\\p{L}` is an identity escape matching the
/// literal "p{L}", `\\w` and `\\d` are ASCII-only, `[]` never matches while
/// `[^]` matches any one code unit, and every astral character is a
/// surrogate PAIR.
#[test]
fn pattern_matching_reproduces_the_oracle_including_utf16_semantics() {
    // (pattern, value, oracle said MATCH?)
    let measured: &[(&str, &str, bool)] = &[
        (r"^\p{L}+$", "p{L}", true),
        (r"^\p{L}+$", "abc", false),
        (r"^\p{L}+$", "é", false),
        (r"\p{L}", "p{L}", true),
        (r"\p{L}", "a", false),
        (r"^\w+$", "abc_1", true),
        (r"^\w+$", "é", false),
        (r"^\d$", "5", true),
        (r"^\d$", "٥", false),
        ("^[]$", "", false),
        ("^[]$", "a", false),
        ("^[^]$", "a", true),
        ("^[^]$", "", false),
        ("^.$", "a", true),
        ("^.$", "\n", false),
        // Astral: JavaScript counts UTF-16 code units, so one emoji is TWO.
        ("^.$", "😀", false),
        ("^.{1}$", "😀", false),
        ("^.{2}$", "😀", true),
        ("^😀$", "😀", true),
        ("^[^]$", "😀", false),
        ("^a$", "a", true),
        ("^a$", "a\n", false),
        (r"^(\w)\1$", "aa", true),
        (r"^(\w)\1$", "ab", false),
        (r"^(?<n>a)\k<n>$", "aa", true),
        (r"^(?<n>a)\k<n>$", "ab", false),
        (r"\k<n>", "k<n>", true),
        (r"\k<n>", "kn", false),
        ("^(?=.*[0-9]).+$", "a1", true),
        ("^(?=.*[0-9]).+$", "ab", false),
        (r"^\u0041$", "A", true),
        (r"^\S+@\S+$", "a@b", true),
        (r"^\S+@\S+$", "a b@c", false),
    ];
    for (pattern, value, oracle_matched) in measured {
        let outcome = js_pattern_matches(pattern, value);
        let expected = if *oracle_matched {
            PatternOutcome::Matched
        } else {
            PatternOutcome::NotMatched
        };
        assert_eq!(
            outcome, expected,
            "pattern {pattern:?} against {value:?}: the oracle said matched={oracle_matched}"
        );
    }
}

/// The oracle THROWS on each of these, so this engine must too — otherwise a
/// form the product refuses to evaluate gets evaluated here, which is the
/// reverse-direction defect the previous engine had.
#[test]
fn an_invalid_pattern_is_uncompilable_exactly_where_the_oracle_throws() {
    for throws_in_oracle in ["^[0-9", "(", "a{2,1}", "[z-a]", r"\", "(?i)abc"] {
        assert_eq!(
            js_pattern_matches(throws_in_oracle, "anything"),
            PatternOutcome::Uncompilable,
            "{throws_in_oracle:?} throws in the oracle"
        );
    }
    // ...and a valid one is still evaluated rather than refused.
    assert_eq!(
        js_pattern_matches("^[0-9]+$", "123"),
        PatternOutcome::Matched
    );
}

/// The SIZE bound is real and reports itself truthfully: a pattern past it
/// is neither a match nor a refusal of the value.
/// A catastrophic pattern is BOUNDED at the validator, not just in the
/// engine, and the refusal blames the pattern rather than the answer.
///
/// `^(a+)+$` against twenty-two characters exceeds fifty million work units
/// upstream; here it stops on the budget and says so. Reporting "does not
/// match" would tell the user their answer was wrong when the truth is we
/// never finished checking it.
#[test]
fn a_catastrophic_pattern_is_bounded_and_blames_the_pattern() {
    let fields = json!([{"key": "code", "type": "string", "pattern": "^(a+)+$"}]);
    let hostile = "a".repeat(22) + "!";
    assert_eq!(
        validate_answer(&fields, &json!({"code": hostile})),
        Some(String::from(
            "Form field pattern could not be evaluated within its work budget: code"
        ))
    );
    // The same pattern on an answer it can settle is still decided normally.
    assert_eq!(validate_answer(&fields, &json!({"code": "aaaa"})), None);
}

#[test]
fn an_oversized_pattern_reports_exhaustion_rather_than_a_verdict() {
    let huge = "a".repeat(MAX_PATTERN_BYTES + 1);
    assert_eq!(js_pattern_matches(&huge, "a"), PatternOutcome::TooLarge);
    let fields = json!([{"key": "code", "type": "string", "pattern": huge}]);
    assert_eq!(
        validate_answer(&fields, &json!({"code": "a"})),
        Some(String::from(
            "Form field pattern is too large to evaluate: code"
        ))
    );
    assert!(PATTERN_BOUNDS.contains("both bounded"));
}

/// The date-time defect is CLOSED. The exhaustive differential lives in
/// `crate::date` and asserts 111 measured cases; this pins the handful that
/// were the defect, so a regression to an ISO-only check fails here too.
#[test]
fn the_legacy_readings_the_oracle_accepts_are_accepted_here() {
    assert!(DATE_TIME_COMPATIBILITY.contains("measured corpus"));
    let fields = json!([{"key": "at", "type": "string", "format": "date-time"}]);
    for previously_refused in [
        "2026-9-6",
        "2026-09-06 23:59",
        "2026/09/06",
        "March 5, 2026",
        "Marc 5, 2026",
        "2026-02-30",
    ] {
        assert_eq!(
            validate_answer(&fields, &json!({"at": previously_refused})),
            None,
            "{previously_refused} is accepted by the oracle"
        );
    }
}
