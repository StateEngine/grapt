//! Bounded IA0 wire primitives without endpoint vocabulary or an OpenAPI registry.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};
use std::marker::PhantomData;

use grapt_schema::DecodeLimits;
use serde::de::{
    DeserializeOwned, DeserializeSeed, Error as DeError, IgnoredAny, MapAccess, SeqAccess, Visitor,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use super::limits::{
    U3aLimits, MAX_DTO_STRING_BYTES, MAX_METADATA_BYTES, MAX_METADATA_DEPTH, MAX_METADATA_KEYS,
    MAX_METADATA_KEY_BYTES, MAX_METADATA_NODES, MAX_METADATA_SCALAR_BYTES, MAX_PROMPT_TEXT_BYTES,
};

#[derive(Debug, Error)]
pub enum DtoCodecError {
    #[error("request DTO decoding failed: {0}")]
    Decode(#[from] grapt_schema::DecodeError),
    #[error("DTO encoding failed")]
    Encode,
    #[error("encoded DTO exceeds the configured page-byte bound")]
    PageBytes,
    #[error("invalid U3a decode limits")]
    Limits,
}

pub fn decode_request<T>(bytes: &[u8], limits: U3aLimits) -> Result<T, DtoCodecError>
where
    T: DeserializeOwned,
{
    let receipt = limits.receipt();
    let decode = DecodeLimits::new(receipt.request_dto_bytes, receipt.nested_items)
        .map_err(|_| DtoCodecError::Limits)?;
    grapt_schema::decode_json(bytes, decode).map_err(DtoCodecError::Decode)
}

#[path = "image_prompt.rs"]
mod image_prompt;
pub use image_prompt::decode_image_prompt;

/// Decode a RESPONSE page, bounded by the page-byte bound rather than the
/// request-byte bound.
///
/// This surface deliberately bounds requests and pages differently — a request
/// is small by design and a page carries a whole catalog — but until now it
/// offered only [`decode_request`], so a client had no way to READ what
/// [`encode_page`] is permitted to WRITE. The asymmetry was one-directional and
/// therefore a defect: the real model list encodes to 134,604 bytes, which
/// `encode_page` emits happily and `decode_request` refuses at 65,536, so every
/// client saw `InputTooLarge` on a response the server considered valid.
///
/// Bounding a response by the request bound is not a smaller safety margin, it
/// is the WRONG bound: it limits what a peer may send us, applied to what we
/// ourselves just sent them.
pub fn decode_page<T>(bytes: &[u8], limits: U3aLimits) -> Result<T, DtoCodecError>
where
    T: DeserializeOwned,
{
    let receipt = limits.receipt();
    let decode = DecodeLimits::new(receipt.page_bytes, receipt.nested_items)
        .map_err(|_| DtoCodecError::Limits)?;
    grapt_schema::decode_json(bytes, decode).map_err(DtoCodecError::Decode)
}

pub fn encode_page<T>(value: &T, limits: U3aLimits) -> Result<Vec<u8>, DtoCodecError>
where
    T: Serialize,
{
    let mut writer = PageWriter(Vec::new(), limits.receipt().page_bytes);
    match serde_json::to_writer(&mut writer, value) {
        Ok(()) => Ok(writer.0),
        Err(error) if error.is_io() => Err(DtoCodecError::PageBytes),
        Err(_) => Err(DtoCodecError::Encode),
    }
}

struct PageWriter(Vec<u8>, usize);

impl Write for PageWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self.0.len().checked_add(bytes.len());
        if next.is_none_or(|next| next > self.1) {
            return Err(io::Error::other("page byte bound exceeded"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// `Default` is the empty string, which every bound admits.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BoundedString<const MAX: usize>(String);

impl<const MAX: usize> BoundedString<MAX> {
    pub fn try_new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.len() > MAX {
            return Err("string exceeds its byte bound");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl<const MAX: usize> fmt::Debug for BoundedString<MAX> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedString")
            .field("bytes", &self.0.len())
            .finish()
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for BoundedString<MAX> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BoundedStringVisitor<const MAX: usize>;
        impl<const MAX: usize> Visitor<'_> for BoundedStringVisitor<MAX> {
            type Value = BoundedString<MAX>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "a string of at most {MAX} UTF-8 bytes")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: DeError,
            {
                if value.len() > MAX {
                    return Err(E::custom("string exceeds its byte bound"));
                }
                Ok(BoundedString(value.to_owned()))
            }
        }
        deserializer.deserialize_string(BoundedStringVisitor::<MAX>)
    }
}

pub type DtoString = BoundedString<MAX_DTO_STRING_BYTES>;
pub type PromptText = BoundedString<MAX_PROMPT_TEXT_BYTES>;
pub type MetadataKey = BoundedString<MAX_METADATA_KEY_BYTES>;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireId<T>(T);

pub trait WireIdentity: Sized + fmt::Display {
    fn parse_wire(value: String) -> Option<Self>;
}

macro_rules! opaque_wire_identity {
    ($($type:path),+ $(,)?) => {
        $(
            impl WireIdentity for $type {
                fn parse_wire(value: String) -> Option<Self> {
                    Self::new(value)
                }
            }
        )+
    };
}

opaque_wire_identity!(
    grapt_types::AgentId,
    grapt_types::OperationId,
    grapt_types::DecisionNonce,
    grapt_types::ExecutionId,
    grapt_types::ModelId,
    grapt_types::ModelVariantId,
    grapt_types::ProjectId,
    grapt_types::ProviderId,
    grapt_types::RequestId,
    grapt_types::ToolCallId,
    grapt_types::WorkspaceId,
);

impl WireIdentity for grapt_types::SessionId {
    fn parse_wire(value: String) -> Option<Self> {
        value.parse().ok()
    }
}

impl WireIdentity for grapt_types::MessageId {
    fn parse_wire(value: String) -> Option<Self> {
        value.parse().ok()
    }
}

impl WireIdentity for grapt_types::EventId {
    fn parse_wire(value: String) -> Option<Self> {
        value.parse().ok()
    }
}

impl WireIdentity for grapt_types::PermissionId {
    fn parse_wire(value: String) -> Option<Self> {
        value.parse().ok()
    }
}

impl WireIdentity for grapt_types::McpLoginId {
    fn parse_wire(value: String) -> Option<Self> {
        if value.len() > 128 {
            return None;
        }
        value.parse().ok()
    }
}

impl WireIdentity for grapt_types::FormId {
    fn parse_wire(value: String) -> Option<Self> {
        value.parse().ok()
    }
}

impl<T: fmt::Debug> fmt::Debug for WireId<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("WireId").field(&self.0).finish()
    }
}

impl<T> WireId<T> {
    pub fn as_inner(&self) -> &T {
        &self.0
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T: WireIdentity> WireId<T> {
    pub fn parse(value: String) -> Option<Self> {
        T::parse_wire(value).map(Self)
    }
}

impl<T: fmt::Display> Serialize for WireId<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(&self.0)
    }
}

impl<'de, T> Deserialize<'de> for WireId<T>
where
    T: WireIdentity,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = DtoString::deserialize(deserializer)?.0;
        T::parse_wire(value)
            .map(Self)
            .ok_or_else(|| D::Error::custom("invalid typed wire identity"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FiniteF64(f64);

/// A token count that every JavaScript consumer can carry without rounding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SafeTokenCount(u64);

impl SafeTokenCount {
    pub const MAX: u64 = 9_007_199_254_740_991;

    pub fn try_new(value: u64) -> Result<Self, &'static str> {
        if value > Self::MAX {
            return Err("token count exceeds JavaScript safe integer range");
        }
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for SafeTokenCount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > Self::MAX as f64 {
            return Err(D::Error::custom(
                "token count must be a nonnegative safe integer",
            ));
        }
        Self::try_new(value as u64).map_err(D::Error::custom)
    }
}

impl FiniteF64 {
    pub fn try_new(value: f64) -> Result<Self, &'static str> {
        if !value.is_finite() {
            return Err("number must be finite");
        }
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for FiniteF64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::try_new(value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FiniteOrSentinelF64 {
    Finite(FiniteF64),
    NaN,
    Infinity,
    NegativeInfinity,
}

impl Serialize for FiniteOrSentinelF64 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Finite(value) => value.serialize(serializer),
            Self::NaN => serializer.serialize_str("NaN"),
            Self::Infinity => serializer.serialize_str("Infinity"),
            Self::NegativeInfinity => serializer.serialize_str("-Infinity"),
        }
    }
}

impl<'de> Deserialize<'de> for FiniteOrSentinelF64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct NumberOrSentinelVisitor;
        impl Visitor<'_> for NumberOrSentinelVisitor {
            type Value = FiniteOrSentinelF64;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a finite number or exact NaN/Infinity/-Infinity sentinel")
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(FiniteOrSentinelF64::Finite(FiniteF64(value as f64)))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(FiniteOrSentinelF64::Finite(FiniteF64(value as f64)))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: DeError,
            {
                FiniteF64::try_new(value)
                    .map(FiniteOrSentinelF64::Finite)
                    .map_err(E::custom)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: DeError,
            {
                match value {
                    "NaN" => Ok(FiniteOrSentinelF64::NaN),
                    "Infinity" => Ok(FiniteOrSentinelF64::Infinity),
                    "-Infinity" => Ok(FiniteOrSentinelF64::NegativeInfinity),
                    _ => Err(E::unknown_variant(value, &["NaN", "Infinity", "-Infinity"])),
                }
            }
        }
        deserializer.deserialize_any(NumberOrSentinelVisitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WireI64(i64);

impl From<i64> for WireI64 {
    fn from(value: i64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NonNegativeU64(u64);

impl From<u64> for NonNegativeU64 {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

macro_rules! validated_wire_scalar {
    ($name:ident, $ty:ty, $serialize:ident, $valid:expr, $message:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name($ty);

        impl $name {
            pub fn try_new(value: $ty) -> Result<Self, &'static str> {
                if !($valid)(value) {
                    return Err($message);
                }
                Ok(Self(value))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.$serialize(self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = <$ty>::deserialize(deserializer)?;
                Self::try_new(value).map_err(D::Error::custom)
            }
        }
    };
}

validated_wire_scalar!(
    PositiveU64,
    u64,
    serialize_u64,
    |value| value > 0,
    "integer must be positive"
);
validated_wire_scalar!(
    TrueValue,
    bool,
    serialize_bool,
    |value| value,
    "boolean must be true"
);

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct BoundedVec<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> BoundedVec<T, MAX> {
    pub fn try_new(values: Vec<T>) -> Result<Self, &'static str> {
        if values.len() > MAX {
            return Err("collection exceeds its item bound");
        }
        Ok(Self(values))
    }

    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}

impl<'de, T, const MAX: usize> Deserialize<'de> for BoundedVec<T, MAX>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BoundedVecVisitor<T, const MAX: usize>(PhantomData<fn() -> T>);
        impl<'de, T, const MAX: usize> Visitor<'de> for BoundedVecVisitor<T, MAX>
        where
            T: Deserialize<'de>,
        {
            type Value = BoundedVec<T, MAX>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "an array of at most {MAX} items")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                if sequence.size_hint().is_some_and(|size| size > MAX) {
                    return Err(A::Error::custom("collection exceeds its item bound"));
                }
                let mut values = Vec::new();
                while values.len() < MAX {
                    match sequence.next_element()? {
                        Some(value) => values.push(value),
                        None => return Ok(BoundedVec(values)),
                    }
                }
                if sequence.next_element::<IgnoredAny>()?.is_some() {
                    return Err(A::Error::custom("collection exceeds its item bound"));
                }
                Ok(BoundedVec(values))
            }
        }
        deserializer.deserialize_seq(BoundedVecVisitor::<T, MAX>(PhantomData))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct BoundedMap<K, V, const MAX: usize>(BTreeMap<K, V>);

impl<K: Ord, V, const MAX: usize> BoundedMap<K, V, MAX> {
    pub fn try_new(values: BTreeMap<K, V>) -> Result<Self, &'static str> {
        if values.len() > MAX {
            return Err("object exceeds its key bound");
        }
        Ok(Self(values))
    }

    pub fn as_map(&self) -> &BTreeMap<K, V> {
        &self.0
    }
}

impl<'de, K, V, const MAX: usize> Deserialize<'de> for BoundedMap<K, V, MAX>
where
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BoundedMapVisitor<K, V, const MAX: usize>(PhantomData<fn() -> (K, V)>);
        impl<'de, K, V, const MAX: usize> Visitor<'de> for BoundedMapVisitor<K, V, MAX>
        where
            K: Deserialize<'de> + Ord,
            V: Deserialize<'de>,
        {
            type Value = BoundedMap<K, V, MAX>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "an object of at most {MAX} keys")
            }

            fn visit_map<A>(self, mut input: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = BTreeMap::new();
                while let Some(key) = input.next_key()? {
                    if values.len() == MAX {
                        return Err(A::Error::custom("object exceeds its key bound"));
                    }
                    if values.contains_key(&key) {
                        return Err(A::Error::custom("duplicate object key"));
                    }
                    let value = input.next_value()?;
                    values.insert(key, value);
                }
                Ok(BoundedMap(values))
            }
        }
        deserializer.deserialize_map(BoundedMapVisitor::<K, V, MAX>(PhantomData))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum OpenNumber {
    I64(i64),
    U64(u64),
    F64(FiniteF64),
}

impl Serialize for OpenNumber {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::I64(value) => serializer.serialize_i64(*value),
            Self::U64(value) => serializer.serialize_u64(*value),
            Self::F64(value) => value.serialize(serializer),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum OpenValue {
    Null,
    Bool(bool),
    Number(OpenNumber),
    String(BoundedString<MAX_DTO_STRING_BYTES>),
    Array(Vec<OpenValue>),
    Object(BTreeMap<MetadataKey, OpenValue>),
}

impl Serialize for OpenValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(value) => value.serialize(serializer),
            Self::Number(value) => value.serialize(serializer),
            Self::String(value) => value.serialize(serializer),
            Self::Array(value) => value.serialize(serializer),
            Self::Object(value) => value.serialize(serializer),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct OpenObject(BTreeMap<MetadataKey, OpenValue>);

#[path = "open_budget.rs"]
mod open_budget;
use open_budget::OpenBudget;

fn encoded_string_bytes(value: &str) -> usize {
    value.chars().fold(2usize, |total, character| {
        let width = match character {
            '"' | '\\' | '\u{0008}' | '\u{0009}' | '\u{000A}' | '\u{000C}' | '\u{000D}' => 2,
            '\u{0000}'..='\u{001F}' => 6,
            other => other.len_utf8(),
        };
        total.saturating_add(width)
    })
}

struct OpenValueSeed<'a> {
    budget: &'a mut OpenBudget,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for OpenValueSeed<'_> {
    type Value = OpenValue;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.budget.node::<D::Error>(self.depth)?;
        deserializer.deserialize_any(OpenValueVisitor {
            budget: self.budget,
            depth: self.depth,
        })
    }
}

struct OpenItemSeed<'a> {
    budget: &'a mut OpenBudget,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for OpenItemSeed<'_> {
    type Value = OpenValue;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.budget.item::<D::Error>()?;
        OpenValueSeed {
            budget: self.budget,
            depth: self.depth,
        }
        .deserialize(deserializer)
    }
}

struct OpenKeySeed<'a> {
    budget: &'a mut OpenBudget,
}

impl<'de> DeserializeSeed<'de> for OpenKeySeed<'_> {
    type Value = MetadataKey;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.budget.key::<D::Error>()?;
        let key = MetadataKey::deserialize(deserializer)?;
        self.budget
            .encoded::<D::Error>(encoded_string_bytes(key.as_str()))?;
        Ok(key)
    }
}

struct OpenValueVisitor<'a> {
    budget: &'a mut OpenBudget,
    depth: usize,
}

impl<'de> Visitor<'de> for OpenValueVisitor<'_> {
    type Value = OpenValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded JSON metadata")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: DeError,
    {
        self.budget.encoded::<E>(4)?;
        Ok(OpenValue::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: DeError,
    {
        self.budget.encoded::<E>(if value { 4 } else { 5 })?;
        Ok(OpenValue::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: DeError,
    {
        self.budget.encoded::<E>(value.to_string().len())?;
        Ok(OpenValue::Number(OpenNumber::I64(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: DeError,
    {
        self.budget.encoded::<E>(value.to_string().len())?;
        Ok(OpenValue::Number(OpenNumber::U64(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: DeError,
    {
        let value = FiniteF64::try_new(value).map_err(E::custom)?;
        self.budget.encoded::<E>(value.0.to_string().len())?;
        Ok(OpenValue::Number(OpenNumber::F64(value)))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: DeError,
    {
        self.budget.scalar::<E>(value.len())?;
        self.budget.encoded::<E>(encoded_string_bytes(value))?;
        BoundedString::try_new(value.to_owned())
            .map(OpenValue::String)
            .map_err(E::custom)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        self.budget.encoded::<A::Error>(2)?;
        let mut values = Vec::new();
        let mut first = true;
        while let Some(value) = sequence.next_element_seed(OpenItemSeed {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            if !first {
                self.budget.encoded::<A::Error>(1)?;
            }
            first = false;
            values.push(value);
        }
        Ok(OpenValue::Array(values))
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        decode_open_map(map, self.budget, self.depth).map(OpenValue::Object)
    }
}

fn decode_open_map<'de, A>(
    mut input: A,
    budget: &mut OpenBudget,
    depth: usize,
) -> Result<BTreeMap<MetadataKey, OpenValue>, A::Error>
where
    A: MapAccess<'de>,
{
    budget.encoded::<A::Error>(2)?;
    let mut values = BTreeMap::new();
    let mut first = true;
    while let Some(key) = input.next_key_seed(OpenKeySeed { budget })? {
        if values.contains_key(&key) {
            return Err(A::Error::custom("duplicate object key"));
        }
        budget.encoded::<A::Error>(if first { 1 } else { 2 })?;
        first = false;
        let value = input.next_value_seed(OpenValueSeed {
            budget,
            depth: depth + 1,
        })?;
        values.insert(key, value);
    }
    Ok(values)
}

/// Bounded tool-input JSON with collection, byte and depth budgets for admitted
/// provider/local arguments. Metadata continues to use the smaller OpenObject budget.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ToolInputObject(OpenObject);

impl<'de> Deserialize<'de> for ToolInputObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        decode_open_object(deserializer, OpenBudget::tool_input()).map(Self)
    }
}

impl<'de> Deserialize<'de> for OpenObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        decode_open_object(deserializer, OpenBudget::new())
    }
}

fn decode_open_object<'de, D: Deserializer<'de>>(
    deserializer: D,
    mut budget: OpenBudget,
) -> Result<OpenObject, D::Error> {
    struct OpenObjectVisitor<'a> {
        budget: &'a mut OpenBudget,
    }
    impl<'de> Visitor<'de> for OpenObjectVisitor<'_> {
        type Value = OpenObject;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a bounded metadata object")
        }

        fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            decode_open_map(map, self.budget, 1).map(OpenObject)
        }
    }

    budget.node::<D::Error>(1)?;
    deserializer.deserialize_map(OpenObjectVisitor {
        budget: &mut budget,
    })
}

#[path = "codec_base64.rs"]
mod base64;
pub use base64::Base64Dto;
#[path = "open_access.rs"]
mod open_access;
pub use open_access::OpenValueView;

#[cfg(test)]
#[path = "image_prompt_tests.rs"]
mod image_prompt_tests;
