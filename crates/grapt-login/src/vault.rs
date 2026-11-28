//! Synchronous credential storage, bound to the destinations chosen at sign-in.
//! Backend selection never grants permission to send a credential. The caller
//! validates provider choice, then checks the retained API/refresh binding again
//! where it injects material into a request. Redirects require the same check.
use grapt_types::{ProviderId, Secret, Timestamp};
use zeroize::Zeroizing;

mod destination;
mod material;
mod messages;
mod sealed_file;
pub use destination::{ApiDestination, DestinationBinding, OAuthRefreshDestination};
pub use messages::MessagesDestination;
pub use sealed_file::SealedFileVault;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("credential storage input is invalid")]
    InvalidInput,
    #[error("credential storage is being changed by another operation")]
    Busy,
    #[error("credential storage is locked")]
    Locked,
    #[error("credential storage is unavailable: {0}")]
    Unavailable(StorageFailure),
    #[error("credential protection refused: {0}")]
    Protection(String),
    #[error("credential storage is malformed")]
    Corrupt,
    #[error("credential belongs to another connection or destination")]
    BindingMismatch,
    #[error("legacy credential needs an explicit destination-bound import")]
    LegacyImportRequired,
    /// The stored credential's sealing key is gone; it can be replaced or
    /// removed, never read.
    #[error("{}", crate::profile_security::KEY_MISSING)]
    KeyMissing,
}

/// A finite storage boundary; never a caller-supplied path or request label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageOperation {
    VerifyDirectory,
    CanonicalizeRoot,
    RootIdentity,
    ReadEntry,
    OpenEnvelope,
    CreateLock,
    ProtectLock,
    OpenLock,
    VerifyLock,
    LockMetadata,
    AcquireLock,
    ReadStaging,
    RemoveStaging,
    #[cfg(unix)]
    SyncDirectory,
    Seal,
    WriteEntry,
    RemoveEntry,
}

/// Keep only bounded diagnostics before dropping the original I/O error.
#[derive(Debug)]
pub struct StorageFailure {
    operation: StorageOperation,
    kind: std::io::ErrorKind,
    os_code: Option<i32>,
    protection: &'static str,
}
impl StorageFailure {
    fn capture(operation: StorageOperation, error: &std::io::Error) -> Self {
        Self {
            operation,
            kind: error.kind(),
            os_code: error.raw_os_error(),
            protection: crate::profile_security::failure_code(error),
        }
    }
}
impl std::fmt::Display for StorageFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} [operation={:?}", self.kind, self.operation)?;
        if let Some(code) = self.os_code {
            write!(f, " os={code}")?;
        }
        if self.protection != "io" {
            write!(f, " protection={}", self.protection)?;
        }
        f.write_str("]")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultKey {
    connection: ProviderId,
    file: String,
}
impl VaultKey {
    pub fn new(connection: ProviderId, file: &str) -> Result<Self, VaultError> {
        if !local(connection.as_str())
            || !local(file)
            || !(file.ends_with(".credential") || file.ends_with(".oauth.json"))
        {
            return Err(VaultError::InvalidInput);
        }
        Ok(Self {
            connection,
            file: file.into(),
        })
    }
}

fn local(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('.')
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Secret<T> redacts formatting; each retained payload additionally zeroizes.
pub struct VaultSecret(Secret<material::Material>);
impl std::fmt::Debug for VaultSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultSecret(<redacted>)")
    }
}
impl VaultSecret {
    pub fn azure_cli() -> Self {
        Self(Secret::new(material::Material::AzureCli))
    }
    pub fn vertex_adc() -> Self {
        Self(Secret::new(material::Material::VertexAdc))
    }
    pub fn aws_chain(selection: crate::aws::Selection) -> Result<Self, VaultError> {
        if !selection.valid() {
            return Err(VaultError::InvalidInput);
        }
        Ok(Self(Secret::new(material::Material::AwsChain {
            selection,
        })))
    }
    pub fn api_key(value: Zeroizing<String>) -> Result<Self, VaultError> {
        material::key(value).map(|value| Self(Secret::new(value)))
    }
    pub fn vertex(key: Zeroizing<String>, token: crate::vertex::Token) -> Result<Self, VaultError> {
        material::vertex(key, token).map(|value| Self(Secret::new(value)))
    }
    pub fn watsonx(
        key: Zeroizing<String>,
        token: crate::watsonx::Token,
    ) -> Result<Self, VaultError> {
        material::watsonx(key, token).map(|value| Self(Secret::new(value)))
    }
    pub fn oauth(tokens: crate::chatgpt::Tokens) -> Result<Self, VaultError> {
        material::oauth(tokens).map(|value| Self(Secret::new(value)))
    }
    pub fn expose(&self) -> CredentialView<'_> {
        self.0.expose().view()
    }
}

/// A borrow used only at the injection boundary; it has no formatter or codec.
#[allow(
    missing_debug_implementations,
    reason = "a secret view has no formatter"
)]
pub enum CredentialView<'a> {
    AzureCli,
    VertexAdc,
    AwsChain(&'a crate::aws::Selection),
    ApiKey(&'a str),
    WatsonxIam {
        key: &'a str,
        access: &'a str,
        expires_at: Timestamp,
    },
    VertexServiceAccount {
        key: &'a str,
        access: &'a str,
        expires_at: Timestamp,
    },
    OAuth {
        access: &'a str,
        refresh: &'a str,
        expires_at: Timestamp,
        account: &'a str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    WindowsCurrentUserEncryption,
    OwnerOnlyLegacyPlaintext,
}

#[derive(Debug)]
pub struct CredentialDescription {
    pub backend: BackendKind,
    pub last4: Option<String>,
    pub updated: Timestamp,
}

/// CRUD is synchronous. A service runs potentially blocking backend operations
/// in its bounded I/O owner; no storage lock may outlive the operation.
pub trait CredentialVault {
    fn put(
        &mut self,
        key: &VaultKey,
        binding: &DestinationBinding,
        secret: VaultSecret,
        updated: Timestamp,
    ) -> Result<(), VaultError>;
    fn get(
        &self,
        key: &VaultKey,
        binding: &DestinationBinding,
    ) -> Result<Option<VaultSecret>, VaultError>;
    fn remove(&mut self, key: &VaultKey) -> Result<bool, VaultError>;
    fn describe(
        &self,
        key: &VaultKey,
        binding: &DestinationBinding,
    ) -> Result<Option<CredentialDescription>, VaultError>;
}

#[cfg(test)]
mod diagnostic_tests;
#[cfg(test)]
mod tests;
