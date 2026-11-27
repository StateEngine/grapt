// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: explicit public-client registration without borrowed
// client identities, a bounded response, and exact loopback redirect binding.
// (dynamic client registration), commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d.

use grapt_types::Secret;
use serde::Deserialize;
use url::Url;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("OAuth public-client registration is malformed or does not match its requested redirect")]
pub struct RegistrationError;

pub struct PublicClient {
    client: Secret<String>,
}
impl std::fmt::Debug for PublicClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PublicClient([REDACTED])")
    }
}

fn redirect(raw: &str) -> Result<Url, RegistrationError> {
    if raw.len() > 8192 || raw.chars().any(char::is_control) || raw.trim() != raw {
        return Err(RegistrationError);
    }
    let url = Url::parse(raw).map_err(|_| RegistrationError)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || url.port() == Some(0)
        || !url.path().starts_with("/mcp/oauth/")
        || url.path().len() != "/mcp/oauth/".len() + 43
        || !url.path()["/mcp/oauth/".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err(RegistrationError);
    }
    Ok(url)
}

impl PublicClient {
    /// Pre-registration is the explicit alternative when an authorization
    /// server does not support dynamic public-client registration. Grapt
    /// never claims another application's hosted client metadata identity.
    pub fn pre_registered(client: &str) -> Result<Self, RegistrationError> {
        if client.is_empty() || client.len() > 8192 || client.chars().any(char::is_control) {
            return Err(RegistrationError);
        }
        Ok(Self {
            client: Secret::new(client.to_owned()),
        })
    }
    pub fn client_id(&self) -> &Secret<String> {
        &self.client
    }

    pub fn registration_request(redirect_uri: &str) -> Result<Vec<u8>, RegistrationError> {
        let redirect = redirect(redirect_uri)?;
        serde_json::to_vec(&serde_json::json!({
            "client_name":"Grapt",
            "application_type":"native",
            "redirect_uris":[redirect.as_str()],
            "grant_types":["authorization_code","refresh_token"],
            "response_types":["code"],
            "token_endpoint_auth_method":"none"
        }))
        .map_err(|_| RegistrationError)
    }

    pub fn from_registration(
        response: &Secret<Vec<u8>>,
        redirect_uri: &str,
    ) -> Result<Self, RegistrationError> {
        #[derive(Deserialize)]
        struct Response {
            client_id: String,
            redirect_uris: Vec<String>,
            token_endpoint_auth_method: String,
            grant_types: Option<Vec<String>>,
            response_types: Option<Vec<String>>,
            error: Option<String>,
        }
        if response.expose().len() > 64 * 1024 {
            return Err(RegistrationError);
        }
        let expected = redirect(redirect_uri)?;
        let value: Response =
            serde_json::from_slice(response.expose()).map_err(|_| RegistrationError)?;
        if value.error.is_some()
            || value.token_endpoint_auth_method != "none"
            || value.redirect_uris.len() != 1
            || value.redirect_uris[0] != expected.as_str()
            || value.grant_types.as_ref().is_some_and(|grants| {
                grants.len() > 2
                    || !grants.iter().any(|grant| grant == "authorization_code")
                    || grants.iter().any(|grant| {
                        !matches!(grant.as_str(), "authorization_code" | "refresh_token")
                    })
            })
            || value
                .response_types
                .as_ref()
                .is_some_and(|types| types.len() != 1 || types[0] != "code")
        {
            return Err(RegistrationError);
        }
        Self::pre_registered(&value.client_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const REDIRECT: &str =
        "http://127.0.0.1:39281/mcp/oauth/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    #[test]
    fn registration_requests_an_identified_native_public_client_and_checks_the_reply(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let request: serde_json::Value =
            serde_json::from_slice(&PublicClient::registration_request(REDIRECT)?)?;
        assert_eq!(request["client_name"], "Grapt");
        assert_eq!(request["token_endpoint_auth_method"], "none");
        assert!(request.get("client_uri").is_none());
        let response = json!({"client_id":"private-client-fixture","redirect_uris":[REDIRECT],"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"]});
        let client = PublicClient::from_registration(
            &Secret::new(serde_json::to_vec(&response)?),
            REDIRECT,
        )?;
        assert_eq!(client.client_id().expose(), "private-client-fixture");
        assert!(!format!("{client:?}").contains("private-client-fixture"));
        for (name, value) in [
            ("redirect_uris", json!([REDIRECT.replace("39281", "39282")])),
            ("token_endpoint_auth_method", json!("client_secret_basic")),
            ("grant_types", json!(["client_credentials"])),
            ("error", json!("invalid_client_metadata")),
        ] {
            let mut invalid = response.clone();
            invalid[name] = value;
            assert!(PublicClient::from_registration(
                &Secret::new(serde_json::to_vec(&invalid)?),
                REDIRECT
            )
            .is_err());
        }
        let duplicate=format!("{{\"client_id\":\"first\",\"client_id\":\"second\",\"redirect_uris\":[\"{REDIRECT}\"],\"token_endpoint_auth_method\":\"none\"}}");
        assert!(
            PublicClient::from_registration(&Secret::new(duplicate.into_bytes()), REDIRECT)
                .is_err()
        );
        Ok(())
    }
}
