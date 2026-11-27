//! Credential and account identity. Law (foundational report row
//! `path`/`hook`/`credential`; supersession matrix "Credentials" row): "auth is
//! not process-global"; credentials are typed per-attempt with a versioned
//! storage envelope and typed account ownership, and the raw secret material
//! never appears here — only in the unprintable `Secret<T>` boundary and the
//! secure backend (later).
//!
//! Scope note: this module lands the account/credential IDENTITIES F1A owns —
//! including [`AccountId`], the first-class routing dimension a
//! [`crate::model::ResolvedModelTarget`] binds — plus the
//! [`CredentialStorageMode`] vocabulary the versioned envelope enumerates. The
//! secure-backend implementation and platform ACL work remain later (survey
//! row C12).

crate::id::opaque_id! {
    /// Typed account ownership. Account is a first-class routing dimension for
    /// campaign residency and rate-limit recovery; it is not a provider or model
    /// identity and cannot manufacture their authority.
    AccountId
}

crate::id::opaque_id! {
    /// A stable credential record identity. The record is a versioned envelope;
    /// the raw secret is never this identity.
    CredentialRecordId
}

crate::id::opaque_id! {
    /// The version of a stored credential envelope. Rotation advances it; a stale
    /// version is a distinguishable fact, not a silent reuse.
    CredentialVersion
}

/// Where a credential envelope is stored.
///
/// Law C12: the storage mode is declared now so an authority graph can name
/// how a credential is held without ever naming the secret. `PlaintextFile` is
/// representable because it exists in the field and must be *visible* as a
/// weaker posture — not because it is endorsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialStorageMode {
    /// An OS keychain or equivalent platform secret store.
    PlatformSecureStore,
    /// An encrypted file under a key held elsewhere.
    EncryptedFile,
    /// A plaintext file. Visible as a weaker posture, never a default.
    PlaintextFile,
    /// Supplied by the environment for one process lifetime.
    ProcessEnvironment,
}

impl CredentialStorageMode {
    /// Whether the mode protects the secret at rest.
    pub fn protects_at_rest(self) -> bool {
        matches!(
            self,
            CredentialStorageMode::PlatformSecureStore | CredentialStorageMode::EncryptedFile
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_identities_are_distinct_non_empty() {
        assert!(AccountId::new("acct_1").is_some());
        assert!(CredentialRecordId::new("cred_1").is_some());
        assert!(CredentialVersion::new("1").is_some());
        assert!(AccountId::new("").is_none());
    }

    #[test]
    fn plaintext_storage_is_representable_but_never_protected() {
        assert!(!CredentialStorageMode::PlaintextFile.protects_at_rest());
        assert!(!CredentialStorageMode::ProcessEnvironment.protects_at_rest());
        assert!(CredentialStorageMode::PlatformSecureStore.protects_at_rest());
        assert!(CredentialStorageMode::EncryptedFile.protects_at_rest());
    }
}
