//! Process exit classes and their stable CLI codes.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitClass {
    Success,
    Usage,
    NotFound,
    Refused,
    Discovery,
    Protocol,
    Remote,
    Authentication,
    Cancelled,
    Output,
    Child,
}

impl ExitClass {
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Usage => 2,
            Self::NotFound => 3,
            Self::Refused => 4,
            Self::Discovery => 10,
            Self::Protocol => 11,
            Self::Remote => 12,
            Self::Authentication => 13,
            Self::Cancelled => 130,
            Self::Output => 74,
            Self::Child => 71,
        }
    }
}

impl crate::ApiClientError {
    /// Stable process classification, independent of server diagnostic wording.
    pub const fn exit_class(&self) -> ExitClass {
        use crate::ApiClientError;
        match self {
            ApiClientError::Status(status) | ApiClientError::StatusDetail { status, .. } => {
                match status.as_u16() {
                    404 => ExitClass::NotFound,
                    401 => ExitClass::Authentication,
                    400..=499 => ExitClass::Refused,
                    500..=599 => ExitClass::Remote,
                    _ => ExitClass::Protocol,
                }
            }
            ApiClientError::ProviderKeyRejected
            | ApiClientError::GoogleAdcCredentials
            | ApiClientError::AzureCredentials
            | ApiClientError::AwsCredentials
            | ApiClientError::AwsProfileConfiguration => ExitClass::Authentication,
            ApiClientError::ProviderVerificationFailed
            | ApiClientError::EndpointRefused(_)
            | ApiClientError::Transport(_)
            | ApiClientError::TransportStalled { .. }
            | ApiClientError::EmbeddedServiceExited { .. }
            | ApiClientError::EventClosed => ExitClass::Remote,
            ApiClientError::InvalidEndpoint
            | ApiClientError::PromptBound
            | ApiClientError::ProviderSetup => ExitClass::Usage,
            ApiClientError::Tls(_)
            | ApiClientError::Build(_)
            | ApiClientError::Encode
            | ApiClientError::ResponseTooLarge { .. }
            | ApiClientError::Decode
            | ApiClientError::SessionWire
            | ApiClientError::EventContentType
            | ApiClientError::EventFrameTooLarge { .. }
            | ApiClientError::EventFrame(_)
            | ApiClientError::EventPendingBound { .. }
            | ApiClientError::MissingChildPid
            | ApiClientError::EmbeddedOwnerMismatch { .. } => ExitClass::Protocol,
        }
    }
}
