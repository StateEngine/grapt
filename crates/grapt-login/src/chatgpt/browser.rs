// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: explicit entropy, bounded owned callback listener,
// exact loopback/Host/state binding, and destination-bound existing token client.
use super::*;
use grapt_types::Secret;
use sha2::{Digest, Sha256};
use std::net::{Ipv4Addr, SocketAddrV4};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use zeroize::Zeroizing;

mod callback;

/// One bound browser authorization, consumed by its sole code exchange.
pub struct BrowserSignIn {
    listener: TcpListener,
    issuer: String,
    redirect: String,
    authorization: Secret<String>,
    state: Zeroizing<String>,
    verifier: Zeroizing<String>,
}
impl fmt::Debug for BrowserSignIn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BrowserSignIn(<pending>)")
    }
}
impl BrowserSignIn {
    /// These ports are registered for the public Codex client. Never displace
    /// another listener or publish a redirect that this process does not own.
    pub async fn listen(
        verifier: Secret<[u8; 32]>,
        state: Secret<[u8; 32]>,
    ) -> Result<Self, SignInError> {
        let listener = match TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1455)).await {
            Ok(listener) => listener,
            Err(_) => TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1457))
                .await
                .map_err(|_| {
                    SignInError::Store("browser sign-in callback ports are unavailable".into())
                })?,
        };
        Self::new(listener, ISSUER, verifier, state)
    }
    #[cfg(feature = "loopback-testkit")]
    pub async fn listen_loopback(
        issuer: &str,
        verifier: Secret<[u8; 32]>,
        state: Secret<[u8; 32]>,
    ) -> Result<Self, SignInError> {
        // Use the same literal-loopback fixture policy as the token client.
        let _client = super::loopback_client(issuer)?;
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| SignInError::Store("browser fixture listener unavailable".into()))?;
        Self::new(listener, issuer, verifier, state)
    }
    fn new(
        listener: TcpListener,
        issuer: &str,
        verifier: Secret<[u8; 32]>,
        state: Secret<[u8; 32]>,
    ) -> Result<Self, SignInError> {
        let port = listener
            .local_addr()
            .map_err(|_| SignInError::Malformed("browser callback"))?
            .port();
        let redirect = format!("http://127.0.0.1:{port}/auth/callback");
        let verifier = Zeroizing::new(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier.expose()),
        );
        let state =
            Zeroizing::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(state.expose()));
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let mut url = reqwest::Url::parse(&format!("{issuer}/oauth/authorize"))
            .map_err(|_| SignInError::Malformed("authorization URL"))?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", redirect.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
            ("scope", "openid profile email offline_access"),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", super::ORIGINATOR),
        ]);
        Ok(Self {
            listener,
            issuer: issuer.into(),
            redirect,
            authorization: Secret::new(url.into()),
            state,
            verifier,
        })
    }
    /// Only the initiating UI receives this URL; it never enters diagnostics.
    pub fn authorization_url(&self) -> &Secret<String> {
        &self.authorization
    }

    pub async fn wait(
        self,
        client: &SignInClient,
        clock: &(dyn ClockAuthority + Sync),
    ) -> Result<Tokens, SignInError> {
        let code = self.callback().await?;
        let form = form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &self.redirect),
            ("client_id", CLIENT_ID),
            ("code_verifier", &self.verifier),
        ]);
        token_request(client, &self.issuer, form, None, clock).await
    }
    fn accept(&self, target: &str) -> Result<Option<Zeroizing<String>>, SignInError> {
        if target.len() > 8192 {
            return Ok(None);
        }
        let port = self
            .listener
            .local_addr()
            .map_err(|_| SignInError::Malformed("browser callback"))?
            .port();
        let Ok(url) = reqwest::Url::parse(&format!("http://127.0.0.1:{port}{target}")) else {
            return Ok(None);
        };
        if url.path() != "/auth/callback" || url.fragment().is_some() {
            return Ok(None);
        }
        let mut state = None;
        let mut code = None;
        let mut error = None;
        for (index, (name, value)) in url.query_pairs().enumerate() {
            if index >= 32 {
                return Ok(None);
            }
            let slot = match name.as_ref() {
                "state" => &mut state,
                "code" => &mut code,
                "error" => &mut error,
                _ => continue,
            };
            if value.len() > 4096 || slot.replace(Zeroizing::new(value.into_owned())).is_some() {
                return Ok(None);
            }
        }
        let Some(state) = state else {
            return Ok(None);
        };
        if state.as_bytes().ct_eq(self.state.as_bytes()).unwrap_u8() != 1 {
            return Ok(None);
        }
        if error.is_some() {
            return Err(SignInError::Refused {
                what: "ChatGPT authorization",
                status: 403,
            });
        }
        Ok(code.filter(|code| !code.is_empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn authorization_uses_the_s256_verifier_and_an_independent_state(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let flow =
            BrowserSignIn::new(listener, ISSUER, Secret::new([7; 32]), Secret::new([9; 32]))?;
        let url = reqwest::Url::parse(flow.authorization_url().expose())?;
        let parameter = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
                .ok_or("authorization parameter absent")
        };
        assert_eq!(parameter("code_challenge_method")?, "S256");
        assert_eq!(
            parameter("code_challenge")?,
            "3Ev4DHdHPRMPoN6GukAY_pi7IUAF5qWJHRK6kURvnoE"
        );
        assert_ne!(parameter("state")?, flow.verifier.as_str());
        assert!(!format!("{flow:?}").contains(flow.verifier.as_str()));
        Ok(())
    }
}
