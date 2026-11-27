//! Closed native Messages authentication, retained with its approved API base.
use super::{CredentialError, CredentialPlacement};
use crate::ProtocolFamily;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MessagesAuthMode {
    #[default]
    ApiKey,
    Bearer,
}
impl MessagesAuthMode {
    pub fn placement(self) -> CredentialPlacement {
        match self {
            Self::ApiKey => CredentialPlacement::AnthropicApiKey,
            Self::Bearer => CredentialPlacement::BearerAuthorization,
        }
    }
}

pub fn messages_placement(
    family: ProtocolFamily,
    mode: MessagesAuthMode,
) -> Option<CredentialPlacement> {
    if family == ProtocolFamily::AnthropicMessages {
        Some(mode.placement())
    } else if mode == MessagesAuthMode::ApiKey {
        Some(crate::transport::adapter::placement_for(family))
    } else {
        None
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct MessagesBinding {
    pub(super) mode: MessagesAuthMode,
    base: String,
}
impl MessagesBinding {
    pub(super) fn new(base: &str, mode: MessagesAuthMode) -> Result<Self, CredentialError> {
        super::destination::origin(base)?;
        let url = reqwest::Url::parse(base).map_err(|_| CredentialError::InvalidDestination)?;
        if url.query().is_some() || url.fragment().is_some() {
            return Err(CredentialError::InvalidDestination);
        }
        Ok(Self {
            mode,
            base: url.as_str().trim_end_matches('/').into(),
        })
    }
    pub(super) fn check_base(&self, base: &str) -> Result<(), CredentialError> {
        if self.base == base {
            Ok(())
        } else {
            Err(CredentialError::DestinationMismatch)
        }
    }
    pub(super) fn check_url(&self, url: &reqwest::Url) -> Result<(), CredentialError> {
        if url.as_str() == format!("{}/messages", self.base) {
            Ok(())
        } else {
            Err(CredentialError::DestinationMismatch)
        }
    }
}
