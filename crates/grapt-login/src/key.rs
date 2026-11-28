//! Bounded API key checks, before any connection is published.
use crate::vault::{ApiDestination, CredentialView, DestinationBinding, VaultSecret};
pub mod cloudflare;

pub mod azure;
mod cohere;
mod endpoint_metadata;
pub use endpoint_metadata::enrich_listing;
mod served_models;
pub use served_models::{candidate_models, served_listing, ServedError};
mod google;
pub use cohere::verify_cohere_completion;
mod vertex;
pub use google::verify_completion;
pub use vertex::{verify_vertex, verify_vertex_adc, VertexProtocol};
mod watsonx;
pub use watsonx::verify_watsonx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPlacement {
    Google,
    Cohere,
    Bearer,
    Anthropic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyProtocol {
    Responses,
    Messages,
    BearerMessages,
    Chat,
    GroqChat,
    CompletionChat,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("Microsoft Entra ID authorization could not verify the selected Azure deployment")]
    AzureCredentials,
    #[error("provider endpoint or API key is invalid")]
    Invalid,
    #[error("provider could not be reached to check the key")]
    Unreachable,
    #[error("provider refused the API key")]
    Refused,
    #[error("provider model list could not be read")]
    Listing,
    #[error("provider could not complete the connection check")]
    Completion,
    #[error("AWS credentials are unavailable; configure the selected AWS profile or credential chain and retry Connect")]
    AwsCredentials,
    #[error("AWS profile configuration could not be used; credential_process is unsupported; use environment credentials, a shared static profile, SSO, web identity or an IAM role, then retry Connect")]
    AwsProfileConfiguration,
}

/// Retains the verified destination together with its key and bounded listing.
/// There is no constructor which bypasses the network check.
#[derive(Debug)]
pub struct VerifiedKey {
    secret: VaultSecret,
    /// The exact API base this key was checked against. Discovery reads only
    /// this, so a caller-supplied destination can never move the key.
    base: String,
    binding: DestinationBinding,
    listing: Vec<u8>,
    cloudflare: Option<grapt_types::cloudflare::CloudflareGatewayScope>,
    watsonx_project: Option<grapt_types::WatsonxProjectId>,
}

impl VerifiedKey {
    pub fn cloudflare(&self) -> Option<&grapt_types::cloudflare::CloudflareGatewayScope> {
        self.cloudflare.as_ref()
    }
    pub fn listing(&self) -> &[u8] {
        &self.listing
    }

    /// The verified key and the exact base it was checked against, for the
    /// best-effort catalogue read. Crate-visible so no other crate can reach a
    /// key ahead of its check, and there is no caller-supplied base to confuse.
    pub(crate) fn secret(&self) -> &VaultSecret {
        &self.secret
    }

    /// The base this key was verified against.
    pub(crate) fn base(&self) -> &str {
        &self.base
    }

    /// The destination binding this key was verified against.
    pub(crate) fn binding(&self) -> &DestinationBinding {
        &self.binding
    }

    /// Replace the checked listing with an enriched one. The caller has already
    /// verified this key against the same destination, and enrichment only adds
    /// reported fields, so the verification is not weakened.
    pub fn with_listing(mut self, listing: Vec<u8>) -> Self {
        self.listing = listing;
        self
    }

    pub fn watsonx_project(&self) -> Option<&grapt_types::WatsonxProjectId> {
        self.watsonx_project.as_ref()
    }

    pub fn into_parts(self) -> (VaultSecret, DestinationBinding, Vec<u8>) {
        (self.secret, self.binding, self.listing)
    }

    /// Public model lists do not authenticate a key. Complete a small request
    /// against the selected model before publishing a compatible connection.
    /// The key is kept whatever the outcome, so a connect can try the
    /// endpoint's next listed model after a refusal.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub async fn check_model(
        &self,
        api_base: &str,
        protocol: KeyProtocol,
        model: &str,
    ) -> Result<(), KeyError> {
        let path = match protocol {
            KeyProtocol::Responses => "responses",
            KeyProtocol::Messages | KeyProtocol::BearerMessages => "messages",
            KeyProtocol::Chat | KeyProtocol::GroqChat | KeyProtocol::CompletionChat => {
                "chat/completions"
            }
        };
        let url = format!("{}/{path}", api_base.trim_end_matches('/'));
        self.binding
            .check_api(&url)
            .map_err(|_| KeyError::Invalid)?;
        let CredentialView::ApiKey(key) = self.secret.expose() else {
            return Err(KeyError::Invalid);
        };
        let client = crate::chatgpt::bounded_client(api_base.starts_with("https://"))
            .map_err(|_| KeyError::Unreachable)?;
        let request = client.post(&url);
        // A server on this machine may load the model from disk on its first
        // request; that is a slow answer, not an unreachable one.
        let request = if served_models::loopback_origin(api_base).is_some() {
            request.timeout(LOCAL_LOAD_ALLOWANCE)
        } else {
            request
        };
        // The session-routed gateway refuses a request without a Session and
        // asks a client to name itself; the check is its own conversation.
        let request = if session_routed(api_base) {
            request
                .header("user-agent", concat!("grapt/", env!("CARGO_PKG_VERSION")))
                .header(SESSION_ROUTING_HEADER, "grapt-key-check")
        } else {
            request
        };
        let (request, body) = match protocol {
            KeyProtocol::GroqChat | KeyProtocol::CompletionChat => {
                let mut body = serde_json::json!({
                    "model":model, "messages":[{"role":"user","content":"Hello"}],
                    "max_completion_tokens":32, "stream":false,
                });
                if protocol == KeyProtocol::GroqChat && !model.starts_with("openai/gpt-oss-") {
                    body["reasoning_format"] = serde_json::json!("parsed");
                }
                (request.bearer_auth(key), body)
            }
            KeyProtocol::Chat => (
                request.bearer_auth(key),
                serde_json::json!({
                    "model":model, "messages":[{"role":"user","content":"Hello"}],
                    "max_tokens":32, "stream":false,
                }),
            ),
            KeyProtocol::Responses => (
                request.bearer_auth(key),
                serde_json::json!({
                    "model":model, "input":"Hello", "max_output_tokens":32, "stream":false,
                }),
            ),
            KeyProtocol::Messages | KeyProtocol::BearerMessages => (
                match protocol {
                    KeyProtocol::BearerMessages => request.bearer_auth(key),
                    _ => request.header("x-api-key", key),
                }
                .header("anthropic-version", "2023-06-01"),
                serde_json::json!({
                    "model":model, "messages":[{"role":"user","content":"Hello"}],
                    "max_tokens":32, "stream":false,
                }),
            ),
        };
        let request = request
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&body).map_err(|_| KeyError::Invalid)?);
        let mut response = request.send().await.map_err(|_| KeyError::Unreachable)?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(KeyError::Refused);
        }
        if !response.status().is_success() {
            return Err(KeyError::Completion);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| KeyError::Completion)? {
            if chunk.len() > (64 * 1024_usize).saturating_sub(bytes.len()) {
                return Err(KeyError::Completion);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| KeyError::Completion)?;
        let completed = match protocol {
            KeyProtocol::Chat | KeyProtocol::GroqChat | KeyProtocol::CompletionChat => {
                value["choices"][0]["message"].is_object()
                    && matches!(
                        value["choices"][0]["finish_reason"].as_str(),
                        Some("stop" | "length")
                    )
            }
            KeyProtocol::Responses => {
                value["output"].is_array()
                    && matches!(value["status"].as_str(), Some("completed" | "incomplete"))
            }
            KeyProtocol::Messages | KeyProtocol::BearerMessages => {
                value["content"].is_array()
                    && matches!(
                        value["stop_reason"].as_str(),
                        Some("end_turn" | "max_tokens" | "stop_sequence")
                    )
            }
        };
        if value["error"].is_null() && completed {
            Ok(())
        } else {
            Err(KeyError::Completion)
        }
    }
}

/// How long a completion check waits on a loopback server, which may be
/// loading the model before it answers.
pub const LOCAL_LOAD_ALLOWANCE: std::time::Duration = std::time::Duration::from_secs(120);

/// A session-routed gateway: its compatible chat route refuses a request that
/// names no Session.
const SESSION_ROUTED_HOST: &str = "opencode.ai";
const SESSION_ROUTING_HEADER: &str = "x-opencode-session";

/// Same host rule as the provider transport's `session_routed`; neither crate
/// depends on the other, so a new gateway host changes both.
fn session_routed(api_base: &str) -> bool {
    reqwest::Url::parse(api_base).ok().is_some_and(|url| {
        url.host_str().is_some_and(|host| {
            host.strip_suffix(SESSION_ROUTED_HOST)
                .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with('.'))
        })
    })
}

/// A custom endpoint has user-supplied model metadata, rather than a model
/// catalogue. Only a completed request can authenticate that connection.
pub async fn verify_model_listing(
    api_base: &str,
    secret: VaultSecret,
    model: &str,
    listing: Vec<u8>,
    protocol: KeyProtocol,
) -> Result<VerifiedKey, KeyError> {
    if protocol == KeyProtocol::BearerMessages {
        crate::vault::MessagesDestination::parse(api_base).map_err(|_| KeyError::Invalid)?;
    }
    let binding =
        DestinationBinding::key(ApiDestination::parse(api_base).map_err(|_| KeyError::Invalid)?);
    let verified = VerifiedKey {
        watsonx_project: None,
        base: api_base.to_owned(),
        secret,
        binding,
        listing,
        cloudflare: None,
    };
    verified.check_model(api_base, protocol, model).await?;
    Ok(verified)
}

/// Accept HTTPS or a literal loopback HTTP fixture; never follow a redirect or
/// inherit proxy configuration while a key is attached. `api_base` includes
/// the provider's API prefix, such as `/v1` or `/api/v1`.
pub async fn verify(
    api_base: &str,
    placement: KeyPlacement,
    secret: VaultSecret,
) -> Result<VerifiedKey, KeyError> {
    if placement == KeyPlacement::Google {
        return google::verify(api_base, secret).await;
    }
    if placement == KeyPlacement::Cohere {
        return cohere::verify(api_base, secret).await;
    }
    let binding =
        DestinationBinding::key(ApiDestination::parse(api_base).map_err(|_| KeyError::Invalid)?);
    let suffix = if placement == KeyPlacement::Anthropic {
        "?limit=1000"
    } else {
        ""
    };
    let url = format!("{}/models{suffix}", api_base.trim_end_matches('/'));
    binding.check_api(&url).map_err(|_| KeyError::Invalid)?;
    let client = crate::chatgpt::bounded_client(api_base.starts_with("https://"))
        .map_err(|_| KeyError::Unreachable)?;
    let CredentialView::ApiKey(key) = secret.expose() else {
        return Err(KeyError::Invalid);
    };
    let request = match placement {
        KeyPlacement::Bearer => client.get(&url).bearer_auth(key),
        KeyPlacement::Google => return Err(KeyError::Invalid),
        KeyPlacement::Cohere => return Err(KeyError::Invalid),
        KeyPlacement::Anthropic => client
            .get(&url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
    };
    let mut response = request.send().await.map_err(|_| KeyError::Unreachable)?;
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(KeyError::Refused);
    }
    if !response.status().is_success() {
        return Err(KeyError::Listing);
    }
    const BYTE_CAP: usize = 4 * 1024 * 1024;
    let mut listing = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| KeyError::Listing)? {
        if chunk.len() > BYTE_CAP.saturating_sub(listing.len()) {
            return Err(KeyError::Listing);
        }
        listing.extend_from_slice(&chunk);
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&listing).map_err(|_| KeyError::Listing)?;
    // Together returns a bare array. Both listing shapes pass the same
    // bounds, and neither authenticates a key without the completion check.
    let rows = parsed["data"]
        .as_array()
        .or_else(|| parsed.as_array())
        .ok_or(KeyError::Listing)?;
    if rows.is_empty()
        || rows.len() > 1024
        || parsed["has_more"].as_bool() == Some(true)
        || rows.iter().any(|row| {
            row["id"].as_str().is_none_or(|id| {
                id.is_empty() || id.len() > 256 || id.chars().any(char::is_control)
            })
        })
    {
        return Err(KeyError::Listing);
    }
    let listing = if parsed.is_array() {
        let normalized = serde_json::to_vec(&serde_json::json!({"data":parsed}))
            .map_err(|_| KeyError::Listing)?;
        if normalized.len() > BYTE_CAP {
            return Err(KeyError::Listing);
        }
        normalized
    } else {
        listing
    };
    Ok(VerifiedKey {
        base: api_base.to_owned(),
        secret,
        binding,
        listing,
        cloudflare: None,
        watsonx_project: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn verified_keys_keep_the_destination_and_refuse_redirects(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let server = tokio::spawn(async move {
            for reply in [
                "HTTP/1.1 200 OK\r\nContent-Length: 29\r\nConnection: close\r\n\r\n{\"data\":[{\"id\":\"gpt-model\"}]}",
                "HTTP/1.1 302 Found\r\nLocation: https://foreign.example/models\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            ] {
                let (mut socket, _) = listener.accept().await?;
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") && request.len() <= 8192 {
                    let byte = socket.read_u8().await?;
                    request.push(byte);
                }
                let request = String::from_utf8(request)?;
                assert!(request.starts_with("GET /v1/models "));
                assert!(request.to_ascii_lowercase().contains("authorization: bearer fixture-key"));
                socket.write_all(reply.as_bytes()).await?;
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        });
        let secret = || VaultSecret::api_key("fixture-key".to_owned().into());
        let verified = verify(&base, KeyPlacement::Bearer, secret()?).await?;
        let (_, binding, _) = verified.into_parts();
        assert!(binding.check_api(&format!("{base}/responses")).is_ok());
        assert!(binding
            .check_api("https://foreign.example/responses")
            .is_err());
        assert!(matches!(
            verify(&base, KeyPlacement::Bearer, secret()?).await,
            Err(KeyError::Listing)
        ));
        assert!(matches!(
            verify(&base, KeyPlacement::Bearer, secret()?).await,
            Err(KeyError::Refused)
        ));
        assert!(matches!(
            verify("http://localhost/v1", KeyPlacement::Bearer, secret()?).await,
            Err(KeyError::Invalid)
        ));
        server.await?.map_err(|error| error.to_string())?;
        Ok(())
    }
}

pub mod bedrock;
