//! Opaque read-only views over the open metadata value surface.
//!
//! Split from `codec.rs` when that file passed this workspace's module
//! ceiling. The decoded value types stay private: these views borrow the
//! strings the codec already bounded, construct nothing, and expose no
//! recursive public shape.
use super::{MetadataKey, OpenObject, OpenValue};

/// A read-only view into one decoded open value. Opaque on purpose: the
/// value's type and every bound were enforced at decode, so a consumer that
/// renders or classifies the metadata only ever borrows what stands.
#[derive(Debug)]
pub struct OpenValueView<'a> {
    value: &'a OpenValue,
}

impl<'a> OpenValueView<'a> {
    pub fn as_str(&self) -> Option<&'a str> {
        match self.value {
            OpenValue::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<impl Iterator<Item = OpenValueView<'a>>> {
        match self.value {
            OpenValue::Array(values) => Some(values.iter().map(|value| OpenValueView { value })),
            _ => None,
        }
    }

    /// A nested entry by key when this value is an object; the object form
    /// of [`OpenObject::get`] for metadata nested one level down.
    pub fn entry(&self, key: &str) -> Option<OpenValueView<'a>> {
        match self.value {
            OpenValue::Object(values) => {
                let key = MetadataKey::try_new(key).ok()?;
                values.get(&key).map(|value| OpenValueView { value })
            }
            _ => None,
        }
    }
}

impl OpenObject {
    /// Read-only lookup for a consumer that renders or classifies one
    /// metadata entry. Decoding keeps enforcing every bound; this accessor
    /// adds no wire shape, no construction and no new validation.
    pub fn get(&self, key: &str) -> Option<OpenValueView<'_>> {
        let key = MetadataKey::try_new(key).ok()?;
        self.0.get(&key).map(|value| OpenValueView { value })
    }
}
