use super::super::McpRegistry;
use super::*;
use grapt_mcp_client::oauth::{
    callback::CallbackListener, callback_uri, registration::PublicClient, tokens::TokenGrant,
    AuthorizationFlow,
};
use grapt_store::OsEntropy;
use std::{sync::Weak, time::Duration};

impl AuthManager {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(crate) async fn login(
        self: &Arc<Self>,
        server: String,
        resource: String,
        id: McpLoginId,
        client: Option<String>,
        scope: Option<String>,
        registry: Weak<McpRegistry>,
    ) -> Result<Secret<String>, AuthError> {
        let permit = self
            .operations
            .clone()
            .try_acquire_owned()
            .map_err(|_| AuthError("too many authentication operations"))?;
        let key = Self::key(&server, &resource)?;
        // Fence admission before setup can yield to logout or another login.
        // A later committed revision must never be adopted by this attempt.
        let expected = {
            let _guard = self
                .lock
                .acquire(Duration::from_secs(20))
                .await
                .map_err(|_| AuthError("credential transaction busy"))?;
            self.read(&key)?.revision
        };
        let listener = CallbackListener::bind()
            .await
            .map_err(|_| AuthError("callback listener unavailable"))?;
        let discovered = self
            .http()?
            .discover(&resource, Duration::from_secs(30))
            .await
            .map_err(|_| AuthError("authorization discovery failed"))?;
        let redirect = callback_uri(&resource, listener.port())
            .map_err(|_| AuthError("invalid callback URI"))?;
        let client = match client {
            Some(client) => PublicClient::pre_registered(&client)
                .map_err(|_| AuthError("invalid public client ID"))?,
            None => self
                .http()?
                .register(
                    &discovered.authorization,
                    redirect.as_str(),
                    Duration::from_secs(20),
                )
                .await
                .map_err(|_| {
                    AuthError("client registration failed; provide a registered public client ID")
                })?,
        };
        let metadata = discovered.authorization;
        let flow = AuthorizationFlow::new(
            metadata.clone(),
            &resource,
            client.client_id().expose(),
            listener.port(),
            OsEntropy::draw_256().map_err(|_| AuthError("OS entropy unavailable"))?,
            OsEntropy::draw_256().map_err(|_| AuthError("OS entropy unavailable"))?,
            scope.as_deref().or(discovered.scope.as_deref()),
        )
        .map_err(|_| AuthError("invalid authorization flow"))?;
        let url = Secret::new(flow.authorization_url().expose().clone());

        {
            let mut attempts = self
                .attempts
                .lock()
                .map_err(|_| AuthError("sign-in status unavailable"))?;
            if attempts.len() >= 16 {
                let old = attempts
                    .iter()
                    .find(|(_, a)| a.state != LoginState::Pending)
                    .map(|(id, _)| id.clone())
                    .ok_or(AuthError("too many sign-in attempts"))?;
                attempts.remove(&old);
            }
            if attempts.contains_key(&id) {
                return Err(AuthError("sign-in ID already exists"));
            }
            attempts.insert(
                id.clone(),
                Attempt {
                    server: server.clone(),
                    resource: resource.clone(),
                    state: LoginState::Pending,
                },
            );
        }
        let manager = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result = async {
                let exchange = listener
                    .wait(flow, Duration::from_secs(120))
                    .await
                    .map_err(|_| AuthError("authorization callback failed"))?;
                let response = manager
                    .http()?
                    .token(&metadata, &exchange.form, Duration::from_secs(30))
                    .await
                    .map_err(|_| AuthError("token exchange failed"))?;
                let credential = OAuthCredential::from_response(
                    &metadata,
                    &resource,
                    client.client_id().expose(),
                    &response,
                    manager.now()?,
                    TokenGrant::Login(id.clone()),
                )
                .map_err(|_| AuthError("invalid token response"))?;
                let _guard = manager
                    .lock
                    .acquire(Duration::from_secs(20))
                    .await
                    .map_err(|_| AuthError("credential transaction busy"))?;
                manager.write(
                    &key,
                    expected,
                    Some(
                        credential
                            .encode()
                            .map_err(|_| AuthError("credential too large"))?,
                    ),
                )?;
                if let Some(registry) = registry.upgrade() {
                    registry.retire_server(&server, &resource).await;
                }
                Ok::<(), AuthError>(())
            }
            .await;
            if let Ok(mut attempts) = manager.attempts.lock() {
                if let Some(attempt) = attempts.get_mut(&id) {
                    attempt.state = if result.is_ok() {
                        LoginState::Complete
                    } else {
                        LoginState::Failed
                    };
                }
            }
        });
        Ok(url)
    }
}
