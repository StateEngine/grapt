//! A credential owner's opaque, bounded renewal port. It grants no route.
use std::future::Future;
use std::pin::Pin;

use super::ProviderCredential;

/// Volatile revision within one owner, used only to recognize stale rejection
/// responses. It is neither a credential version nor a secret fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CredentialRevision(u64);

impl CredentialRevision {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CredentialRenewalRefusal {
    #[error("provider credential renewal is unavailable")]
    Unavailable,
    #[error("the provider credential source is absent")]
    SourceAbsent,
    #[error("the provider credential source is unreadable")]
    SourceUnreadable,
    #[error("the provider credential source is invalid or exceeds its bound")]
    SourceInvalid,
    #[error("the provider credential source changed while it was read")]
    SourceChanged,
    #[error("the ChatGPT sign-in changed account; restart with the intended profile")]
    AccountChanged,
    #[error("the ChatGPT sign-in expired and could not be renewed; sign in again")]
    Expired,
    #[error("credential renewal changed its bound profile or authentication coordinates")]
    CoordinatesChanged,
    #[error("the credential owner's revision limit was reached")]
    RevisionExhausted,
}

/// Material stays opaque and must match the attempt's already-bound identity.
#[allow(
    missing_debug_implementations,
    reason = "the snapshot owns an opaque provider credential"
)]
pub struct CurrentCredential {
    pub credential: ProviderCredential,
    pub revision: CredentialRevision,
}

pub type CredentialRenewalFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, CredentialRenewalRefusal>> + Send + 'a>>;

/// Implemented by the credential owner, never by a provider transport that can
/// read arbitrary files. The opener bounds its caller's wait; a credential owner
/// must retain custody of an in-flight rotating grant through bounded persistence
/// even when that caller stops waiting.
pub trait CredentialOwner: Send + Sync {
    fn current(&self) -> CredentialRenewalFuture<'_, CurrentCredential>;
    fn recover(&self, rejected: CredentialRevision) -> CredentialRenewalFuture<'_, bool>;
    fn accepted(&self, revision: CredentialRevision) -> CredentialRenewalFuture<'_, ()>;
}
