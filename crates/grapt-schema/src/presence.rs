//! The four ratified wire-presence categories.
//!
//! Every field is classified before its codec lands:
//!
//! | Category | Missing | Explicit `null` | Value |
//! | --- | --- | --- | --- |
//! | [`Required`] | reject | reject | validate `T` |
//! | [`Optional`] | `Absent` | reject | validate `T` |
//! | [`Nullable`] | reject | `Null` | validate `T` |
//! | [`OptionalNullable`] | `Absent` | `Null` | validate `T` |
//!
//! Each category holds its presence state privately and admits domain values
//! only through its own constructor, so a decoded state can never be forged by
//! tuple construction, a derived deserializer, or a domain default.
//!
//! ## `serde(default)` is structurally impossible
//!
//! The ratified codec section forbids `serde(default)`, and serde surfaces an
//! omitted field only through a field default. No category therefore
//! implements [`Default`], which makes `#[serde(default)]` on any of them a
//! compile error rather than a convention.
//!
//! A container with an absent-capable field must instead decode by walking its
//! wire map, observing which keys were actually present. Absence is then a fact
//! the container read, never a value serde manufactured.
//!
//! ## Canonical serialization
//!
//! Serialization emits `null` only for the two nullable categories and omits
//! only an absent state. Because serde can omit a field only through
//! `#[serde(skip_serializing_if = "…")]`, serializing an absent value directly
//! is a loud error rather than a silent `null`; use [`Optional::is_absent`] or
//! [`OptionalNullable::is_absent`] as the skip predicate.

use serde::de::{Error as DeError, Visitor};
use serde::ser::Error as SerError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

const NULL_FORBIDDEN: &str = "explicit null is forbidden at this field category";
const ABSENT_MUST_BE_OMITTED: &str =
    "an absent field must be omitted with serde(skip_serializing_if), never serialized";

/// A field that rejects both omission and explicit JSON `null`.
///
/// The inner value is private, so a decoded field cannot be forged:
///
/// ```compile_fail
/// use grapt_schema::Required;
/// let _ = Required("bypass");
/// ```
///
/// `Required` has no [`Default`], so a missing field cannot be repaired:
///
/// ```compile_fail
/// use grapt_schema::Required;
/// let _: Required<bool> = Default::default();
/// ```
///
/// `#[serde(default)]` therefore cannot be applied to a required field:
///
/// ```compile_fail
/// use grapt_schema::Required;
/// use serde::Deserialize;
/// #[derive(Deserialize)]
/// struct Document {
///     #[serde(default)]
///     required: Required<bool>,
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Required<T>(T);

impl<T> Required<T> {
    /// Wraps an already-validated domain value as a required field.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Borrows the required value.
    pub fn get(&self) -> &T {
        &self.0
    }

    /// Consumes the field and returns the required value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<'de, T> Deserialize<'de> for Required<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer
            .deserialize_newtype_struct(CATEGORY_NEWTYPE, RejectNullField(PhantomData))
            .map(Self)
    }
}

impl<T> Serialize for Required<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

/// A field that may be omitted but rejects explicit JSON `null`.
///
/// The presence state is private, so a present-null state cannot be forged:
///
/// ```compile_fail
/// use grapt_schema::Optional;
/// let _ = Optional(Some("bypass"));
/// ```
///
/// No category implements [`Default`], so the ratified prohibition on
/// `serde(default)` is structural rather than a convention:
///
/// ```compile_fail
/// use grapt_schema::Optional;
/// use serde::Deserialize;
/// #[derive(Deserialize)]
/// struct Document {
///     #[serde(default)]
///     optional: Optional<bool>,
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Optional<T>(Option<T>);

impl<T> Optional<T> {
    /// Constructs the absent presence state.
    pub fn absent() -> Self {
        Self(None)
    }

    /// Wraps an already-validated domain value as a present field.
    pub fn present(value: T) -> Self {
        Self(Some(value))
    }

    /// Reports whether the containing object omitted the field.
    ///
    /// This is the canonical `serde(skip_serializing_if)` predicate.
    pub fn is_absent(&self) -> bool {
        self.0.is_none()
    }

    /// Borrows the present value, or returns `None` for omission.
    pub fn as_present(&self) -> Option<&T> {
        self.0.as_ref()
    }

    /// Consumes the field and returns the present value, if any.
    pub fn into_present(self) -> Option<T> {
        self.0
    }
}

impl<'de, T> Deserialize<'de> for Optional<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer
            .deserialize_newtype_struct(CATEGORY_NEWTYPE, RejectNullField(PhantomData))
            .map(|value| Self(Some(value)))
    }
}

impl<T> Serialize for Optional<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.0 {
            Some(value) => value.serialize(serializer),
            None => Err(S::Error::custom(ABSENT_MUST_BE_OMITTED)),
        }
    }
}

/// A field that requires presence but accepts explicit JSON `null`.
///
/// The presence state is private, so a null state cannot be forged:
///
/// ```compile_fail
/// use grapt_schema::Nullable;
/// let _ = Nullable(None::<&str>);
/// ```
///
/// `Nullable` has no [`Default`], so a missing field cannot become `null`:
///
/// ```compile_fail
/// use grapt_schema::Nullable;
/// let _: Nullable<bool> = Default::default();
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nullable<T>(Option<T>);

impl<T> Nullable<T> {
    /// Constructs the explicit-null presence state.
    pub fn null() -> Self {
        Self(None)
    }

    /// Wraps an already-validated domain value as a present field.
    pub fn value(value: T) -> Self {
        Self(Some(value))
    }

    /// Reports whether the containing object supplied explicit `null`.
    pub fn is_null(&self) -> bool {
        self.0.is_none()
    }

    /// Borrows the present value, or returns `None` for explicit `null`.
    pub fn as_value(&self) -> Option<&T> {
        self.0.as_ref()
    }

    /// Consumes the field and returns the present value, if any.
    pub fn into_value(self) -> Option<T> {
        self.0
    }
}

impl<'de, T> Deserialize<'de> for Nullable<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_newtype_struct(CATEGORY_NEWTYPE, AcceptNullField(PhantomData))
    }
}

impl<T> Serialize for Nullable<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.0 {
            Some(value) => value.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }
}

/// Names the compatibility-ledger row that proves a field is genuinely
/// tri-state on the wire.
///
/// [`OptionalNullable`] is admitted only for an oracle-proven field, so every
/// use site must name its evidence. The declaring crate owns the marker type.
pub trait TriStateEvidence: sealed::Sealed {
    /// The exact frozen-contract-proven compatibility row distinguishing
    /// missing, `null` and value for this field.
    const LEDGER_ROW: &'static str;
}

mod sealed {
    /// Confines evidence to [`crate::tri_state_ledger`]. A downstream crate
    /// cannot declare that its own field is oracle-proven.
    pub trait Sealed {}
}

/// The closed ledger of fields the frozen oracle proves are genuinely
/// tri-state.
///
/// A row is added here only with its oracle evidence, and only by this crate,
/// so [`OptionalNullable`] can never be reached by self-attestation.
///
/// Frozen-contract-proven exact Rust-owned field carriers; no blanket marker.
pub mod tri_state_ledger {
    macro_rules! u3a_rows {
        ($( $marker:ident => $row:literal ),+ $(,)?) => {
            $(
                #[doc = concat!("Evidence marker for `", $row, "`.")]
                #[derive(Debug, Clone, Copy, PartialEq, Eq)]
                pub struct $marker;

                impl super::sealed::Sealed for $marker {}

                impl super::TriStateEvidence for $marker {
                    const LEDGER_ROW: &'static str = $row;
                }
            )+

            /// Exact frozen-contract inventory consumed by U3a-A1.
            ///
            /// The compatibility contract requires one unique row per proven
            /// tri-state carrier; this is not an empirical runtime census.
            pub const U3A_OPTIONAL_NULLABLE_LEDGER: &[&str] = &[$($row),+];
        };
    }

    u3a_rows! {
        U3aConflictErrorResource => "schema:ConflictError.resource",
        U3aInvalidRequestErrorKind => "schema:InvalidRequestError.kind",
        U3aInvalidRequestErrorField => "schema:InvalidRequestError.field",
        U3aInvalidRequestError1Kind => "schema:InvalidRequestError1.kind",
        U3aInvalidRequestError1Field => "schema:InvalidRequestError1.field",
        U3aServiceUnavailableErrorService => "schema:ServiceUnavailableError.service",
        U3aSessionActiveExecutionId => "schema:SessionActive.executionID",
        U3aSessionMessagesCursorPrevious => "schema:SessionMessagesResponse.cursor.previous",
        U3aSessionMessagesCursorNext => "schema:SessionMessagesResponse.cursor.next",
        U3aSessionsCursorPrevious => "schema:SessionsResponse.cursor.previous",
        U3aSessionsCursorNext => "schema:SessionsResponse.cursor.next",
        U3aToolFileContentName => "schema:Tool.FileContent.name",
        U3aUnknownErrorRef => "schema:UnknownError.ref",
        U3aProjectCurrentLocation => "operation:GET /api/project/current.query.location",
        U3aProjectCurrentLocationDirectory => "operation:GET /api/project/current.query.location.directory",
        U3aProjectCurrentLocationWorkspace => "operation:GET /api/project/current.query.location.workspace",
        U3aProviderLocation => "operation:GET /api/provider.query.location",
        U3aProviderLocationDirectory => "operation:GET /api/provider.query.location.directory",
        U3aProviderLocationWorkspace => "operation:GET /api/provider.query.location.workspace",
        U3aModelLocation => "operation:GET /api/model.query.location",
        U3aModelLocationDirectory => "operation:GET /api/model.query.location.directory",
        U3aModelLocationWorkspace => "operation:GET /api/model.query.location.workspace",
        U3aModelDefaultLocation => "operation:GET /api/model/default.query.location",
        U3aModelDefaultLocationDirectory => "operation:GET /api/model/default.query.location.directory",
        U3aModelDefaultLocationWorkspace => "operation:GET /api/model/default.query.location.workspace",
        U3aAgentLocation => "operation:GET /api/agent.query.location",
        U3aAgentLocationDirectory => "operation:GET /api/agent.query.location.directory",
        U3aAgentLocationWorkspace => "operation:GET /api/agent.query.location.workspace",
        U3aSessionListWorkspace => "operation:GET /api/session.query.workspace",
        U3aSessionListLimit => "operation:GET /api/session.query.limit",
        U3aSessionListOrder => "operation:GET /api/session.query.order",
        U3aSessionListSearch => "operation:GET /api/session.query.search",
        U3aSessionListParentId => "operation:GET /api/session.query.parentID",
        U3aSessionListDirectory => "operation:GET /api/session.query.directory",
        U3aSessionListProject => "operation:GET /api/session.query.project",
        U3aSessionListSubpath => "operation:GET /api/session.query.subpath",
        U3aSessionListCursor => "operation:GET /api/session.query.cursor",
        U3aMessageListLimit => "operation:GET /api/session/{sessionID}/message.query.limit",
        U3aMessageListOrder => "operation:GET /api/session/{sessionID}/message.query.order",
        U3aMessageListCursor => "operation:GET /api/session/{sessionID}/message.query.cursor",
        U3aSessionCreateId => "operation:POST /api/session.body.id",
        U3aSessionCreateAgent => "operation:POST /api/session.body.agent",
        U3aSessionCreateModel => "operation:POST /api/session.body.model",
        U3aSessionCreateLocation => "operation:POST /api/session.body.location",
        U3aPromptId => "operation:POST /api/session/{sessionID}/prompt.body.id",
        U3aPromptDelivery => "operation:POST /api/session/{sessionID}/prompt.body.delivery",
        U3aPromptResume => "operation:POST /api/session/{sessionID}/prompt.body.resume",
        U3aInterruptExecutionId => "operation:POST /api/session/{sessionID}/interrupt.body.executionID",
    }
}

/// The private marker this crate's own unit tests decode against.
///
/// It is never exported, so it cannot become a production field's evidence.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnitTestTriState;

#[cfg(test)]
impl sealed::Sealed for UnitTestTriState {}

#[cfg(test)]
impl TriStateEvidence for UnitTestTriState {
    const LEDGER_ROW: &'static str = "unit-test marker, not an oracle row";
}

/// A field that preserves all three wire states.
///
/// This category is admitted only for an oracle-proven tri-state field, so it
/// carries a [`TriStateEvidence`] marker naming that evidence, and evidence is
/// sealed to [`tri_state_ledger`].
///
/// Two separate laws guard it, each with its own right-reason probe under
/// `tests/probes`: a consumer cannot attest its own field is oracle-proven
/// (`03_tri_state_evidence_is_sealed`), and a presence state cannot be forged
/// by tuple construction (`04_category_tuple_state_is_private`). They are kept
/// apart so neither failure can mask the other.
pub struct OptionalNullable<T, E: TriStateEvidence>(TriState<T>, PhantomData<fn() -> E>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum TriState<T> {
    Absent,
    Null,
    Value(T),
}

impl<T, E: TriStateEvidence> OptionalNullable<T, E> {
    /// Constructs the absent presence state.
    pub fn absent() -> Self {
        Self(TriState::Absent, PhantomData)
    }

    /// Constructs the explicit-null presence state.
    pub fn null() -> Self {
        Self(TriState::Null, PhantomData)
    }

    /// Wraps an already-validated domain value as a present field.
    pub fn value(value: T) -> Self {
        Self(TriState::Value(value), PhantomData)
    }

    /// Reports whether the containing object omitted the field.
    ///
    /// This is the canonical `serde(skip_serializing_if)` predicate.
    pub fn is_absent(&self) -> bool {
        matches!(self.0, TriState::Absent)
    }

    /// Reports whether the containing object supplied explicit `null`.
    pub fn is_null(&self) -> bool {
        matches!(self.0, TriState::Null)
    }

    /// Borrows the present value, or returns `None` for absence or `null`.
    pub fn as_value(&self) -> Option<&T> {
        match &self.0 {
            TriState::Value(value) => Some(value),
            TriState::Absent | TriState::Null => None,
        }
    }

    /// Returns the ledger row proving this field's tri-state contract.
    pub fn ledger_row() -> &'static str {
        E::LEDGER_ROW
    }
}

impl<T: fmt::Debug, E: TriStateEvidence> fmt::Debug for OptionalNullable<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OptionalNullable").field(&self.0).finish()
    }
}

impl<T: Clone, E: TriStateEvidence> Clone for OptionalNullable<T, E> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
}

impl<T: PartialEq, E: TriStateEvidence> PartialEq for OptionalNullable<T, E> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<T: Eq, E: TriStateEvidence> Eq for OptionalNullable<T, E> {}

impl<'de, T, E> Deserialize<'de> for OptionalNullable<T, E>
where
    T: Deserialize<'de>,
    E: TriStateEvidence,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer
            .deserialize_newtype_struct(CATEGORY_NEWTYPE, AcceptNullField(PhantomData))
            .map(|nullable: Nullable<T>| match nullable.into_value() {
                Some(value) => Self::value(value),
                None => Self::null(),
            })
    }
}

impl<T, E> Serialize for OptionalNullable<T, E>
where
    T: Serialize,
    E: TriStateEvidence,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.0 {
            TriState::Value(value) => value.serialize(serializer),
            TriState::Null => serializer.serialize_none(),
            TriState::Absent => Err(S::Error::custom(ABSENT_MUST_BE_OMITTED)),
        }
    }
}

/// Name used for the outer newtype-struct stage of every category.
///
/// serde reports a missing struct field by re-running the field type's
/// `Deserialize` against a deserializer that answers `deserialize_option` with
/// `visit_none` and every other method with "missing field". A category that
/// asked for an option directly would therefore see omission and explicit
/// `null` as the same call, and could not reject one while accepting the other.
///
/// Every category instead asks for a newtype struct first. Omission fails there
/// with serde's own missing-field error and its exact path, while a real JSON
/// deserializer hands back the underlying deserializer, which the second stage
/// asks for an option to tell `null` from a value.
const CATEGORY_NEWTYPE: &str = "GraptFieldCategory";

/// Outer stage for a category that refuses explicit `null`.
struct RejectNullField<T>(PhantomData<T>);

impl<'de, T> Visitor<'de> for RejectNullField<T>
where
    T: Deserialize<'de>,
{
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a present non-null value")
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_option(RejectNullVisitor(PhantomData))
    }
}

/// Outer stage for a category that accepts explicit `null`.
struct AcceptNullField<T>(PhantomData<T>);

impl<'de, T> Visitor<'de> for AcceptNullField<T>
where
    T: Deserialize<'de>,
{
    type Value = Nullable<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a present value or explicit null")
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_option(NullableVisitor(PhantomData))
    }
}

/// Accepts one present, non-null value and refuses explicit `null`.
struct RejectNullVisitor<T>(PhantomData<T>);

impl<'de, T> Visitor<'de> for RejectNullVisitor<T>
where
    T: Deserialize<'de>,
{
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a present non-null value")
    }

    fn visit_none<Err>(self) -> Result<Self::Value, Err>
    where
        Err: DeError,
    {
        Err(Err::custom(NULL_FORBIDDEN))
    }

    fn visit_unit<Err>(self) -> Result<Self::Value, Err>
    where
        Err: DeError,
    {
        self.visit_none()
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer)
    }
}

struct NullableVisitor<T>(PhantomData<T>);

impl<'de, T> Visitor<'de> for NullableVisitor<T>
where
    T: Deserialize<'de>,
{
    type Value = Nullable<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a value or explicit null")
    }

    fn visit_none<Err>(self) -> Result<Self::Value, Err>
    where
        Err: DeError,
    {
        Ok(Nullable::null())
    }

    fn visit_unit<Err>(self) -> Result<Self::Value, Err>
    where
        Err: DeError,
    {
        self.visit_none()
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Nullable::value)
    }
}

/// Deserializes a string through a domain type's [`FromStr`] implementation.
///
/// Use this with `#[serde(deserialize_with = "deserialize_from_str")]` for an
/// opaque newtype that owns its parser. Parse failure text remains inside serde
/// and is discarded by [`crate::decode_json`]'s redacted public error.
pub fn deserialize_from_str<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: fmt::Display,
{
    String::deserialize(deserializer)?
        .parse()
        .map_err(DeError::custom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_json, DecodeLimits};
    use serde::de::MapAccess;

    /// A container exercising the tri-state category against the private
    /// unit-test marker, so no production export is needed to witness it.
    #[derive(Debug, Serialize, PartialEq, Eq)]
    struct TriStateDocument {
        #[serde(skip_serializing_if = "OptionalNullable::is_absent")]
        tri_state: OptionalNullable<String, UnitTestTriState>,
    }

    impl<'de> Deserialize<'de> for TriStateDocument {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_map(TriStateVisitor)
        }
    }

    struct TriStateVisitor;

    impl<'de> Visitor<'de> for TriStateVisitor {
        type Value = TriStateDocument;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a tri-state document object")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut tri_state = None;
            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "tri_state" => {
                        if tri_state.replace(map.next_value()?).is_some() {
                            return Err(A::Error::custom("duplicate field `tri_state`"));
                        }
                    }
                    unknown => return Err(A::Error::unknown_field(unknown, &["tri_state"])),
                }
            }
            Ok(TriStateDocument {
                tri_state: tri_state.unwrap_or_else(OptionalNullable::absent),
            })
        }
    }

    fn limits() -> DecodeLimits {
        DecodeLimits::new(1_024, 8).unwrap_or_else(|error| panic!("test limits: {error}"))
    }

    fn decode(input: &[u8]) -> TriStateDocument {
        decode_json::<TriStateDocument>(input, limits())
            .unwrap_or_else(|error| panic!("tri-state document should decode: {error}"))
    }

    #[test]
    fn optional_nullable_keeps_absent_null_and_value_distinct() {
        let absent = decode(br#"{}"#);
        assert!(absent.tri_state.is_absent());
        assert!(!absent.tri_state.is_null());

        let null = decode(br#"{"tri_state":null}"#);
        assert!(null.tri_state.is_null());
        assert!(!null.tri_state.is_absent());

        let value = decode(br#"{"tri_state":"t"}"#);
        assert_eq!(value.tri_state.as_value().map(String::as_str), Some("t"));
        assert!(!value.tri_state.is_absent());
        assert!(!value.tri_state.is_null());
    }

    #[test]
    fn optional_nullable_serializes_null_but_omits_absent() {
        let null = decode(br#"{"tri_state":null}"#);
        let encoded = serde_json::to_vec(&null)
            .unwrap_or_else(|error| panic!("null tri-state encodes: {error}"));
        assert_eq!(encoded, br#"{"tri_state":null}"#);

        let absent = decode(br#"{}"#);
        let encoded = serde_json::to_vec(&absent)
            .unwrap_or_else(|error| panic!("absent tri-state encodes: {error}"));
        assert_eq!(encoded, br#"{}"#);
    }

    #[test]
    fn the_category_names_the_ledger_row_it_was_admitted_under() {
        assert_eq!(
            OptionalNullable::<String, UnitTestTriState>::ledger_row(),
            UnitTestTriState::LEDGER_ROW
        );
    }
}
