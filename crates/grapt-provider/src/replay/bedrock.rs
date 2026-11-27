//! Native Converse reasoning retention with bounded signed blocks.
//! License and attribution notices are retained in THIRD_PARTY_NOTICES.md.
use super::{ReplayBinding, MAX_REASONING_ITEM_BYTES};
use crate::AdapterState;
use serde_json::{json, Value};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Thinking(Value);
impl std::fmt::Debug for Thinking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BedrockThinking([REDACTED])")
    }
}
impl Thinking {
    pub(super) fn restore(item: &Value) -> Result<Self, &'static str> {
        if item.to_string().len() > MAX_REASONING_ITEM_BYTES
            || item.as_object().is_none_or(|object| object.len() != 1)
        {
            return Err("invalid Bedrock reasoning replay");
        }
        if let Some(text) = item.get("reasoningText") {
            if text.as_object().is_none_or(|object| object.len() != 2)
                || text["text"].as_str().is_none()
                || text["signature"].as_str().is_none_or(str::is_empty)
            {
                return Err("invalid Bedrock reasoning signature");
            }
        } else {
            decode(
                item["redactedContent"]
                    .as_str()
                    .ok_or("missing Bedrock redacted reasoning")?,
            )?;
        }
        Ok(Self(item.clone()))
    }
    pub(super) fn block(&self) -> Value {
        json!({"reasoningContent":self.0})
    }
    pub(super) fn bytes(&self) -> usize {
        self.0.to_string().len()
    }
    pub(super) fn digest(&self, field: &mut impl FnMut(&[u8])) {
        field(b"bedrock-reasoning-v1");
        field(self.0.to_string().as_bytes());
    }
}

/// One reasoning block, bounded before growth. Redacted byte chunks are
/// decoded separately and re-encoded together; padded base64 cannot concatenate.
#[derive(Default)]
pub(crate) struct Accumulation {
    text: String,
    signature: String,
    redacted: Vec<u8>,
}
impl std::fmt::Debug for Accumulation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BedrockReasoning([REDACTED])")
    }
}
impl Accumulation {
    pub(crate) fn delta(&mut self, delta: &Value) -> Result<Option<&str>, &'static str> {
        let object = delta.as_object().ok_or("invalid Bedrock reasoning delta")?;
        if object.len() != 1 {
            return Err("Bedrock reasoning delta must have one member");
        }
        if let Some(value) = object.get("redactedContent") {
            if !self.text.is_empty() || !self.signature.is_empty() {
                return Err("Bedrock redacted and signed reasoning overlap");
            }
            let bytes = decode(value.as_str().ok_or("invalid Bedrock redacted reasoning")?)?;
            if self.redacted.len().saturating_add(bytes.len()) > MAX_REASONING_ITEM_BYTES / 4 * 3 {
                return Err("Bedrock redacted reasoning exceeds its bound");
            }
            self.redacted.extend(bytes);
            return Ok(None);
        }
        if !self.redacted.is_empty() {
            return Err("Bedrock signed and redacted reasoning overlap");
        }
        if let Some(value) = object.get("signature") {
            let value = value
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("missing Bedrock signature")?;
            if !self.signature.is_empty() && self.signature != value {
                return Err("Bedrock reasoning signature changed");
            }
            if self.text.len().saturating_add(value.len()) > MAX_REASONING_ITEM_BYTES {
                return Err("Bedrock reasoning exceeds its bound");
            }
            self.signature = value.to_owned();
            return Ok(None);
        }
        let value = object
            .get("text")
            .and_then(Value::as_str)
            .ok_or("invalid Bedrock reasoning text")?;
        if !self.signature.is_empty()
            || self.text.len().saturating_add(value.len()) > MAX_REASONING_ITEM_BYTES
        {
            return Err("Bedrock reasoning text follows its signature or exceeds its bound");
        }
        let offset = self.text.len();
        self.text.push_str(value);
        Ok(Some(&self.text[offset..]))
    }
    pub(crate) fn finish(self, binding: &ReplayBinding) -> Result<AdapterState, &'static str> {
        let item = if !self.redacted.is_empty() {
            json!({"redactedContent":encode(&self.redacted)})
        } else if !self.signature.is_empty() {
            json!({"reasoningText":{"text":self.text,"signature":self.signature}})
        } else {
            // Converse makes signatures optional. Unsigned reasoning remains
            // visible reasoning, without manufacturing opaque replay state.
            return Ok(AdapterState::none());
        };
        Thinking::restore(&item)?;
        AdapterState::retain(
            json!({"kind":"bedrock-reasoning-v1","item":item,"binding":binding.0}).to_string(),
        )
        .map_err(|_| "Bedrock reasoning exceeds its artifact bound")
    }
}

/// Model-version detection follows the pinned upstream, ignoring snapshot dates.
pub(crate) fn binding_controls(model: &str) -> bool {
    let Some((_, suffix)) = model.split_once("claude-") else {
        return false;
    };
    let Some((_, version)) = suffix.split_once('-') else {
        return false;
    };
    let mut parts = version.split(['-', '.', ':', '@']);
    let Some(major) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    let minor = parts
        .next()
        .filter(|part| part.len() <= 2)
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(0);
    major > 5 || major == 5 && minor >= 1
}

// Standard padded-base64 adaptation of this repository's bounded cursor codec.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
fn decode(text: &str) -> Result<Vec<u8>, &'static str> {
    if text.is_empty() || text.len() > MAX_REASONING_ITEM_BYTES || !text.len().is_multiple_of(4) {
        return Err("invalid Bedrock redacted base64 length");
    }
    let mut bytes = Vec::new();
    let (mut value, mut bits) = (0u32, 0usize);
    for byte in text.trim_end_matches('=').bytes() {
        let index = ALPHABET
            .iter()
            .position(|entry| *entry == byte)
            .ok_or("invalid Bedrock redacted base64")?;
        value = (value << 6) | index as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((value >> bits) as u8);
            value &= (1u32 << bits) - 1;
        }
    }
    if value != 0 || bytes.is_empty() || encode(&bytes) != text {
        return Err("noncanonical Bedrock redacted base64");
    }
    Ok(bytes)
}
fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        out.push(ALPHABET[(value >> 18) as usize] as char);
        out.push(ALPHABET[((value >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
