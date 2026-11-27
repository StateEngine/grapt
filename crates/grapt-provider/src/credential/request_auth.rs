//! An injected credential owner authenticates final request bytes without disclosing material.
use super::CredentialError;
use std::{future::Future, pin::Pin};

pub type RequestAuthenticationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<reqwest::Request, CredentialError>> + Send + 'a>>;

/// The credential owner resolves and authenticates; the provider cannot read its source.
pub trait RequestAuthenticator: Send + Sync {
    fn authenticate(&self, request: reqwest::Request) -> RequestAuthenticationFuture<'_>;
}
