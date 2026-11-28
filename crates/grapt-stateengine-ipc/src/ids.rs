// SPDX-License-Identifier: MIT OR Apache-2.0
//! The owning identifier types of the public API, as the plain strings the
//! wire carries. Use with `#[serde(with = "crate::ids")]`, or
//! `crate::ids::option` for an optional identifier.
use grapt_stateengine_api::{AgentId, EventId, IdempotencyKey, ProjectId, SessionId};
use serde::{Deserialize, Deserializer, Serializer};

pub(crate) trait WireId: Sized {
    fn text(&self) -> &str;
    fn from_text(text: String) -> Self;
}

macro_rules! wire_id {
    ($($id:ty),+) => {$(
        impl WireId for $id {
            fn text(&self) -> &str {
                &self.0
            }
            fn from_text(text: String) -> Self {
                Self(text)
            }
        }
    )+};
}

wire_id!(AgentId, EventId, IdempotencyKey, ProjectId, SessionId);

pub(crate) fn serialize<T: WireId, S: Serializer>(
    id: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(id.text())
}

pub(crate) fn deserialize<'de, T: WireId, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    String::deserialize(deserializer).map(T::from_text)
}

pub(crate) mod option {
    use super::WireId;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<T: WireId, S: Serializer>(
        id: &Option<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match id {
            Some(id) => serializer.serialize_some(id.text()),
            None => serializer.serialize_none(),
        }
    }

    pub(crate) fn deserialize<'de, T: WireId, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        Option::<String>::deserialize(deserializer).map(|text| text.map(T::from_text))
    }
}
