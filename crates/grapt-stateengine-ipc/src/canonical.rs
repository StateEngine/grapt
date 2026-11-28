// SPDX-License-Identifier: MIT OR Apache-2.0
//! Canonical JSON for the owner proof: object keys sorted by their UTF-8
//! bytes, no whitespace, and scalars written exactly as serde_json writes them.
use serde_json::Value;

pub(crate) fn canonical(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write(value, &mut out);
    out
}

fn write(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write(item, out);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
            out.push(b'{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                scalar(&Value::String(key.clone()), out);
                out.push(b':');
                write(item, out);
            }
            out.push(b'}');
        }
        scalar_value => scalar(scalar_value, out),
    }
}

fn scalar(value: &Value, out: &mut Vec<u8>) {
    // Writing a scalar into a Vec cannot fail.
    let _infallible = serde_json::to_writer(&mut *out, value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sort_at_every_level_without_whitespace() -> Result<(), serde_json::Error> {
        let value: Value = serde_json::from_str(r#"{ "b": [ {"z":1, "a":"é"} ], "a": null }"#)?;
        assert_eq!(
            canonical(&value),
            r#"{"a":null,"b":[{"a":"é","z":1}]}"#.as_bytes()
        );
        Ok(())
    }
}
