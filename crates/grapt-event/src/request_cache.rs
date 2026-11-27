//! A bounded hash-only snapshot of one staged request. Usage stays in its own facts.
use grapt_types::request_cache::{
    RequestCacheFingerprint, RequestComponents, RequestHash, MAX_PREFIX_ITEMS,
};
use grapt_types::{PhysicalAttemptId, StepNumber};
use serde_json::{json, Value};

pub const KIND: &str = "session.request.cache";
pub const MAX_RECORD_BYTES: usize = 36 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestCacheFact {
    pub attempt: PhysicalAttemptId,
    pub step: StepNumber,
    pub fingerprint: Option<RequestCacheFingerprint>,
}

pub fn hash_hex(hash: &RequestHash) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(64);
    for byte in hash {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    text
}
fn parse_hash(value: &Value) -> Result<RequestHash, String> {
    let text = value
        .as_str()
        .filter(|text| text.len() == 64)
        .ok_or("invalid request hash")?;
    let mut hash = [0; 32];
    let nibble = |byte| match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err("invalid request hash".to_owned()),
    };
    for (out, pair) in hash.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        *out = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(hash)
}
pub fn encode(fact: &RequestCacheFact) -> Value {
    let fingerprint = fact.fingerprint.as_ref().map(|fp| {
        let c = fp.components();
        json!({"target":hash_hex(&c.target),"cacheKey":hash_hex(&c.cache_key),
            "routing":hash_hex(&c.routing),"tools":hash_hex(&c.tools),
            "instructions":hash_hex(&c.instructions),"options":hash_hex(&c.options),
            "input":hash_hex(&c.input),"inputItems":fp.input_items(),
            "prefixes":fp.prefixes().iter().map(hash_hex).collect::<Vec<_>>()})
    });
    json!({"attempt":fact.attempt.as_str(),"step":fact.step.value(),"fingerprint":fingerprint})
}
pub fn decode(value: &Value) -> Result<RequestCacheFact, String> {
    let object = value
        .as_object()
        .filter(|o| o.len() == 3)
        .ok_or("invalid request cache fact")?;
    let attempt = object
        .get("attempt")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .and_then(PhysicalAttemptId::new)
        .ok_or("invalid request cache attempt")?;
    let step = object
        .get("step")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .and_then(StepNumber::new)
        .ok_or("invalid request cache Step")?;
    let raw = object
        .get("fingerprint")
        .ok_or("missing request fingerprint")?;
    let fingerprint = if raw.is_null() {
        None
    } else {
        let o = raw
            .as_object()
            .filter(|o| o.len() == 9)
            .ok_or("invalid request fingerprint")?;
        let h = |key| {
            o.get(key)
                .ok_or_else(|| "missing request hash".to_owned())
                .and_then(parse_hash)
        };
        let count = o
            .get("inputItems")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("invalid request input count")?;
        let prefixes = o
            .get("prefixes")
            .and_then(Value::as_array)
            .filter(|items| items.len() <= MAX_PREFIX_ITEMS)
            .ok_or("request prefix bound")?
            .iter()
            .map(parse_hash)
            .collect::<Result<Vec<_>, _>>()?;
        Some(
            RequestCacheFingerprint::new(
                RequestComponents {
                    target: h("target")?,
                    cache_key: h("cacheKey")?,
                    routing: h("routing")?,
                    tools: h("tools")?,
                    instructions: h("instructions")?,
                    options: h("options")?,
                    input: h("input")?,
                },
                prefixes,
                count,
            )
            .ok_or("request prefix count mismatch")?,
        )
    };
    Ok(RequestCacheFact {
        attempt,
        step,
        fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fact(items: u32) -> Result<RequestCacheFact, String> {
        let components = RequestComponents {
            target: [1; 32],
            cache_key: [2; 32],
            routing: [3; 32],
            tools: [4; 32],
            instructions: [5; 32],
            options: [6; 32],
            input: [7; 32],
        };
        Ok(RequestCacheFact {
            attempt: PhysicalAttemptId::new("request-cache-fixture").ok_or("attempt")?,
            step: StepNumber::new(1).ok_or("step")?,
            fingerprint: Some(
                RequestCacheFingerprint::new(
                    components,
                    vec![[8; 32]; (items as usize).min(MAX_PREFIX_ITEMS)],
                    items,
                )
                .ok_or("fingerprint")?,
            ),
        })
    }
    #[test]
    fn bounded_hash_only_record_round_trips_and_refuses_content_fields() -> Result<(), String> {
        let fact = fact(600)?;
        let value = encode(&fact);
        assert_eq!(decode(&value)?, fact);
        assert!(value.to_string().len() < MAX_RECORD_BYTES);
        let mut invalid = value.clone();
        invalid["fingerprint"]["instructions"] = json!("private message text");
        assert!(decode(&invalid).is_err());
        invalid = value.clone();
        invalid["message"] = json!("private message text");
        assert!(decode(&invalid).is_err());
        invalid = value.clone();
        invalid["fingerprint"]["prefixes"]
            .as_array_mut()
            .ok_or("prefixes")?
            .push(json!(hash_hex(&[8; 32])));
        assert!(decode(&invalid).is_err());
        invalid = value;
        invalid["fingerprint"]["inputItems"] = json!(0);
        assert!(decode(&invalid).is_err());
        Ok(())
    }
    #[test]
    fn explicit_unavailable_is_distinct_from_missing_or_empty() -> Result<(), String> {
        let mut fact = fact(0)?;
        let empty = encode(&fact);
        fact.fingerprint = None;
        let unavailable = encode(&fact);
        assert_ne!(empty, unavailable);
        assert_eq!(decode(&unavailable)?, fact);
        assert!(decode(&json!({"attempt":"a","step":1})).is_err());
        Ok(())
    }
}
