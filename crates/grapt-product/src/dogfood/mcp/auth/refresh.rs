use super::*;
use grapt_mcp_client::{oauth::tokens::TokenGrant, BearerFuture, BearerProvider, HttpError};
use std::time::Duration;

struct ServerBearer {
    manager: Arc<AuthManager>,
    server: String,
    resource: String,
}
impl BearerProvider for ServerBearer {
    fn acquire(&self, rejected_revision: Option<u64>) -> BearerFuture<'_> {
        Box::pin(async move {
            self.manager
                .clone()
                .access(
                    self.server.clone(),
                    self.resource.clone(),
                    rejected_revision,
                )
                .await
                .map_err(|_| HttpError::Authentication)
        })
    }
}
impl AuthManager {
    pub(crate) fn bearer(
        self: &Arc<Self>,
        server: &str,
        resource: &str,
    ) -> Arc<dyn BearerProvider> {
        Arc::new(ServerBearer {
            manager: self.clone(),
            server: server.to_owned(),
            resource: resource.to_owned(),
        })
    }
    async fn access(
        self: Arc<Self>,
        server: String,
        resource: String,
        rejected: Option<u64>,
    ) -> Result<Option<BearerLease>, AuthError> {
        let key = Self::key(&server, &resource)?;
        let observed = self.read(&key)?;
        let Some(credential) = Self::decode(&observed)? else {
            return Ok(None);
        };
        credential
            .check_request(&resource, None)
            .map_err(|_| AuthError("credential resource changed"))?;
        let rejected_current = matches!(observed.revision,McpCredentialRevision::Stored(revision) if Some(revision)==rejected);
        if !rejected_current && !credential.needs_refresh(self.now()?) {
            return Self::lease(&observed, &credential).map(Some);
        }
        let permit = self
            .operations
            .clone()
            .try_acquire_owned()
            .map_err(|_| AuthError("too many authentication operations"))?;
        // Rotation and its commit belong to this task, even if the HTTP caller
        // cancels. Dropping a token response before persistence loses refresh
        // tokens on servers which rotate them on every use.
        tokio::spawn(async move {
            let _permit = permit;
            let _guard = self
                .lock
                .acquire(Duration::from_secs(20))
                .await
                .map_err(|_| AuthError("credential transaction busy"))?;
            let current = self.read(&key)?;
            let next = Self::decode(&current)?.ok_or(AuthError("signed out"))?;
            next.check_request(&resource, None)
                .map_err(|_| AuthError("credential resource changed"))?;
            if next.generation() != credential.generation() {
                return Err(AuthError("sign-in generation changed"));
            }
            if current.revision != observed.revision {
                return Self::lease(&current, &next).map(Some);
            }
            let discovered = self
                .http()?
                .discover(&resource, Duration::from_secs(30))
                .await
                .map_err(|_| AuthError("authorization discovery failed"))?;
            let form = next
                .refresh_form(
                    &discovered.authorization,
                    &resource,
                    next.client_id().expose(),
                )
                .map_err(|_| AuthError("sign in again"))?;
            let response = match self
                .http()?
                .token(&discovered.authorization, &form, Duration::from_secs(30))
                .await
            {
                Ok(response) => response,
                Err(grapt_mcp_client::oauth::http::OAuthHttpError::TokenRejected) => {
                    // Only a conclusive rejection retires the exact revision
                    // held by this transaction. Network uncertainty does not.
                    self.write(&key, current.revision, None)?;
                    return Err(AuthError("refresh rejected; sign in again"));
                }
                Err(_) => return Err(AuthError("refresh failed; sign in again")),
            };
            let refreshed = OAuthCredential::from_response(
                &discovered.authorization,
                &resource,
                next.client_id().expose(),
                &response,
                self.now()?,
                TokenGrant::Refresh(&next),
            )
            .map_err(|_| AuthError("invalid token response"))?;
            let stored = self.write(
                &key,
                current.revision,
                Some(
                    refreshed
                        .encode()
                        .map_err(|_| AuthError("credential too large"))?,
                ),
            )?;
            Self::lease(&stored, &refreshed).map(Some)
        })
        .await
        .map_err(|_| AuthError("credential refresh task failed"))?
    }
}
