// SPDX-License-Identifier: MIT OR Apache-2.0
//! The checks every frame passes before its schema is read: nesting within
//! [`crate::MAX_JSON_DEPTH`] and no key repeated in any object, including
//! inside the free-form payloads a schema leaves open.
use crate::{IpcError, MAX_JSON_DEPTH};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;

enum Violation {
    TooDeep,
    DuplicateKey(String),
}

pub(crate) fn check(bytes: &[u8]) -> Result<(), IpcError> {
    let found = RefCell::new(None);
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let walked = Walk {
        depth: 0,
        found: &found,
    }
    .deserialize(&mut deserializer)
    .and_then(|()| deserializer.end());
    match (walked, found.into_inner()) {
        (Ok(()), _) => Ok(()),
        (Err(_), Some(Violation::TooDeep)) => Err(IpcError::TooDeep),
        (Err(_), Some(Violation::DuplicateKey(key))) => Err(IpcError::DuplicateKey(key)),
        (Err(error), None) => Err(IpcError::Malformed(error.to_string())),
    }
}

#[derive(Clone, Copy)]
struct Walk<'a> {
    depth: usize,
    found: &'a RefCell<Option<Violation>>,
}

impl Walk<'_> {
    fn deeper<E: de::Error>(self) -> Result<Self, E> {
        if self.depth >= MAX_JSON_DEPTH {
            *self.found.borrow_mut() = Some(Violation::TooDeep);
            return Err(E::custom("too deep"));
        }
        Ok(Self {
            depth: self.depth + 1,
            found: self.found,
        })
    }
}

impl<'de> DeserializeSeed<'de> for Walk<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Walk<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON")
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let inner = self.deeper()?;
        while seq.next_element_seed(inner)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let inner = self.deeper()?;
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                *self.found.borrow_mut() = Some(Violation::DuplicateKey(key));
                return Err(de::Error::custom("duplicate key"));
            }
            map.next_value_seed(inner)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_and_repeated_keys_refuse_at_any_level() {
        let deep = |levels: usize| "[".repeat(levels) + &"]".repeat(levels);
        assert!(check(deep(MAX_JSON_DEPTH).as_bytes()).is_ok());
        assert!(matches!(
            check(deep(MAX_JSON_DEPTH + 1).as_bytes()),
            Err(IpcError::TooDeep)
        ));
        assert!(matches!(
            check(br#"{"a":{"b":1,"b":2}}"#),
            Err(IpcError::DuplicateKey(key)) if key == "b"
        ));
        assert!(matches!(check(b"{} {}"), Err(IpcError::Malformed(_))));
    }
}
