//! Secret material handling.
//!
//! Master §8.4: "There is no public raw SHA-256 secret fingerprint or other
//! stable offline equality oracle. Authority graphs name a credential
//! record/version or a keyed opaque identity, never the raw-secret digest."
//!
//! This type previously exposed `fingerprint()`, a public raw SHA-256 of the
//! material, and derived `PartialEq`/`Eq`. Together those were exactly the
//! offline oracle the law forbids: anyone holding a candidate secret could
//! digest it and compare, or compare two `Secret`s directly. Both are gone.
//!
//! `Secret` therefore:
//!
//! - has a `Debug` that redacts the material (no sentinel leaks through a
//!   `{:?}` in a panic or trace),
//! - is neither `Display` nor serializable,
//! - has NO public digest, and NO `PartialEq`/`Eq`,
//! - exposes its bytes only through one named read path for the code that
//!   must actually send the credential.
//!
//! The effective graph names the owning [`grapt_types::CredentialRecordId`]
//! and [`grapt_types::CredentialVersion`] instead. Rotation advances the
//! version, which is what a graph receipt needs to observe; it never needs to
//! observe the material.

/// Opaque secret material.
#[derive(Clone)]
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(material: impl Into<Vec<u8>>) -> Self {
        Self(material.into())
    }

    /// The one named read path, for the transport that must actually present
    /// the credential. Deliberately not `Deref` and not `AsRef`, so a borrow
    /// cannot happen implicitly at an unintended call site.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No length, no prefix — nothing that narrows the material.
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_material_or_its_length() {
        let secret = Secret::new(b"api-key-material".to_vec());
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "Secret(<redacted>)");
        assert!(!rendered.contains("api-key"));
        assert!(!rendered.contains("16"));
    }

    #[test]
    fn the_named_read_path_is_the_only_route_to_material() {
        let secret = Secret::new(b"api-key-material".to_vec());
        assert_eq!(secret.expose(), b"api-key-material");
        // There is no `fingerprint()` and no `PartialEq`: a caller holding a
        // candidate secret cannot confirm a guess offline.
    }
}
