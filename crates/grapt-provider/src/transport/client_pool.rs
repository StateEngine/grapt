//! Account-scoped transport reuse.
//!
//! Copyright 2025 OpenAI. Modified by Grapt: one lazy client, keyed by account, credential
//! version and origin, instead of a route map. Credentials remain on requests.

use std::sync::{Arc, Mutex};

use grapt_types::{AccountId, CredentialVersion};

use crate::codex::{CodexDeadlines, CodexError};
use crate::credential::{CredentialLease, CredentialPlacement};
use crate::route::ProviderEndpoint;

pub(super) type ClientFactory = fn(CodexDeadlines) -> Result<reqwest::Client, CodexError>;

#[derive(PartialEq, Eq)]
struct Scope {
    account: AccountId,
    version: CredentialVersion,
    placement: CredentialPlacement,
    origin: String,
}

struct Entry {
    scope: Scope,
    client: reqwest::Client,
}

/// One retained client per adapter. A changed identity drops the old pool;
/// clones held by already-staged attempts retain their original identity.
#[derive(Clone)]
pub(super) struct ScopedClients {
    factory: ClientFactory,
    deadlines: CodexDeadlines,
    entry: Arc<Mutex<Option<Entry>>>,
}

impl ScopedClients {
    pub(super) fn new(factory: ClientFactory, deadlines: CodexDeadlines) -> Self {
        Self {
            factory,
            deadlines,
            entry: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) fn for_attempt(
        &self,
        lease: &CredentialLease,
        endpoint: &ProviderEndpoint,
    ) -> Result<reqwest::Client, CodexError> {
        let url = reqwest::Url::parse(&endpoint.url()).map_err(|_| CodexError::RequestBuild {
            reason: "the bound provider endpoint is invalid".to_owned(),
        })?;
        let scope = Scope {
            account: lease.account().clone(),
            version: lease.credential_version().clone(),
            placement: lease.placement(),
            origin: url.origin().ascii_serialization(),
        };
        let mut entry = self
            .entry
            .lock()
            .map_err(|_| CodexError::RegistryPoisoned)?;
        if let Some(existing) = entry.as_ref().filter(|existing| existing.scope == scope) {
            return Ok(existing.client.clone());
        }
        // Drop first: even a failed replacement must not resurrect a previous
        // account's idle connections on the next attempt.
        *entry = None;
        let client = (self.factory)(self.deadlines)?;
        *entry = Some(Entry {
            scope,
            client: client.clone(),
        });
        Ok(client)
    }

    /// A credential source refusal invalidates idle reuse. An in-flight
    /// response may finish, but no later attempt inherits this pool.
    pub(super) fn invalidate(&self) {
        match self.entry.lock() {
            Ok(mut entry) => *entry = None,
            Err(poisoned) => *poisoned.into_inner() = None,
        }
    }
}
