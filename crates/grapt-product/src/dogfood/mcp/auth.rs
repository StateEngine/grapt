//! Product composition for MCP OAuth. SQLite stays behind Inbox's existing
//! store owner; network operations never hold its mutex or a SQL transaction.
mod login;
mod refresh;

use grapt_inbox::Inbox;
use grapt_mcp_client::{
    oauth::{credential_lock::CredentialLock, http::MetadataHttp, tokens::OAuthCredential},
    BearerLease,
};
use grapt_store::mcp_credential::{McpCredentialKey, McpCredentialRevision, McpCredentialSnapshot};
use grapt_types::{ClockAuthority, McpLoginId, Secret};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

#[derive(Debug, thiserror::Error)]
#[error("MCP authentication failed: {0}")]
pub(crate) struct AuthError(pub(super) &'static str);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginState {
    Pending,
    Complete,
    Failed,
}
struct Attempt {
    server: String,
    resource: String,
    state: LoginState,
}

pub(crate) struct AuthManager {
    inbox: Arc<Mutex<Inbox>>,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
    lock: CredentialLock,
    http: std::sync::OnceLock<Result<MetadataHttp, grapt_mcp_client::oauth::http::OAuthHttpError>>,
    operations: Arc<Semaphore>,
    attempts: Mutex<BTreeMap<McpLoginId, Attempt>>,
}
impl std::fmt::Debug for AuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("McpAuthManager")
    }
}
impl AuthManager {
    pub(crate) fn new(
        root: &Path,
        inbox: Arc<Mutex<Inbox>>,
        clock: Arc<dyn ClockAuthority + Send + Sync>,
    ) -> Result<Arc<Self>, AuthError> {
        Ok(Arc::new(Self {
            inbox,
            clock,
            lock: CredentialLock::for_profile(root)
                .map_err(|_| AuthError("profile lock unavailable"))?,
            http: std::sync::OnceLock::new(),
            operations: Arc::new(Semaphore::new(8)),
            attempts: Mutex::new(BTreeMap::new()),
        }))
    }
    fn http(&self) -> Result<&MetadataHttp, AuthError> {
        self.http
            .get_or_init(MetadataHttp::new)
            .as_ref()
            .map_err(|_| AuthError("HTTP client unavailable"))
    }
    fn now(&self) -> Result<u64, AuthError> {
        u64::try_from(self.clock.now().as_millis())
            .map_err(|_| AuthError("clock precedes the Unix epoch"))
    }
    fn read(&self, key: &McpCredentialKey) -> Result<McpCredentialSnapshot, AuthError> {
        self.inbox
            .lock()
            .map_err(|_| AuthError("store unavailable"))?
            .store()
            .mcp_credential(key)
            .map_err(|_| AuthError("stored credential is unreadable"))
    }
    fn write(
        &self,
        key: &McpCredentialKey,
        revision: McpCredentialRevision,
        value: Option<Secret<String>>,
    ) -> Result<McpCredentialSnapshot, AuthError> {
        self.inbox
            .lock()
            .map_err(|_| AuthError("store unavailable"))?
            .store_mut()
            .replace_mcp_credential(key, revision, value)
            .map_err(|_| AuthError("credential changed; retry sign-in"))
    }
    fn key(server: &str, resource: &str) -> Result<McpCredentialKey, AuthError> {
        McpCredentialKey::for_server(server, resource)
            .map_err(|_| AuthError("invalid server identity"))
    }
    fn decode(snapshot: &McpCredentialSnapshot) -> Result<Option<OAuthCredential>, AuthError> {
        snapshot
            .value
            .as_ref()
            .map(OAuthCredential::decode)
            .transpose()
            .map_err(|_| AuthError("stored credential is malformed; sign out first"))
    }
    fn lease(
        snapshot: &McpCredentialSnapshot,
        credential: &OAuthCredential,
    ) -> Result<BearerLease, AuthError> {
        let McpCredentialRevision::Stored(revision) = snapshot.revision else {
            return Err(AuthError("credential revision missing"));
        };
        Ok(BearerLease {
            token: Secret::new(credential.access_token().expose().clone()),
            generation: credential.generation().clone(),
            revision,
        })
    }
    pub(crate) async fn logout(&self, server: &str, resource: &str) -> Result<(), AuthError> {
        let key = Self::key(server, resource)?;
        let _guard = self
            .lock
            .acquire(std::time::Duration::from_secs(20))
            .await
            .map_err(|_| AuthError("credential transaction busy"))?;
        let snapshot = self.read(&key)?;
        self.write(&key, snapshot.revision, None)?;
        Ok(())
    }
    pub(crate) fn status(
        &self,
        server: &str,
        resource: &str,
        login: &McpLoginId,
    ) -> Result<LoginState, AuthError> {
        let attempts = self
            .attempts
            .lock()
            .map_err(|_| AuthError("sign-in status unavailable"))?;
        let attempt = attempts
            .get(login)
            .filter(|a| a.server == server && a.resource == resource)
            .ok_or(AuthError("unknown sign-in attempt"))?;
        Ok(attempt.state)
    }
}

#[cfg(test)]
mod tests;
