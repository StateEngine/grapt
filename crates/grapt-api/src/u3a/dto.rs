//! Shared handwritten DTO vocabulary support for the Dogfood-0 API.
//!
//! Frozen OpenAPI bytes are evidence only. Production dispatch is compile-resolved
//! through the Rust types and explicit visitors declared under vocabulary.

use serde::de::{DeserializeSeed, MapAccess};

pub use super::codec::{
    Base64Dto, BoundedMap, BoundedString, BoundedVec, DtoString, FiniteF64, FiniteOrSentinelF64,
    MetadataKey, NonNegativeU64, OpenObject, PositiveU64, PromptText, SafeTokenCount, TrueValue,
    WireI64, WireId,
};
pub use super::limits::{
    MAX_DTO_STRING_BYTES, MAX_METADATA_BYTES, MAX_METADATA_DEPTH, MAX_METADATA_KEYS,
    MAX_METADATA_KEY_BYTES, MAX_METADATA_NODES, MAX_METADATA_SCALAR_BYTES, MAX_NESTED_ITEMS,
    MAX_PROMPT_TEXT_BYTES,
};

pub const FROZEN_OPENAPI_SHA256: &str =
    "0c3801f00ebc474222dd8bcf026aac5d34c82bf23ef24ac7111f5d776e758f4b";
pub const FROZEN_SCHEMA_COUNT: usize = 75;
pub const FROZEN_SCHEMA_BYTES: usize = 23_820;
pub const U3A_DTO_VERSION: &str = "grapt.api.dogfood0.dto.v1";

pub const MAX_CATALOG_ROWS: usize = 256;

/// The model list's OWN row bound, larger than [`MAX_CATALOG_ROWS`] because a
/// real provider account's catalog is larger than every other catalog this API
/// serves.
///
/// MEASURED, not guessed. Against the campaign's configured accounts the real
/// product emits 366 model rows — 7 from the OpenAI account and 359 from the
/// OpenRouter account — serialising to 134,604 bytes, or 368 bytes per row.
/// At 256 the route did not truncate, it REFUSED, so no client could list
/// models at all.
///
/// Why 2,048 and not a number fitted to today's catalog: it is 5.6x the measured
/// catalog, which leaves room for an account that grows without another refusal.
/// At the measured 367 bytes per row, 2,048 rows is about 754 KiB — 9% of the
/// 8,388,608-byte page bound — and about 3 MiB even at four times that row size.
///
/// It does NOT follow that every list this bound admits fits every byte cap, and
/// the earlier version of this note claimed exactly that. Rows carry no
/// individual size limit, so 2,048 unusually large rows can exceed the page
/// bound, and a CONSUMER may hold a smaller cap than this surface does — the
/// native transport currently stops at 4 MiB against an 8 MiB page bound. The
/// row bound and the byte bounds are INDEPENDENT and all of them apply; this one
/// is sized so real catalogs are nowhere near any of them, not so the others can
/// be ignored.
///
/// The other catalogs keep [`MAX_CATALOG_ROWS`]. One oversized surface does not
/// widen the rest.
pub const MAX_MODEL_ROWS: usize = 2_048;
pub const MAX_SESSION_ROWS: usize = 128;
pub const MAX_MESSAGE_ROWS: usize = 200;
pub const MAX_PROMPT_FILES: usize = 64;
pub const MAX_PROMPT_AGENTS: usize = 64;

pub type GenericItems<T> = BoundedVec<T, MAX_NESTED_ITEMS>;
pub type CatalogRows<T> = BoundedVec<T, MAX_CATALOG_ROWS>;
pub type SessionRows<T> = BoundedVec<T, MAX_SESSION_ROWS>;
pub type MessageRows<T> = BoundedVec<T, MAX_MESSAGE_ROWS>;
pub type PromptFiles<T> = BoundedVec<T, MAX_PROMPT_FILES>;
pub type PromptAgents<T> = BoundedVec<T, MAX_PROMPT_AGENTS>;

pub trait NamedSchema {
    const OPENAPI_NAME: &'static str;
    const DTO_VERSION: &'static str = U3A_DTO_VERSION;
    const OPTIONAL_NULLABLE_ROWS: &'static [&'static str] = &[];

    fn optional_nullable_encoding_receipts(
    ) -> Result<Vec<TriStateEncodingReceipt>, serde_json::Error> {
        Ok(Vec::new())
    }
}

#[derive(Debug)]
pub struct TriStateEncodingReceipt {
    pub row: &'static str,
    pub wire: &'static str,
    pub absent: serde_json::Value,
    pub null: serde_json::Value,
    pub value: serde_json::Value,
}

/// Replays one or more already-read discriminants before the untouched map.
///
/// The fixed array length is part of the type, so tag replay cannot grow without
/// an explicit reviewed edit.
pub(crate) struct ReplayMap<A, const N: usize> {
    keys: [Option<&'static str>; N],
    values: [Option<String>; N],
    index: usize,
    awaiting_value: bool,
    rest: A,
}

struct ReplayStringDeserializer<E> {
    value: String,
    marker: std::marker::PhantomData<E>,
}

impl<E> ReplayStringDeserializer<E> {
    fn new(value: String) -> Self {
        Self {
            value,
            marker: std::marker::PhantomData,
        }
    }
}

impl<'de, E> serde::Deserializer<'de> for ReplayStringDeserializer<E>
where
    E: serde::de::Error,
{
    type Error = E;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_string(self.value)
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_some(self)
    }

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        serde::Deserializer::deserialize_enum(
            serde::de::value::StringDeserializer::<E>::new(self.value),
            name,
            variants,
            visitor,
        )
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes byte_buf
        unit unit_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

impl<A, const N: usize> ReplayMap<A, N> {
    pub(crate) fn new(keys: [&'static str; N], values: [String; N], rest: A) -> Self {
        Self {
            keys: keys.map(Some),
            values: values.map(Some),
            index: 0,
            awaiting_value: false,
            rest,
        }
    }
}

impl<'de, A, const N: usize> MapAccess<'de> for ReplayMap<A, N>
where
    A: MapAccess<'de>,
{
    type Error = A::Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        if self.awaiting_value {
            return Err(<A::Error as serde::de::Error>::custom(
                "tag value must be consumed before the next key",
            ));
        }
        if self.index == N {
            return self.rest.next_key_seed(seed);
        }
        let Some(key) = self.keys[self.index].take() else {
            return Err(<A::Error as serde::de::Error>::custom(
                "tag replay key was already consumed",
            ));
        };
        self.awaiting_value = true;
        seed.deserialize(serde::de::value::BorrowedStrDeserializer::new(key))
            .map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        if self.index == N {
            return self.rest.next_value_seed(seed);
        }
        if !self.awaiting_value {
            return Err(<A::Error as serde::de::Error>::custom(
                "tag replay value has no key",
            ));
        }
        let Some(value) = self.values[self.index].take() else {
            return Err(<A::Error as serde::de::Error>::custom(
                "tag replay value was already consumed",
            ));
        };
        self.index += 1;
        self.awaiting_value = false;
        seed.deserialize(ReplayStringDeserializer::new(value))
    }
}

macro_rules! field_type {
    (required, $ty:ty) => { grapt_schema::Required<$ty> };
    (optional, $ty:ty) => { grapt_schema::Optional<$ty> };
    (nullable, $ty:ty) => { grapt_schema::Nullable<$ty> };
    (optional_nullable($evidence:ty), $ty:ty) => {
        grapt_schema::OptionalNullable<$ty, $evidence>
    };
}

macro_rules! presence_row {
    (required) => {
        ""
    };
    (optional) => {
        ""
    };
    (nullable) => {
        ""
    };
    (optional_nullable($evidence:ty)) => {
        <$evidence as grapt_schema::TriStateEvidence>::LEDGER_ROW
    };
}

macro_rules! finish_field {
    (required, $slot:ident, $wire:literal, $error:ty) => {
        $slot.ok_or_else(|| <$error as serde::de::Error>::missing_field($wire))?
    };
    (nullable, $slot:ident, $wire:literal, $error:ty) => {
        $slot.ok_or_else(|| <$error as serde::de::Error>::missing_field($wire))?
    };
    (optional, $slot:ident, $wire:literal, $error:ty) => {
        $slot.unwrap_or_else(grapt_schema::Optional::absent)
    };
    (optional_nullable, $slot:ident, $wire:literal, $error:ty) => {
        $slot.unwrap_or_else(grapt_schema::OptionalNullable::absent)
    };
}

macro_rules! serialize_field {
    ($state:ident, $value:expr, required, $wire:literal) => {
        serde::ser::SerializeStruct::serialize_field(&mut $state, $wire, $value)?;
    };
    ($state:ident, $value:expr, nullable, $wire:literal) => {
        serde::ser::SerializeStruct::serialize_field(&mut $state, $wire, $value)?;
    };
    ($state:ident, $value:expr, optional, $wire:literal) => {
        if !$value.is_absent() {
            serde::ser::SerializeStruct::serialize_field(&mut $state, $wire, $value)?;
        }
    };
    ($state:ident, $value:expr, optional_nullable, $wire:literal) => {
        if !$value.is_absent() {
            serde::ser::SerializeStruct::serialize_field(&mut $state, $wire, $value)?;
        }
    };
}

macro_rules! serialized_field_count {
    ($value:expr, required) => {
        1usize
    };
    ($value:expr, nullable) => {
        1usize
    };
    ($value:expr, optional) => {
        usize::from(!$value.is_absent())
    };
    ($value:expr, optional_nullable) => {
        usize::from(!$value.is_absent())
    };
}

macro_rules! encoding_receipt {
    (required, $wire:literal) => {
        None
    };
    (nullable, $wire:literal) => {
        None
    };
    (optional, $wire:literal) => {
        None
    };
    (optional_nullable($evidence:ty), $wire:literal) => {{
        struct Probe(grapt_schema::OptionalNullable<bool, $evidence>);

        impl serde::Serialize for Probe {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                let field_count = serialized_field_count!(&self.0, optional_nullable);
                let mut state = serde::Serializer::serialize_struct(
                    serializer,
                    "TriStateEncodingProbe",
                    field_count,
                )?;
                serialize_field!(state, &self.0, optional_nullable, $wire);
                serde::ser::SerializeStruct::end(state)
            }
        }

        Some($crate::u3a::dto::TriStateEncodingReceipt {
            row: <$evidence as grapt_schema::TriStateEvidence>::LEDGER_ROW,
            wire: $wire,
            absent: serde_json::to_value(Probe(grapt_schema::OptionalNullable::absent()))?,
            null: serde_json::to_value(Probe(grapt_schema::OptionalNullable::null()))?,
            value: serde_json::to_value(Probe(grapt_schema::OptionalNullable::value(true)))?,
        })
    }};
}

macro_rules! closed_object {
    ($vis:vis struct $name:ident => $schema:literal { }) => {
        #[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
        $vis struct $name {}

        impl $name {
            pub fn new() -> Self {
                Self {}
            }
        }

        impl $crate::u3a::dto::NamedSchema for $name {
            const OPENAPI_NAME: &'static str = $schema;
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct ObjectVisitor;
                impl<'de> serde::de::Visitor<'de> for ObjectVisitor {
                    type Value = $name;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str(concat!("closed object ", $schema))
                    }

                    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
                    where
                        A: serde::de::MapAccess<'de>,
                    {
                        if let Some(key) =
                            serde::de::MapAccess::next_key::<BoundedString<256>>(&mut map)?
                        {
                            return Err(<A::Error as serde::de::Error>::unknown_field(key.as_str(), &[]));
                        }
                        Ok($name {})
                    }
                }
                serde::Deserializer::deserialize_map(deserializer, ObjectVisitor)
            }
        }
    };
    ($vis:vis struct $name:ident => $schema:literal {
        $( $kind:ident $(($evidence:ty))? $field:ident : $ty:ty => $wire:literal ),* $(,)?
    }) => {
        #[derive(Debug, Clone, PartialEq)]
        $vis struct $name {
            $( $field: field_type!($kind $(($evidence))?, $ty), )*
        }

        impl $name {
            #[allow(clippy::too_many_arguments)]
            pub fn new($( $field: field_type!($kind $(($evidence))?, $ty) ),*) -> Self {
                Self { $( $field ),* }
            }

            $( pub fn $field(&self) -> &field_type!($kind $(($evidence))?, $ty) {
                &self.$field
            } )*
        }

        impl $crate::u3a::dto::NamedSchema for $name {
            const OPENAPI_NAME: &'static str = $schema;
            const OPTIONAL_NULLABLE_ROWS: &'static [&'static str] = &[
                $( presence_row!($kind $(($evidence))?), )*
            ];

            fn optional_nullable_encoding_receipts(
            ) -> Result<Vec<$crate::u3a::dto::TriStateEncodingReceipt>, serde_json::Error> {
                let receipts = [$( encoding_receipt!($kind $(($evidence))?, $wire) ),*]
                    .into_iter()
                    .flatten()
                    .collect();
                Ok(receipts)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                let field_count =
                    0usize $( + serialized_field_count!(&self.$field, $kind) )*;
                let mut state = serde::Serializer::serialize_struct(
                    serializer,
                    stringify!($name),
                    field_count,
                )?;
                $( serialize_field!(state, &self.$field, $kind, $wire); )*
                serde::ser::SerializeStruct::end(state)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct ObjectVisitor;
                impl<'de> serde::de::Visitor<'de> for ObjectVisitor {
                    type Value = $name;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str(concat!("closed object ", $schema))
                    }

                    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
                    where
                        A: serde::de::MapAccess<'de>,
                    {
                        $( let mut $field: Option<field_type!($kind $(($evidence))?, $ty)> = None; )*
                        while let Some(key) =
                            serde::de::MapAccess::next_key::<BoundedString<256>>(&mut map)?
                        {
                            match key.as_str() {
                                $(
                                    $wire => {
                                        if $field.replace(
                                            serde::de::MapAccess::next_value(&mut map)?
                                        ).is_some() {
                                            return Err(
                                                <A::Error as serde::de::Error>::duplicate_field($wire)
                                            );
                                        }
                                    }
                                )*
                                unknown => {
                                    return Err(<A::Error as serde::de::Error>::unknown_field(
                                        unknown,
                                        &[$($wire),*],
                                    ));
                                }
                            }
                        }
                        Ok($name {
                            $( $field: finish_field!($kind, $field, $wire, A::Error), )*
                        })
                    }
                }
                serde::Deserializer::deserialize_map(deserializer, ObjectVisitor)
            }
        }
    };
}

macro_rules! string_enum {
    ($vis:vis enum $name:ident { $( $variant:ident => $wire:literal ),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        $vis enum $name {
            $( #[serde(rename = $wire)] $variant, )+
        }
    };
}

macro_rules! named_newtype {
    ($vis:vis struct $name:ident($inner:ty) => $schema:literal) => {
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        $vis struct $name($inner);

        impl $name {
            pub fn new(value: $inner) -> Self {
                Self(value)
            }

            pub fn as_inner(&self) -> &$inner {
                &self.0
            }

            pub fn into_inner(self) -> $inner {
                self.0
            }
        }

        impl $crate::u3a::dto::NamedSchema for $name {
            const OPENAPI_NAME: &'static str = $schema;
        }
    };
}

macro_rules! bounded_id {
    ($vis:vis struct $name:ident) => {
        bounded_id!(@core $vis struct $name, |_| true);
    };
    ($vis:vis struct $name:ident => $prefix:literal) => {
        bounded_id!(@core $vis struct $name, |value: &str| value.starts_with($prefix));
    };
    (@core $vis:vis struct $name:ident, $valid:expr) => {
        #[derive(
            Debug,
            Clone,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            serde::Serialize,
        )]
        #[serde(transparent)]
        $vis struct $name(BoundedString<256>);

        impl $name {
            pub fn try_new(value: impl Into<String>) -> Result<Self, &'static str> {
                let value = BoundedString::try_new(value)?;
                if !($valid)(value.as_str()) {
                    return Err("identity does not match its frozen pattern");
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = <BoundedString<256> as serde::Deserialize>::deserialize(deserializer)?;
                Self::try_new(value.into_string())
                    .map_err(<D::Error as serde::de::Error>::custom)
            }
        }
    };
}

macro_rules! tagged_union {
    (
        $vis:vis enum $name:ident => $schema:literal, tag $tag:literal {
            $( $variant:ident($arm:ty) => $wire:literal ),+ $(,)?
        }
    ) => {
        tagged_union! {
            @core $vis enum $name, tag $tag {
                $( $variant($arm) => $wire ),+
            }
        }
        impl $crate::u3a::dto::NamedSchema for $name {
            const OPENAPI_NAME: &'static str = $schema;
        }
    };
    (
        $vis:vis enum $name:ident, tag $tag:literal {
            $( $variant:ident($arm:ty) => $wire:literal ),+ $(,)?
        }
    ) => {
        tagged_union! {
            @core $vis enum $name, tag $tag {
                $( $variant($arm) => $wire ),+
            }
        }
    };
    (
        @core $vis:vis enum $name:ident, tag $tag:literal {
            $( $variant:ident($arm:ty) => $wire:literal ),+ $(,)?
        }
    ) => {
        #[derive(Debug, Clone, PartialEq)]
        $vis enum $name {
            $( $variant(Box<$arm>), )+
        }

        impl $name {
            pub const VARIANT_COUNT: usize = [$(stringify!($variant)),+].len();

            pub const fn variant_name(&self) -> &'static str {
                match self {
                    $( Self::$variant(_) => $wire, )+
                }
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                match self {
                    $( Self::$variant(value) => serde::Serialize::serialize(value, serializer), )+
                }
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct TaggedVisitor;
                impl<'de> serde::de::Visitor<'de> for TaggedVisitor {
                    type Value = $name;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str(concat!("tag-first union ", stringify!($name)))
                    }

                    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
                    where
                        A: serde::de::MapAccess<'de>,
                    {
                        let Some(first) =
                            serde::de::MapAccess::next_key::<BoundedString<256>>(&mut map)?
                        else {
                            return Err(<A::Error as serde::de::Error>::missing_field($tag));
                        };
                        if first.as_str() != $tag {
                            return Err(<A::Error as serde::de::Error>::unknown_field(
                                first.as_str(),
                                &[$tag],
                            ));
                        }
                        let tag_value: DtoString =
                            serde::de::MapAccess::next_value(&mut map)?;
                        let replay_value = tag_value.as_str().to_owned();
                        match tag_value.as_str() {
                            $(
                                $wire => {
                                    let replay = $crate::u3a::dto::ReplayMap::new(
                                        [$tag],
                                        [replay_value],
                                        map,
                                    );
                                    let input = serde::de::value::MapAccessDeserializer::new(replay);
                                    serde::Deserialize::deserialize(input)
                                        .map(Box::new)
                                        .map($name::$variant)
                                }
                            )+
                            unknown => Err(<A::Error as serde::de::Error>::unknown_variant(
                                unknown,
                                &[$($wire),+],
                            )),
                        }
                    }
                }
                serde::Deserializer::deserialize_map(deserializer, TaggedVisitor)
            }
        }
    };
}

pub(crate) use {
    bounded_id, closed_object, encoding_receipt, field_type, finish_field, named_newtype,
    presence_row, serialize_field, serialized_field_count, string_enum, tagged_union,
};
