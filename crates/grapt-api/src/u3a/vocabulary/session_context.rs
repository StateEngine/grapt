//! Retained process-private capability and durable instruction-entry wire shapes.

use super::prelude::*;
use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SessionCapabilityDto(BoundedString<65_536>);

impl fmt::Debug for SessionCapabilityDto {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("SessionCapabilityDto([REDACTED])")
    }
}

impl SessionCapabilityDto {
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct InstructionEntryKeyDto(BoundedString<256>);

impl InstructionEntryKeyDto {
    pub fn parse(raw: String) -> Option<Self> {
        let first = raw.bytes().next()?;
        if !(first.is_ascii_lowercase() || first.is_ascii_digit())
            || !raw.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
            })
        {
            return None;
        }
        BoundedString::try_new(raw).ok().map(Self)
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> serde::Deserialize<'de> for InstructionEntryKeyDto {
    fn deserialize<D: serde::Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let raw = <BoundedString<256> as serde::Deserialize>::deserialize(input)?;
        Self::parse(raw.as_str().to_owned())
            .ok_or_else(|| D::Error::custom("invalid instruction entry key"))
    }
}

/// General JSON with explicit entry-sized limits. Metadata's 8 KiB scalar
/// limit cannot carry a normal 32 KiB StateEngine context packet.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct InstructionJsonDto(Value);

impl InstructionJsonDto {
    pub fn value(&self) -> &Value {
        &self.0
    }
}

impl<'de> serde::Deserialize<'de> for InstructionJsonDto {
    fn deserialize<D: serde::Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let mut budget = JsonBudget { nodes: 0, bytes: 0 };
        JsonSeed {
            budget: &mut budget,
            depth: 0,
        }
        .deserialize(input)
        .map(Self)
    }
}

struct JsonBudget {
    nodes: usize,
    bytes: usize,
}
impl JsonBudget {
    fn add<E: Error>(&mut self, bytes: usize) -> Result<(), E> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| E::custom("JSON byte overflow"))?;
        if self.bytes > 65_536 {
            return Err(E::custom("instruction JSON exceeds 64 KiB"));
        }
        Ok(())
    }
    fn string<E: Error>(&mut self, value: &str) -> Result<(), E> {
        let encoded = serde_json::to_string(value).map_err(E::custom)?;
        self.add(encoded.len())
    }
}
struct JsonSeed<'a> {
    budget: &'a mut JsonBudget,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for JsonSeed<'_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, input: D) -> Result<Value, D::Error> {
        self.budget.nodes += 1;
        if self.depth > 16 || self.budget.nodes > 1_024 {
            return Err(D::Error::custom(
                "instruction JSON nesting/node bound exceeded",
            ));
        }
        input.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for JsonSeed<'_> {
    type Value = Value;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded instruction JSON")
    }
    fn visit_unit<E: Error>(self) -> Result<Value, E> {
        self.budget.add(4)?;
        Ok(Value::Null)
    }
    fn visit_bool<E: Error>(self, value: bool) -> Result<Value, E> {
        self.budget.add(if value { 4 } else { 5 })?;
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: Error>(self, value: i64) -> Result<Value, E> {
        self.budget.add(value.to_string().len())?;
        Ok(value.into())
    }
    fn visit_u64<E: Error>(self, value: u64) -> Result<Value, E> {
        self.budget.add(value.to_string().len())?;
        Ok(value.into())
    }
    fn visit_f64<E: Error>(self, value: f64) -> Result<Value, E> {
        let number = serde_json::Number::from_f64(value)
            .ok_or_else(|| E::custom("nonfinite JSON number"))?;
        self.budget.add(number.to_string().len())?;
        Ok(Value::Number(number))
    }
    fn visit_str<E: Error>(self, value: &str) -> Result<Value, E> {
        if value.len() > 65_536 {
            return Err(E::custom("instruction JSON scalar bound exceeded"));
        }
        self.budget.string(value)?;
        Ok(Value::String(value.to_owned()))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<Value, A::Error> {
        self.budget.add(2)?;
        let mut values = Vec::new();
        while let Some(value) = input.next_element_seed(JsonSeed {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            if !values.is_empty() {
                self.budget.add(1)?;
            }
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Value, A::Error> {
        self.budget.add(2)?;
        let mut values = serde_json::Map::new();
        while let Some(key) = input.next_key::<BoundedString<256>>()? {
            if values.contains_key(key.as_str()) {
                return Err(A::Error::custom("duplicate instruction JSON member"));
            }
            self.budget.string(key.as_str())?;
            self.budget.add(if values.is_empty() { 1 } else { 2 })?;
            let value = input.next_value_seed(JsonSeed {
                budget: self.budget,
                depth: self.depth + 1,
            })?;
            values.insert(key.as_str().to_owned(), value);
        }
        Ok(Value::Object(values))
    }
}

closed_object! { pub struct SessionCapabilityPutDto => "inline:session-capability-put" {
    required capability: SessionCapabilityDto => "capability",
    optional config_path: BoundedString<4096> => "configPath",
} }
closed_object! { pub struct InstructionEntryPutDto => "inline:instruction-entry-put" {
    nullable value: InstructionJsonDto => "value",
} }
closed_object! { pub struct InstructionEntryInfoDto => "InstructionEntry.Info" {
    required key: InstructionEntryKeyDto => "key",
    nullable value: InstructionJsonDto => "value",
} }
closed_object! { pub struct InstructionEntryListDto => "inline:instruction-entry-list" {
    required data: BoundedVec<InstructionEntryInfoDto, MAX_CATALOG_ROWS> => "data",
} }
closed_object! { pub struct InstructionInspectionDto => "inline:instruction-inspection" {
    required captured: bool => "captured",
    required ambient: BoundedString<1_048_576> => "ambient",
    required keys: BoundedVec<BoundedString<256>, MAX_CATALOG_ROWS> => "keys",
} }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_accepts_packet_sized_scalars_and_json_null_but_refuses_ambiguous_values() {
        for value in [
            Value::Null,
            Value::String("p".repeat(32_768)),
            serde_json::json!([1, true, null]),
        ] {
            let input = serde_json::json!({"value":value});
            let decoded: InstructionEntryPutDto = serde_json::from_value(input.clone())
                .unwrap_or_else(|error| panic!("valid context refused: {error}"));
            assert_eq!(serde_json::to_value(decoded).ok(), Some(input));
        }
        for raw in [
            r#"{}"#,
            r#"{"value":null,"value":1}"#,
            r#"{"value":{"identity":"a","identity":"b"}}"#,
            r#"{"value":1,"foreign":true}"#,
        ] {
            assert!(
                serde_json::from_str::<InstructionEntryPutDto>(raw).is_err(),
                "accepted {raw}"
            );
        }
        assert!(serde_json::from_value::<InstructionEntryPutDto>(
            serde_json::json!({"value":"p".repeat(65_537)})
        )
        .is_err());
        for key in ["", "../identity", "api/key", "Upper", "a%2fb"] {
            assert!(InstructionEntryKeyDto::parse(key.to_owned()).is_none());
        }
        let secret: SessionCapabilityPutDto =
            serde_json::from_str(r#"{"capability":"never-log-this-token"}"#)
                .unwrap_or_else(|error| panic!("capability fixture: {error}"));
        assert!(!format!("{secret:?}").contains("never-log-this-token"));
    }
}
