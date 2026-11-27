//! The canonical-bytes spec, rule by rule:
//!
//! 1. Object keys sort by UTF-16 code-unit order (NOT locale collation, NOT
//!    Unicode scalar order — surrogate pairs sort below `U+E000..` exactly as
//!    JavaScript string comparison does).
//! 2. `undefined` entries cannot exist post-parse (nothing to do here).
//! 3. Arrays preserve order.
//! 4. Numbers print with ECMAScript `Number::toString` semantics: integral
//!    doubles as plain integers, others as shortest round-trip decimal,
//!    exponential notation only when the decimal exponent is < -6 or >= 21.
//!    Empirically load-bearing only for the three `.cost` fields
//!    (e.g. `0.009880199999999999`).
//! 5. Hash = SHA-256 over the UTF-8 bytes of the canonical string.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;

/// Canonical string for a parsed JSON value.
pub fn canonical_string(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

/// Lowercase hex SHA-256 of the canonical UTF-8 bytes.
pub fn canonical_hash_hex(value: &Value) -> String {
    let canon = canonical_string(value);
    let digest = Sha256::digest(canon.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        // Infallible for String; ignore the Ok.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => write_number(out, n),
        Value::String(s) => write_json_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| utf16_code_unit_cmp(a, b));
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(out, key);
                out.push(':');
                // Key came from the map; the index is infallible.
                if let Some(v) = map.get(*key) {
                    write_value(out, v);
                }
            }
            out.push('}');
        }
    }
}

/// JavaScript string comparison: lexicographic over UTF-16 code units.
fn utf16_code_unit_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn write_json_string(out: &mut String, s: &str) {
    // serde_json's escaping matches JSON.stringify for well-formed strings:
    // ", \, and control chars escaped; non-ASCII passed through.
    match serde_json::to_string(s) {
        Ok(encoded) => out.push_str(&encoded),
        // A bare &str cannot fail JSON serialization; keep the canonical
        // stream well-formed if it ever did.
        Err(_) => out.push_str("\"\""),
    }
}

fn write_number(out: &mut String, n: &serde_json::Number) {
    if let Some(i) = n.as_i64() {
        out.push_str(&i.to_string());
    } else if let Some(u) = n.as_u64() {
        out.push_str(&u.to_string());
    } else if let Some(f) = n.as_f64() {
        out.push_str(&ecmascript_double(f));
    } else {
        // Unreachable with default serde_json features.
        out.push_str("null");
    }
}

/// ECMAScript `Number::toString(10)` for a finite double.
fn ecmascript_double(x: f64) -> String {
    if x == 0.0 {
        return "0".to_owned(); // covers -0.0: JSON.stringify(-0) === "0"
    }
    let negative = x < 0.0;
    let magnitude = x.abs();

    // Shortest round-trip digits via Rust's LowerExp (same shortest-digit
    // algorithm family as ryu): "d.ddddde<exp>".
    let exp_form = format!("{magnitude:e}");
    let (mantissa, exp_str) = match exp_form.split_once('e') {
        Some(parts) => parts,
        None => return exp_form, // unreachable for finite doubles
    };
    let exponent: i32 = exp_str.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let k = digits.len() as i32; // digit count
    let n = exponent + 1; // decimal point position, ES spec's `n`

    // ES NumberToString layout: fixed notation only when -6 < n <= 21.
    let body = if (1..=21).contains(&n) && n >= k {
        // Integer with possible trailing zeros: digits then (n-k) zeros.
        let mut s = digits.to_owned();
        s.extend(std::iter::repeat_n('0', (n - k) as usize));
        s
    } else if 0 < n && n < k {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if (-5..=0).contains(&n) {
        format!("0.{}{}", "0".repeat((-n) as usize), digits)
    } else {
        // Exponential notation: exponent < -6 or >= 21.
        let mant = if digits.len() == 1 {
            digits.to_owned()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        let e = n - 1;
        if e >= 0 {
            format!("{mant}e+{e}")
        } else {
            format!("{mant}e-{}", -e)
        }
    };

    if negative {
        format!("-{body}")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_keys_by_utf16_code_units_not_scalar_order() {
        // U+FF61 (BMP, code unit 0xFF61) vs U+10000 (surrogates 0xD800 0xDC00):
        // JS sorts the supplementary char FIRST; Unicode scalar order would
        // sort it last.
        let v = json!({ "\u{ff61}": 1, "\u{10000}": 2, "a": 3 });
        assert_eq!(
            canonical_string(&v),
            "{\"a\":3,\"\u{10000}\":2,\"\u{ff61}\":1}"
        );
    }

    #[test]
    fn numbers_match_ecmascript_tostring() {
        assert_eq!(
            ecmascript_double(0.009880199999999999),
            "0.009880199999999999"
        );
        assert_eq!(ecmascript_double(1.0), "1");
        assert_eq!(ecmascript_double(100.0), "100");
        assert_eq!(ecmascript_double(-0.0), "0");
        assert_eq!(ecmascript_double(1e21), "1e+21");
        assert_eq!(ecmascript_double(1e-7), "1e-7");
        assert_eq!(ecmascript_double(0.000001), "0.000001");
        assert_eq!(ecmascript_double(1234.5), "1234.5");
        assert_eq!(ecmascript_double(-2.5e-8), "-2.5e-8");
    }

    #[test]
    fn integers_pass_through_untouched() {
        let v = json!({ "seq": 0, "created": 1784090756115u64 });
        assert_eq!(
            canonical_string(&v),
            "{\"created\":1784090756115,\"seq\":0}"
        );
    }

    #[test]
    fn canonicalization_is_idempotent() {
        let v = json!({ "b": [1, {"z": null, "a": 0.1}], "a": "x\n\"y\"" });
        let once = canonical_string(&v);
        let reparsed: Value = match serde_json::from_str(&once) {
            Ok(v) => v,
            Err(e) => panic!("canonical output must reparse: {e}"),
        };
        assert_eq!(canonical_string(&reparsed), once);
    }

    #[test]
    fn hash_is_sha256_of_utf8_bytes() {
        // sha256("{}") — independently checkable constant.
        let v = json!({});
        assert_eq!(
            canonical_hash_hex(&v),
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }

    /// H3: a NON-fixed-point golden — a value whose canonical form differs
    /// from its natural serialization, exercising key sorting (including a
    /// supplementary-plane key), the load-bearing shortest-round-trip cost
    /// float, nesting, escapes and null. The canonical string is restated
    /// literally and the digest is frozen, so neither the ordering nor the
    /// number formatting can drift behind a relative assertion.
    #[test]
    fn a_nontrivial_golden_pins_ordering_numbers_and_escapes() {
        let v = json!({
            "seq": 3,
            "cost": 0.009880199999999999,
            "nested": { "z": null, "a": [1, "x\n"] },
            "\u{10000}": true
        });
        let expected_canonical = "{\"cost\":0.009880199999999999,\
             \"nested\":{\"a\":[1,\"x\\n\"],\"z\":null},\"seq\":3,\
             \"\u{10000}\":true}";
        assert_eq!(canonical_string(&v), expected_canonical);
        assert_eq!(
            canonical_hash_hex(&v),
            "80a9d8d7ada0e5d5d7394c4e00db3281394bdf11bc364bccb6d2d0a3a1db4705"
        );
    }
}
