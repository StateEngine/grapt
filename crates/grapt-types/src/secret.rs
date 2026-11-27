//! Secret material boundary. Law (foundational report rows `credential`/`path`;
//! Agency/Continuity Amendment §8.4; supersession matrix "Secrets" row): "Raw
//! secret material is held only in an unprintable/non-serializable `Secret<T>`
//! boundary. There is no public raw SHA-256 secret fingerprint or other stable
//! offline equality oracle." "Secret equality is never a public digest."
//!
//! [`Secret<T>`] therefore exposes NO public `Display`, `Serialize`, `Hash`,
//! `PartialEq`/`Eq`, or `Ord` — those would each leak material or provide an
//! offline-guess oracle. Its `Debug` redacts. The ONLY way to read the material
//! is the explicitly-named [`Secret::expose`], so every access is greppable.
//! Compile-fail probe #9 proves formatting, serializing, or publicly hashing the
//! raw material is rejected by the compiler.

use std::fmt;

/// A holder for secret material. Unprintable and non-serializable by
/// construction: it derives nothing, so there is no accidental `Debug` leak, no
/// serde codec, and no `Hash`/`PartialEq` equality oracle. Reading the material
/// requires the explicit [`Secret::expose`].
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wrap secret material. The value is sealed until an explicit [`expose`].
    ///
    /// [`expose`]: Secret::expose
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The ONE explicit read path. Named so every access to raw material is
    /// auditable by grep; there is no `Deref`, `Display`, or codec that would
    /// expose it implicitly.
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Consume the boundary and take the material out — for handing it to a
    /// secure backend, never for logging or comparison.
    pub fn into_inner(self) -> T {
        self.0
    }
}

/// Redacts. A secret is never rendered, even in a debug log or panic message.
impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::Secret;

    #[test]
    fn secret_redacts_in_debug_and_exposes_only_explicitly() {
        let secret = Secret::new(String::from("hunter2"));
        // Debug never prints the material.
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        // The only read path is the explicit expose.
        assert_eq!(secret.expose(), "hunter2");
        assert_eq!(secret.into_inner(), "hunter2");
    }
}
