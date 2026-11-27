//! Canonical bounded base64 wire values.
use super::DtoString;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Base64Dto(DtoString);

impl Base64Dto {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn valid_canonical_base64(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    if !bytes.len().is_multiple_of(4) {
        return false;
    }
    let padding = bytes.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2 {
        return false;
    }
    let content = bytes.len() - padding;
    if bytes[..content]
        .iter()
        .any(|byte| base64_value(*byte).is_none())
        || bytes[content..].iter().any(|byte| *byte != b'=')
    {
        return false;
    }
    match padding {
        0 => true,
        1 => base64_value(bytes[content - 1]).is_some_and(|value| value & 0b11 == 0),
        2 => base64_value(bytes[content - 1]).is_some_and(|value| value & 0b1111 == 0),
        _ => false,
    }
}

impl<'de> Deserialize<'de> for Base64Dto {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = DtoString::deserialize(deserializer)?;
        if !valid_canonical_base64(value.as_str().as_bytes()) {
            return Err(D::Error::custom("invalid canonical base64 wire value"));
        }
        Ok(Self(value))
    }
}
