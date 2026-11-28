//! Process-only endpoint and credential construction; no service discovery.

use super::{
    production_client, Allowance, ApiClient, ApiClientError, ClientEndpoint, ClientMode,
    Credential, EmbeddedChild, EVENT_IDLE_ALLOWANCE, REQUEST_ALLOWANCE,
};
use crate::CliLimits;
use std::sync::Arc;

impl ApiClient {
    /// Hands the child back on refusal for explicit bounded kill and reap.
    #[allow(clippy::result_large_err)] // matches from_embedded's owned-child cleanup
    pub(crate) fn from_ephemeral(
        endpoint: &str,
        pid: u32,
        username: String,
        secret: String,
        child: tokio::process::Child,
        limits: CliLimits,
    ) -> Result<Self, (ApiClientError, tokio::process::Child)> {
        let build = || {
            let expected = child.id().ok_or(ApiClientError::MissingChildPid)?;
            if expected != pid {
                return Err(ApiClientError::EmbeddedOwnerMismatch {
                    expected,
                    found: pid,
                });
            }
            if username.is_empty()
                || username.contains(':')
                || username.chars().any(char::is_control)
                || secret.is_empty()
                || secret.chars().any(char::is_control)
            {
                return Err(ApiClientError::Decode);
            }
            let mut endpoint = ClientEndpoint::attributed_loopback(endpoint)?;
            endpoint.mode = ClientMode::Embedded;
            Ok(Self {
                endpoint,
                client: production_client(false, Allowance::Request(REQUEST_ALLOWANCE))?,
                event_client: production_client(
                    false,
                    Allowance::EventStream(EVENT_IDLE_ALLOWANCE),
                )?,
                basic: Some(Arc::new(Credential { username, secret })),
                limits,
                embedded: None,
                streak: Arc::new(std::sync::Mutex::new(
                    super::stall::TransportStreak::default(),
                )),
            })
        };
        match build() {
            Ok(mut client) => {
                client.embedded = Some(Arc::new(EmbeddedChild {
                    child: tokio::sync::Mutex::new(Some(child)),
                    stderr: std::sync::Mutex::new(None),
                }));
                Ok(client)
            }
            Err(error) => Err((error, child)),
        }
    }
}
