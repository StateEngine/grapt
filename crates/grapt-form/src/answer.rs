//! Answer validation against a form's own field specification.
//!
//! A reply is refused with ONE message naming ONE field, exactly as the frozen
//! product does: the frozen `Form.InvalidAnswerError` carries no structured
//! field key, so the field name lives in the message or nowhere. Every message
//! this module returns therefore ends with `: <fieldKey>`, and the first
//! failure wins rather than a collected report — a caller fixes one field, not
//! a list.
//!
//! All three constraints are now reconciled with the oracle and each is
//! differential-tested against its own runtime: `format: "uri"` is an
//! engine-for-engine match, `pattern` runs on an ECMAScript engine bounded in
//! size AND evaluator work ([`PATTERN_BOUNDS`]), and `format: "date-time"` is
//! an observable-validity reconstruction ([`DATE_TIME_COMPATIBILITY`]).
//!
//! Everything here is measured against the oracle's JavaScript runtime 1.3.14, the runtime the frozen
//! product actually runs on, rather than argued from the ECMAScript
//! specification: the contract this projection owes is that product's
//! observable behaviour, not agreement between conforming engines.

use serde_json::Value;
use url::Url;

/// `format: "date-time"` compatibility: CLOSED against the measured oracle.
///
/// Was an open defect: this validator accepted only the ECMAScript Date Time
/// String Format and refused everything else, while the oracle also accepts a
/// legacy reading and does not calendar-validate. It now delegates to
/// [`crate::date`], an observable-VALIDITY reconstruction asserted against 111
/// measured cases from the oracle's JavaScript runtime 1.3.14 — ISO dates and their range edges, times,
/// offsets, legacy separators and orders, month names with casing and prefixes,
/// RFC-822 forms, timezone abbreviations, bare numbers and junk.
///
/// The specification argument I made first was wrong, and worth recording as
/// wrong: "the rest is implementation-defined" describes the SPECIFICATION, not
/// the contract this projection owes, which is one product's observable
/// behaviour.
pub const DATE_TIME_COMPATIBILITY: &str =
    "observable-validity reconstruction, differential-tested against a measured corpus";

/// Validate `answer` against `fields`, the form's own field specification.
///
/// `None` is a valid answer. `Some(message)` is the refusal, and the message is
/// the entire content of the frozen `FormInvalidAnswerError`.
pub fn validate_answer(fields: &Value, answer: &Value) -> Option<String> {
    let fields = fields.as_array()?;
    let answer = answer.as_object()?;

    // An unknown key is refused BEFORE any field is examined. A key nothing
    // declares cannot be validated by anything, so admitting it would let an
    // answer carry members the form never asked for.
    for key in answer.keys() {
        if !fields
            .iter()
            .any(|field| field_key(field) == Some(key.as_str()))
        {
            return Some(format!("Unknown form field: {key}"));
        }
    }

    for field in fields {
        let key = field_key(field)?;
        let kind = field.get("type").and_then(Value::as_str)?;
        let value = answer.get(key);

        // An external field is an acknowledgement, not an input: the only
        // admissible answer is `true`, and it has no `when` conditions.
        if kind == "external" {
            if value != Some(&Value::Bool(true)) {
                return Some(format!("External form field must be acknowledged: {key}"));
            }
            continue;
        }

        let active = is_active(field, answer);
        let Some(value) = value else {
            if required(field) && active {
                return Some(format!("Missing required form field: {key}"));
            }
            continue;
        };
        // An inactive field is UNANSWERABLE, not merely ignorable. Accepting a
        // value for a field the answer itself hid would let a caller satisfy a
        // condition and its hidden branch at once.
        if !active {
            return Some(format!("Form field is not active: {key}"));
        }
        if let Some(message) = validate_field(field, kind, key, value) {
            return Some(message);
        }
    }
    None
}

/// Validate a form's own field SPECIFICATION at creation time.
///
/// A `when` condition may only reference an EARLIER field, must carry a value
/// of that field's type, and must name a declared option when the referenced
/// field's options are closed. Refusing these at creation surfaces an authoring
/// mistake to the caller; admitting them mints a form with a field that can
/// never become active and therefore can never be answered.
pub fn validate_fields(fields: &Value) -> Option<String> {
    let Some(fields) = fields.as_array() else {
        return Some(String::from("Form fields must be an array"));
    };
    if fields.is_empty() {
        return Some(String::from("Form must have at least one field"));
    }
    let mut earlier: Vec<&Value> = Vec::new();
    for field in fields {
        let Some(key) = field_key(field) else {
            return Some(String::from("Form field key is required"));
        };
        if earlier.iter().any(|seen| field_key(seen) == Some(key)) {
            return Some(format!("Duplicate form field key: {key}"));
        }
        if field.get("type").and_then(Value::as_str) == Some("external") {
            earlier.push(field);
            continue;
        }
        for when in field
            .get("when")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let Some(reference) = when.get("key").and_then(Value::as_str) else {
                return Some(format!("Form field condition key is required: {key}"));
            };
            let Some(target) = earlier
                .iter()
                .find(|seen| field_key(seen) == Some(reference))
            else {
                return Some(format!(
                    "Form field condition must reference an earlier field: {key} -> {reference}"
                ));
            };
            if let Some(invalid) = validate_when(when, target) {
                return Some(format!("{invalid}: {key} -> {reference}"));
            }
        }
        earlier.push(field);
    }
    None
}

fn validate_when(when: &Value, target: &Value) -> Option<&'static str> {
    let value = when.get("value")?;
    match target.get("type").and_then(Value::as_str) {
        Some("boolean") => {
            (!value.is_boolean()).then_some("Form field condition value must be a boolean")
        }
        Some("number") | Some("integer") => {
            (!value.is_number()).then_some("Form field condition value must be a number")
        }
        // string and multiselect targets both compare against string values.
        Some(kind) => {
            if !value.is_string() {
                return Some("Form field condition value must be a string");
            }
            let options = target.get("options").and_then(Value::as_array);
            let closed = if kind == "multiselect" {
                !custom(target)
            } else {
                options.is_some() && !custom(target)
            };
            let declared = options.is_some_and(|options| {
                options
                    .iter()
                    .any(|option| option.get("value") == Some(value))
            });
            (closed && !declared)
                .then_some("Form field condition value must be one of the field's options")
        }
        None => Some("Form field condition references a field with no type"),
    }
}

fn field_key(field: &Value) -> Option<&str> {
    field.get("key").and_then(Value::as_str)
}

fn required(field: &Value) -> bool {
    field.get("required").and_then(Value::as_bool) == Some(true)
}

fn custom(field: &Value) -> bool {
    field.get("custom").and_then(Value::as_bool) == Some(true)
}

/// A field with no `when` is always active. Otherwise EVERY condition must
/// match, and an unanswered referenced field makes its condition false — so
/// hiding a field cascades to every condition that names it.
fn is_active(field: &Value, answer: &serde_json::Map<String, Value>) -> bool {
    let Some(conditions) = field.get("when").and_then(Value::as_array) else {
        return true;
    };
    conditions.iter().all(|when| {
        let Some(key) = when.get("key").and_then(Value::as_str) else {
            return false;
        };
        matches_condition(when, answer.get(key))
    })
}

fn matches_condition(when: &Value, value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let Some(expected) = when.get("value") else {
        return false;
    };
    let hit = match value.as_array() {
        Some(items) => items.iter().any(|item| item == expected),
        None => value == expected,
    };
    match when.get("op").and_then(Value::as_str) {
        Some("eq") => hit,
        Some("neq") => !hit,
        _ => false,
    }
}

fn validate_field(field: &Value, kind: &str, key: &str, value: &Value) -> Option<String> {
    match kind {
        "string" => validate_string(field, key, value),
        "number" | "integer" => validate_number(field, kind, key, value),
        "boolean" => {
            if value.is_boolean() {
                None
            } else {
                Some(format!("Expected boolean for form field: {key}"))
            }
        }
        "multiselect" => validate_multiselect(field, key, value),
        _ => Some(format!("Unknown form field type: {key}")),
    }
}

fn validate_string(field: &Value, key: &str, value: &Value) -> Option<String> {
    let Some(text) = value.as_str() else {
        return Some(format!("Expected string for form field: {key}"));
    };
    // The frozen check counts UTF-16 code units, which is what a browser client
    // and the oracle both count; counting bytes or scalars here would refuse a
    // non-ASCII answer the product accepts.
    let length = text.encode_utf16().count() as u64;
    if required(field) && text.is_empty() {
        return Some(format!("Missing required form field: {key}"));
    }
    if let Some(minimum) = field.get("minLength").and_then(Value::as_u64) {
        if length < minimum {
            return Some(format!("Form field is too short: {key}"));
        }
    }
    if let Some(maximum) = field.get("maxLength").and_then(Value::as_u64) {
        if length > maximum {
            return Some(format!("Form field is too long: {key}"));
        }
    }
    // The pattern is checked BEFORE format, as the frozen validator does: a
    // value failing both is refused for its pattern, not its format.
    if let Some(pattern) = field.get("pattern").and_then(Value::as_str) {
        match js_pattern_matches(pattern, text) {
            PatternOutcome::Matched => {}
            PatternOutcome::NotMatched => {
                return Some(format!("Form field does not match pattern: {key}"))
            }
            // The oracle reaches this exactly when `new RegExp` THROWS.
            PatternOutcome::Uncompilable => {
                return Some(format!("Form field has invalid pattern: {key}"))
            }
            // NOT a verdict on the value. The pattern is beyond the bound this
            // projection will evaluate, so it says so rather than guessing a
            // match or a refusal — see PATTERN_BOUNDS.
            PatternOutcome::TooLarge => {
                return Some(format!(
                    "Form field pattern is too large to evaluate: {key}"
                ))
            }
            // Also NOT a verdict on the value: the pattern was admissible but
            // evaluating it against this answer exceeded the work budget. Saying
            // "does not match" here would blame the answer for the pattern.
            PatternOutcome::Exhausted => {
                return Some(format!(
                    "Form field pattern could not be evaluated within its work budget: {key}"
                ))
            }
        }
    }
    match field.get("format").and_then(Value::as_str) {
        Some("email") if !is_email(text) => {
            return Some(format!("Expected email for form field: {key}"))
        }
        Some("uri") if !is_uri(text) => return Some(format!("Expected URI for form field: {key}")),
        Some("date") if !is_date(text) => {
            return Some(format!("Expected date for form field: {key}"))
        }
        Some("date-time") if !is_date_time(text) => {
            return Some(format!("Expected date-time for form field: {key}"))
        }
        _ => {}
    }
    if let Some(options) = field.get("options").and_then(Value::as_array) {
        if !custom(field)
            && !options
                .iter()
                .any(|option| option.get("value") == Some(value))
        {
            return Some(format!("Invalid option for form field: {key}"));
        }
    }
    None
}

fn validate_number(field: &Value, kind: &str, key: &str, value: &Value) -> Option<String> {
    let Some(number) = value.as_f64().filter(|number| number.is_finite()) else {
        return Some(format!("Expected number for form field: {key}"));
    };
    if kind == "integer" && number.fract() != 0.0 {
        return Some(format!("Expected integer for form field: {key}"));
    }
    if let Some(minimum) = field.get("minimum").and_then(Value::as_f64) {
        if number < minimum {
            return Some(format!("Form field is too small: {key}"));
        }
    }
    if let Some(maximum) = field.get("maximum").and_then(Value::as_f64) {
        if number > maximum {
            return Some(format!("Form field is too large: {key}"));
        }
    }
    None
}

fn validate_multiselect(field: &Value, key: &str, value: &Value) -> Option<String> {
    let Some(items) = value.as_array() else {
        return Some(format!("Expected string array for form field: {key}"));
    };
    if !items.iter().all(Value::is_string) {
        return Some(format!("Expected string array for form field: {key}"));
    }
    if required(field) && items.is_empty() {
        return Some(format!("Missing required form field: {key}"));
    }
    let count = items.len() as u64;
    if let Some(minimum) = field.get("minItems").and_then(Value::as_u64) {
        if count < minimum {
            return Some(format!("Too few selections for form field: {key}"));
        }
    }
    if let Some(maximum) = field.get("maxItems").and_then(Value::as_u64) {
        if count > maximum {
            return Some(format!("Too many selections for form field: {key}"));
        }
    }
    if !custom(field) {
        let options = field.get("options").and_then(Value::as_array);
        let admitted = |item: &Value| {
            options.is_some_and(|options| {
                options
                    .iter()
                    .any(|option| option.get("value") == Some(item))
            })
        };
        if !items.iter().all(admitted) {
            return Some(format!("Invalid option for form field: {key}"));
        }
    }
    None
}

/// The frozen shape test is `^[^\s@]+@[^\s@]+\.[^\s@]+$` — a shape, not
/// deliverability. Ported by hand because this crate carries no regex engine.
fn is_email(value: &str) -> bool {
    let forbidden = |c: char| c.is_whitespace() || c == '@';
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    if local.is_empty() || local.contains(forbidden) || domain.contains('@') {
        return false;
    }
    let Some((host, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    !host.is_empty()
        && !tld.is_empty()
        && !host.contains(|c: char| c.is_whitespace())
        && !tld.contains(|c: char| c.is_whitespace())
}

/// The bounds this projection places on `pattern` evaluation.
///
/// SIZE is bounded by `MAX_PATTERN_BYTES`. EXECUTION is bounded by a
/// deterministic work budget of `PATTERN_WORK_BUDGET` units, charged by the
/// evaluator as it performs work — per instruction dispatched, per backtrack
/// step, per character scanned, per scan restart, and through lookaround
/// recursion, which re-enters the same charged loop.
///
/// A budget rather than a timeout, because a timeout is not a bound here: a
/// runaway match cannot be stopped from outside the engine, so a timer observes
/// an overrun without ending it. Charging work also makes exhaustion
/// DETERMINISTIC — the same pattern and input spend the same units on every run
/// and every machine, so an exhausted verdict is reproducible rather than a
/// function of load.
///
/// Exhaustion is never a match and never a non-match. It leaves by its own path.
pub const PATTERN_BOUNDS: &str =
    "pattern length AND evaluator work are both bounded; exhaustion is its own result";

/// The largest `pattern` this validator will compile.
const MAX_PATTERN_BYTES: usize = 4096;

/// The evaluator work units one `pattern` evaluation may spend.
///
/// Calibrated against measured costs rather than picked: an ordinary anchored
/// pattern settles in tens of units, a backreference against sixty characters in
/// about 1,400, and a non-matching literal scan across 100,000 characters in
/// about 300,000. The catastrophic shapes are the ones this excludes —
/// `^(a+)+$` against eighteen characters costs 3.2 MILLION units and against
/// twenty-two exceeds fifty million.
const PATTERN_WORK_BUDGET: u64 = 1_000_000;

/// What evaluating a `pattern` against a value produced.
#[derive(Debug, PartialEq, Eq)]
enum PatternOutcome {
    Matched,
    NotMatched,
    /// `new RegExp(pattern)` would throw.
    Uncompilable,
    /// Beyond [`MAX_PATTERN_BYTES`]; deliberately NOT a verdict.
    TooLarge,
    /// The evaluator spent [`PATTERN_WORK_BUDGET`] without reaching a verdict.
    /// Deliberately NOT a verdict either.
    Exhausted,
}

/// Evaluate `pattern` against `text` the way `new RegExp(pattern).test(text)`
/// does, with NO flags, within a bounded work budget.
///
/// Both halves of the UTF-16 model matter and each was measured against the oracle's JavaScript runtime
/// 1.3.14. The PATTERN is parsed as UTF-16 code units, so an astral literal in
/// the pattern is a surrogate PAIR exactly as JavaScript reads it; parsing it as
/// scalars makes it one unit, which code-unit input can then never match. The
/// INPUT is matched as UCS-2, so `.` does not match an astral character, `[^]`
/// matches one code unit, and `^.{2}$` matches a single emoji — all of which the
/// oracle does and a scalar-value engine does not.
fn js_pattern_matches(pattern: &str, text: &str) -> PatternOutcome {
    if pattern.len() > MAX_PATTERN_BYTES {
        return PatternOutcome::TooLarge;
    }
    let compiled = regress::Regex::from_unicode(
        pattern.encode_utf16().map(u32::from),
        regress::Flags::default(),
    );
    let Ok(expression) = compiled else {
        return PatternOutcome::Uncompilable;
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    match expression.find_from_ucs2_budgeted(units.as_slice(), 0, PATTERN_WORK_BUDGET) {
        Ok(Some(_)) => PatternOutcome::Matched,
        Ok(None) => PatternOutcome::NotMatched,
        Err(_) => PatternOutcome::Exhausted,
    }
}

/// The frozen check is `new URL(value)`, which is a WHATWG URL parse and NOT a
/// shape test: it requires an absolute URL with a scheme and rejects a relative
/// one. The `url` crate implements the same standard, so this is the one
/// constraint here that is an engine-for-engine match rather than a port.
fn is_uri(value: &str) -> bool {
    Url::parse(value).is_ok()
}

/// Whether the oracle accepts this as a `date-time`.
///
/// Delegates to [`crate::date`], which reconstructs the OBSERVABLE validity of
/// `new Date(value)` — including the legacy non-ISO readings — from a measured
/// corpus against the frozen product's own runtime. An ISO-only check was the
/// previous behaviour and was a compatibility defect: it refused answers the
/// product accepts.
fn is_date_time(value: &str) -> bool {
    crate::date::is_valid(value)
}

/// `YYYY-MM-DD` that names a REAL day. The frozen check round-trips the value
/// through a UTC date and compares, so 2026-02-30 is refused even though it
/// matches the shape.
fn is_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        return false;
    }
    let (Ok(year), Ok(month), Ok(day)) = (
        value[0..4].parse::<u32>(),
        value[5..7].parse::<u32>(),
        value[8..10].parse::<u32>(),
    ) else {
        return false;
    };
    if !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    day <= days_in_month(year, month)
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        _ => 28,
    }
}

#[cfg(test)]
#[path = "answer_tests.rs"]
mod tests;
