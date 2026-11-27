//! The bounded UTF-16 recent-text value and its wire forms.
//!
//! Split from `events` by the module-size ratchet (the 1,000-line ceiling
//! of grapt-types no_god_files): the value, its serde forms and the
//! canonical base64 helpers are one cohesive responsibility — the
//! oracle's UTF-16 recent text in its wire, durable and public-history
//! forms.

use std::borrow::Cow;

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::limits::MAX_DTO_STRING_BYTES;

/// A bounded sequence of UTF-16 code units: the oracle's native unit for
/// recent text. The oracle truncates at 2_000 UTF-16 units, which can split
/// a surrogate pair and leave a lone high surrogate at the cut. That value
/// has no valid UTF-8 form, so the wire form serializes it as the JSON
/// string the oracle emits (the unit escapes verbatim), while the durable
/// form carries the internal "secode.unit16" unit object. Public history
/// rows carry the same value as [`Utf16TextPublic`], whose encodings a
/// strict JSON parser always re-parses.
///
/// The byte bound is the same DTO string bound as DtoString: the UTF-8
/// length of the decoded text, counting every lone surrogate at the maximum
/// four bytes, so a boundary value can never outrun the bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utf16Text {
    units: Vec<u16>,
}

/// The serde_json private-token contract: a struct of exactly one field
/// with this name serializes the field's string payload verbatim.
const RAW_VALUE_TOKEN: &str = "$serde_json::private::RawValue";

impl Utf16Text {
    /// The UTF-8 byte length of the decoded text. Lone surrogates count at
    /// the maximum four bytes each, which keeps the bound fail-closed.
    fn encoded_len(units: &[u16]) -> usize {
        let mut total = 0;
        let mut index = 0;
        while index < units.len() {
            let unit = units[index];
            if (0xD800..=0xDBFF).contains(&unit) {
                if index + 1 < units.len() {
                    let next = units[index + 1];
                    if (0xDC00..=0xDFFF).contains(&next) {
                        total += 4;
                        index += 2;
                        continue;
                    }
                }
                total += 4;
            } else if (0xDC00..=0xDFFF).contains(&unit) {
                total += 4;
            } else {
                total += char::from_u32(unit as u32).map_or(4, |ch| ch.len_utf8());
            }
            index += 1;
        }
        total
    }

    /// Build from code units, refusing values past the DTO string bound.
    pub fn try_new(units: Vec<u16>) -> Result<Self, &'static str> {
        if Self::encoded_len(&units) > MAX_DTO_STRING_BYTES {
            return Err("utf16 text exceeds its dto string bound");
        }
        Ok(Self { units })
    }

    /// The empty value: no units.
    pub fn empty() -> Self {
        Self { units: Vec::new() }
    }

    pub fn units(&self) -> &[u16] {
        &self.units
    }

    /// The decoded text when every unit forms valid UTF-16 text; a lone
    /// surrogate at the truncation cut makes this None.
    pub fn as_str(&self) -> Option<Cow<'_, str>> {
        match String::from_utf16(&self.units) {
            Ok(text) => Some(Cow::Owned(text)),
            Err(_) => None,
        }
    }

    /// The internal durable form: canonical standard-alphabet base64 of
    /// the big-endian unit bytes.
    pub fn internal_b64(&self) -> String {
        let mut bytes = Vec::with_capacity(self.units.len() * 2);
        for unit in &self.units {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        base64_encode(&bytes)
    }

    fn from_b64(value: &str) -> Result<Self, &'static str> {
        let bytes = base64_decode(value).ok_or("internal unit form is not canonical base64")?;
        if !bytes.len().is_multiple_of(2) {
            return Err("internal unit form carries an odd unit byte count");
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        Self::try_new(units)
    }

    /// One JSON string escape in serde_json's own style: the short forms
    /// for the five named controls, \u00XX for the rest below 0x20,
    /// nothing for 0x20 and up.
    fn escape_json_char(ch: char, out: &mut String) {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{000C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            _ if (ch as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            _ => out.push(ch),
        }
    }
}

impl Serialize for Utf16Text {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match String::from_utf16(&self.units) {
            Ok(text) => serializer.serialize_str(&text),
            Err(_) => {
                let mut fragment = String::with_capacity(self.units.len() * 6 + 2);
                fragment.push('"');
                let mut index = 0;
                while index < self.units.len() {
                    let unit = self.units[index];
                    let next = self.units.get(index + 1).copied();
                    if (0xD800..=0xDBFF).contains(&unit)
                        && next.is_some_and(|next| (0xDC00..=0xDFFF).contains(&next))
                    {
                        let scalar = 0x1_0000
                            + (((unit - 0xD800) as u32) << 10)
                            + (self.units[index + 1] - 0xDC00) as u32;
                        if let Some(ch) = char::from_u32(scalar) {
                            Self::escape_json_char(ch, &mut fragment);
                            index += 2;
                            continue;
                        }
                    }
                    if (0xD800..=0xDFFF).contains(&unit) {
                        fragment.push_str(&format!("\\u{:04x}", unit));
                    } else {
                        let ch = char::from_u32(unit as u32);
                        match ch {
                            Some(ch) => Self::escape_json_char(ch, &mut fragment),
                            None => fragment.push_str(&format!("\\u{:04x}", unit)),
                        }
                    }
                    index += 1;
                }
                fragment.push('"');
                let mut state = serializer.serialize_struct(RAW_VALUE_TOKEN, 1)?;
                state.serialize_field(RAW_VALUE_TOKEN, fragment.as_str())?;
                state.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Utf16Text {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UnitsVisitor;

        impl<'de> Visitor<'de> for UnitsVisitor {
            type Value = Utf16Text;

            fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
                formatter.write_str("a utf16 text string or an internal unit object")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Utf16Text::try_new(value.encode_utf16().collect()).map_err(serde::de::Error::custom)
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Utf16Text::try_new(value.encode_utf16().collect()).map_err(serde::de::Error::custom)
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let Some(key) = access.next_key::<String>()? else {
                    return Err(serde::de::Error::custom("internal unit object is empty"));
                };
                if key != "secode.unit16" {
                    return Err(serde::de::Error::custom(
                        "internal unit object carries an unknown key",
                    ));
                }
                let value: String = access.next_value()?;
                if access.next_key::<String>()?.is_some() {
                    return Err(serde::de::Error::custom(
                        "internal unit object carries more than one key",
                    ));
                }
                Utf16Text::from_b64(&value).map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_any(UnitsVisitor)
    }
}

/// The public history form of [`Utf16Text`]: always the frozen string wire
/// form the oracle emits, a truncation cut escaping a lone surrogate
/// verbatim as its unit escape. The lossless "secode.unit16" unit object
/// is the durable and internal form only: public rows pass through strict
/// JSON parsers, so the object never leaves the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utf16TextPublic(Utf16Text);

impl Utf16TextPublic {
    /// Wraps a bounded recent value for a public history row.
    pub fn new(text: Utf16Text) -> Self {
        Self(text)
    }

    /// Borrows the bounded units.
    pub fn as_inner(&self) -> &Utf16Text {
        &self.0
    }
}

impl Serialize for Utf16TextPublic {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Utf16TextPublic {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Utf16Text::deserialize(deserializer).map(Self)
    }
}
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = (chunk[0] as u32) << 16
            | (chunk.get(1).copied().unwrap_or(0) as u32) << 8
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(BASE64_ALPHABET[(triple >> 18) as usize] as char);
        out.push(BASE64_ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        match chunk.len() {
            1 => out.push('='),
            _ => out.push(BASE64_ALPHABET[((triple >> 6) & 0x3F) as usize] as char),
        }
        match chunk.len() {
            1 | 2 => out.push('='),
            _ => out.push(BASE64_ALPHABET[(triple & 0x3F) as usize] as char),
        }
    }
    out
}

/// Canonical standard-alphabet decode: padding of at most two, only at the
/// end, and the spare bits of a padded quantum must be zero.
fn base64_decode(value: &str) -> Option<Vec<u8>> {
    let bytes = value.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) || bytes.len() > MAX_DTO_STRING_BYTES * 2
    {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut quantum = 0u32;
        let mut pad = 0u32;
        for (position, symbol) in chunk.iter().enumerate() {
            if *symbol == b'=' {
                if position < 2 {
                    return None;
                }
                pad += 1;
                quantum <<= 6;
                continue;
            }
            if pad > 0 {
                return None;
            }
            let decoded = BASE64_ALPHABET
                .iter()
                .position(|known| known == symbol)
                .map(|index| index as u32)?;
            quantum = (quantum << 6) | decoded;
        }
        if pad > 2 || quantum & ((1u32 << (pad * 8)) - 1) != 0 {
            return None;
        }
        for shift in (pad * 8..=16).rev().step_by(8) {
            out.push((quantum >> shift) as u8);
        }
    }
    Some(out)
}
