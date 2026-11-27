use super::VaultError;
use serde::{Deserialize, Serialize};

fn origin(raw: &str) -> Result<String, VaultError> {
    if raw.len() > 2048 {
        return Err(VaultError::InvalidInput);
    }
    let parsed = reqwest::Url::parse(raw).map_err(|_| VaultError::InvalidInput)?;
    let loopback = parsed
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|address| address.is_loopback());
    let secure = parsed.scheme() == "https" || parsed.scheme() == "http" && loopback;
    if !secure
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(VaultError::InvalidInput);
    }
    Ok(parsed.origin().ascii_serialization())
}

fn base_origin(raw: &str) -> Result<String, VaultError> {
    if raw.len() > 2048 {
        return Err(VaultError::InvalidInput);
    }
    if reqwest::Url::parse(raw)
        .map_err(|_| VaultError::InvalidInput)?
        .query()
        .is_some()
    {
        return Err(VaultError::InvalidInput);
    }
    origin(raw)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiDestination(String);
impl ApiDestination {
    pub fn parse(raw: &str) -> Result<Self, VaultError> {
        base_origin(raw).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthRefreshDestination(String);
impl OAuthRefreshDestination {
    pub fn parse(raw: &str) -> Result<Self, VaultError> {
        base_origin(raw).map(Self)
    }
}

/// Persist both origins when storing OAuth state: an access token's API and a
/// refresh token's issuer are distinct destinations, never interchangeable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationBinding {
    api: String,
    refresh: Option<String>,
}
impl DestinationBinding {
    /// The canonical, non-secret origin retained at connection creation.
    pub fn api_origin(&self) -> &str {
        &self.api
    }

    pub fn key(api: ApiDestination) -> Self {
        Self {
            api: api.0,
            refresh: None,
        }
    }
    pub fn oauth(api: ApiDestination, refresh: OAuthRefreshDestination) -> Self {
        Self {
            api: api.0,
            refresh: Some(refresh.0),
        }
    }
    pub fn check_api(&self, url: &str) -> Result<(), VaultError> {
        if origin(url)? == self.api {
            Ok(())
        } else {
            Err(VaultError::BindingMismatch)
        }
    }
    pub fn check_refresh(&self, url: &str) -> Result<(), VaultError> {
        if self
            .refresh
            .as_ref()
            .is_some_and(|expected| origin(url).is_ok_and(|current| &current == expected))
        {
            Ok(())
        } else {
            Err(VaultError::BindingMismatch)
        }
    }
    pub(super) fn valid(&self) -> bool {
        origin(&self.api).is_ok_and(|parsed| parsed == self.api)
            && self
                .refresh
                .as_ref()
                .is_none_or(|value| origin(value).is_ok_and(|parsed| &parsed == value))
    }
    pub(super) fn has_refresh(&self) -> bool {
        self.refresh.is_some()
    }
}
