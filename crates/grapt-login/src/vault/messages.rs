//! The complete API base approved for a bearer-authenticated Messages token.
use super::VaultError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MessagesDestination(String);
impl MessagesDestination {
    pub fn parse(raw: &str) -> Result<Self, VaultError> {
        if raw.len() > 2048 {
            return Err(VaultError::InvalidInput);
        }
        let url = reqwest::Url::parse(raw).map_err(|_| VaultError::InvalidInput)?;
        let secure = url.scheme() == "https";
        #[cfg(feature = "loopback-testkit")]
        let secure = secure
            || (url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port().is_some());
        if !secure
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(VaultError::InvalidInput);
        }
        Ok(Self(url.as_str().trim_end_matches('/').into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for MessagesDestination {
    type Error = VaultError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<MessagesDestination> for String {
    fn from(value: MessagesDestination) -> Self {
        value.0
    }
}
