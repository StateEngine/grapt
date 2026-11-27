//! The opaque injected credential capability.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`.
//! The boundary is an opaque injected credential capability only: no credential
//! storage or authority conversion.
//!
//! # What "opaque injected capability" means here, mechanically
//!
//! **Injected.** There is no constructor that reads a file, an environment
//! variable, a keychain or a credential store. The only way a secret enters
//! this crate is an explicit injection, called by the credential owner
//! that already holds the material. This crate therefore cannot be the thing
//! that leaks a store, because it cannot open one.
//!
//! **Opaque.** [`ProviderCredential`] and [`CredentialLease`] derive nothing:
//!
//! - no `Clone`, because a clonable capability is a reusable admission ticket
//!   and the whole point of a per-attempt lease is that it is not;
//! - no `Debug`, because a formatter over a credential is a read channel —
//!   `Secret<T>` already redacts, so the absent formatter is a second guard
//!   rather than the only one;
//! - no `PartialEq`/`Eq`/`Hash`, because each is an offline equality oracle
//!   over secret material (amendment §8.4: "There is no public raw SHA-256
//!   secret fingerprint or other stable offline equality oracle");
//! - no `Serialize`, because a serialized capability is a leaked one.
//!
//! "Same credential?" is answered by [`CredentialLease::credential_version`],
//! never by the material. Header material is read only by private placement.
//! Request-authentication material remains behind the injected credential owner;
//! this crate receives only the authenticated request.
//!
//! **No authority conversion.** Holding this capability proves the caller was
//! given a credential. It does not decide which model, route or account the
//! attempt may use: that is the issuance the carrier checks
//! ([`crate::authority::ProviderAttemptCarrier`]'s `bind`, which is
//! crate-private for the reason recorded there), and a lease is one of the
//! coordinates checked *against* it, never a substitute for it. There is
//! deliberately no method here that returns a scope, a permission, a capability
//! set, or anything else a caller could treat as authorization.
//!
//! # Placement is closed, and belongs to the route
//!
//! Native Messages uses a closed API-key or bearer mode with the required
//! `anthropic-version` header. Bearer Messages additionally retains its complete
//! destination base on the credential. OpenAI-shaped routes use bearer tokens;
//! a configuration value cannot name an arbitrary authentication header.

use grapt_types::{AccountId, CredentialVersion, Secret};
use std::sync::Arc;

mod request_auth;
pub use request_auth::{RequestAuthenticationFuture, RequestAuthenticator};
mod destination;
mod messages;
pub use messages::{messages_placement, MessagesAuthMode};
mod renewal;
pub use renewal::{
    CredentialOwner, CredentialRenewalFuture, CredentialRenewalRefusal, CredentialRevision,
    CurrentCredential,
};

/// The credential owner may retire future leases without exposing material.
/// Validation performs no provider request and never changes binding coordinates.
pub trait CredentialUseAuthority: Send + Sync {
    fn validate(&self) -> Result<(), CredentialError>;
}

/// How a route places its credential on the wire.
///
/// Closed and route-owned. There is no `Custom(String)` variant: a caller
/// cannot invent a header name, and no configuration surface selects one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialPlacement {
    GoogleApiKey,
    /// AWS Signature Version 4 over the final Bedrock request.
    AwsSigV4,
    /// Microsoft Entra ID authorization for native Azure resources.
    AzureEntra,
    /// Google application-default authorization for native Vertex.
    VertexAdc,
    /// `Authorization: Bearer <secret>`. OpenAI-shaped routes and explicitly
    /// destination-bound bearer Messages connections.
    BearerAuthorization,
    /// `x-api-key: <secret>`. The default native Messages authentication mode.
    AnthropicApiKey,
    /// A token for Cloudflare's native Responses or Messages gateway.
    CloudflareGateway,
}

/// A malformed credential injection, refused before any capability exists.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("the credential secret must not be empty")]
    SecretEmpty,
    /// A secret carrying a byte that cannot appear in an HTTP header value.
    ///
    /// Refused **here**, at injection, rather than at send: `reqwest` would
    /// reject the header later, and that failure would arrive as an opaque
    /// client error at the moment bytes were supposed to leave — indistinguishable
    /// from a transport fault, and classified as one. A credential that can
    /// never be placed is a configuration defect and says so.
    #[error("the credential secret contains a byte that cannot appear in an HTTP header value")]
    SecretNotHeaderSafe,
    #[error("credential destination is invalid")]
    InvalidDestination,
    #[error("credential is bound to a different destination")]
    DestinationMismatch,
    #[error("credential request could not be built")]
    InvalidRequest,
    #[error("connection was removed; connect a provider and pick a model")]
    Revoked,
    #[error("connection authority is unavailable")]
    UseAuthorityUnavailable,
    #[error("AWS Bedrock request signing failed")]
    AwsSigningFailed,
    #[error("Vertex ADC authorization failed")]
    VertexAuthFailed,
    #[error("Azure Entra authorization failed")]
    AzureAuthFailed,
}

enum Material {
    Header(Secret<String>),
    RequestAuthentication(Arc<dyn RequestAuthenticator>),
}

/// One injected credential, held by an opener for its lifetime and leased per
/// attempt. Derives nothing — see the module note.
#[allow(
    missing_debug_implementations,
    reason = "it owns secret material; a formatter over it is a read channel"
)]
pub struct ProviderCredential {
    account: AccountId,
    credential_version: CredentialVersion,
    placement: CredentialPlacement,
    material: Material,
    destination: Option<String>,
    messages: Option<messages::MessagesBinding>,
    use_authority: Option<Arc<dyn CredentialUseAuthority>>,
    cloudflare: Option<grapt_types::cloudflare::CloudflareGatewayScope>,
}

impl ProviderCredential {
    /// Inject a credential the caller already holds.
    ///
    /// This crate reads no store, so a credential exists
    /// here only because its owner passed it in, and the owner remains the
    /// only thing that can refresh or revoke it.
    pub fn inject(
        account: AccountId,
        credential_version: CredentialVersion,
        placement: CredentialPlacement,
        secret: Secret<String>,
    ) -> Result<Self, CredentialError> {
        validate(secret.expose())?;
        if matches!(
            placement,
            CredentialPlacement::AwsSigV4
                | CredentialPlacement::AzureEntra
                | CredentialPlacement::VertexAdc
        ) {
            return Err(CredentialError::InvalidRequest);
        }
        Ok(Self {
            account,
            credential_version,
            placement,
            material: Material::Header(secret),
            destination: None,
            messages: None,
            use_authority: None,
            cloudflare: None,
        })
    }

    /// Inject the selected Azure resource authorization owner.
    pub fn inject_azure(
        account: AccountId,
        credential_version: CredentialVersion,
        signer: Arc<dyn RequestAuthenticator>,
        destination: &str,
    ) -> Result<Self, CredentialError> {
        Self {
            account,
            credential_version,
            placement: CredentialPlacement::AzureEntra,
            material: Material::RequestAuthentication(signer),
            destination: None,
            messages: None,
            use_authority: None,
            cloudflare: None,
        }
        .bind_destination(destination)
    }

    /// Inject a destination-bound AWS signer held by the credential owner.
    pub fn inject_vertex_adc(
        account: AccountId,
        credential_version: CredentialVersion,
        signer: Arc<dyn RequestAuthenticator>,
        destination: &str,
    ) -> Result<Self, CredentialError> {
        Self {
            account,
            credential_version,
            placement: CredentialPlacement::VertexAdc,
            material: Material::RequestAuthentication(signer),
            destination: None,
            messages: None,
            use_authority: None,
            cloudflare: None,
        }
        .bind_destination(destination)
    }

    pub fn inject_aws(
        account: AccountId,
        credential_version: CredentialVersion,
        signer: Arc<dyn RequestAuthenticator>,
        destination: &str,
    ) -> Result<Self, CredentialError> {
        Self {
            account,
            credential_version,
            placement: CredentialPlacement::AwsSigV4,
            material: Material::RequestAuthentication(signer),
            destination: None,
            messages: None,
            use_authority: None,
            cloudflare: None,
        }
        .bind_destination(destination)
    }

    /// Attach the source owner's future-use check. Already-issued leases settle
    /// independently; every new lease must still belong to its connection.
    pub fn with_use_authority(mut self, authority: Arc<dyn CredentialUseAuthority>) -> Self {
        self.use_authority = Some(authority);
        self
    }

    /// Retain the storage owner's approved API origin. Once bound, an existing
    /// capability cannot be repurposed by editing the configured endpoint.
    pub fn bind_destination(mut self, url: &str) -> Result<Self, CredentialError> {
        let origin = destination::origin(url)?;
        if self
            .destination
            .as_ref()
            .is_some_and(|bound| bound != &origin)
        {
            return Err(CredentialError::DestinationMismatch);
        }
        self.destination = Some(origin);
        Ok(self)
    }

    pub(crate) fn keeps_destination(&self, lease: &CredentialLease) -> bool {
        self.destination == lease.destination
            && self.messages == lease.messages
            && self.cloudflare == lease.cloudflare
    }

    /// Capture the approved base together with the native Messages auth mode.
    pub fn bind_messages_destination(
        mut self,
        base: &str,
        mode: MessagesAuthMode,
    ) -> Result<Self, CredentialError> {
        if self.placement != mode.placement() {
            return Err(CredentialError::DestinationMismatch);
        }
        let binding = messages::MessagesBinding::new(base, mode)?;
        if self.messages.as_ref().is_some_and(|held| held != &binding) {
            return Err(CredentialError::DestinationMismatch);
        }
        self = self.bind_destination(base)?;
        self.messages = Some(binding);
        Ok(self)
    }
    pub(crate) fn messages_auth(&self) -> MessagesAuthMode {
        self.messages
            .as_ref()
            .map_or(MessagesAuthMode::ApiKey, |held| held.mode)
    }
    pub(crate) fn check_messages_base(&self, base: &str) -> Result<(), CredentialError> {
        match &self.messages {
            Some(binding) => binding.check_base(base),
            None => Ok(()),
        }
    }
    pub fn bind_cloudflare(
        mut self,
        scope: grapt_types::cloudflare::CloudflareGatewayScope,
    ) -> Result<Self, CredentialError> {
        if self
            .cloudflare
            .as_ref()
            .is_some_and(|bound| bound != &scope)
        {
            return Err(CredentialError::DestinationMismatch);
        }
        self.cloudflare = Some(scope);
        Ok(self)
    }
    pub(crate) fn cloudflare(&self) -> Option<&grapt_types::cloudflare::CloudflareGatewayScope> {
        self.cloudflare.as_ref()
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    pub fn credential_version(&self) -> &CredentialVersion {
        &self.credential_version
    }

    pub fn placement(&self) -> CredentialPlacement {
        self.placement
    }

    /// Mint one per-attempt lease from the held material.
    ///
    /// Crate-private. The re-wrap through [`Secret::expose`] is deliberate and
    /// auditable: `Secret<T>` is not `Clone`, so this is the one place in the
    /// crate where the material is copied, and it is copied into a value whose
    /// lifetime is one PhysicalAttempt.
    pub(crate) fn lease(&self) -> Result<CredentialLease, CredentialError> {
        if let Some(authority) = &self.use_authority {
            authority.validate()?;
        }
        let mut lease = match &self.material {
            Material::Header(secret) => CredentialLease::issue(
                self.account.clone(),
                self.credential_version.clone(),
                self.placement,
                Secret::new(secret.expose().clone()),
            )?,
            Material::RequestAuthentication(signer) => CredentialLease {
                account: self.account.clone(),
                credential_version: self.credential_version.clone(),
                placement: self.placement,
                material: Material::RequestAuthentication(signer.clone()),
                destination: None,
                messages: None,
                cloudflare: None,
            },
        };
        lease.destination = self.destination.clone();
        lease.messages = self.messages.clone();
        lease.cloudflare = self.cloudflare.clone();
        Ok(lease)
    }
}

/// One attempt's credential, held for exactly one PhysicalAttempt.
///
/// Derives nothing, for the reasons in the module note.
#[allow(
    missing_debug_implementations,
    reason = "a formatter over a credential lease is a read channel; see the module note"
)]
pub struct CredentialLease {
    account: AccountId,
    credential_version: CredentialVersion,
    placement: CredentialPlacement,
    material: Material,
    destination: Option<String>,
    messages: Option<messages::MessagesBinding>,
    cloudflare: Option<grapt_types::cloudflare::CloudflareGatewayScope>,
}

impl CredentialLease {
    pub(crate) fn messages_auth(&self) -> MessagesAuthMode {
        self.messages
            .as_ref()
            .map_or(MessagesAuthMode::ApiKey, |held| held.mode)
    }

    pub(crate) fn cloudflare(&self) -> Option<&grapt_types::cloudflare::CloudflareGatewayScope> {
        self.cloudflare.as_ref()
    }
    /// Crate-private issuance. The credential owner mints this through
    /// [`ProviderCredential::lease`]; an adapter consumes it. There is no
    /// public path from loose parts to a lease, which is what stops an adapter
    /// being handed material that was never injected through the capability.
    pub(crate) fn issue(
        account: AccountId,
        credential_version: CredentialVersion,
        placement: CredentialPlacement,
        secret: Secret<String>,
    ) -> Result<Self, CredentialError> {
        validate(secret.expose())?;
        if matches!(
            placement,
            CredentialPlacement::AwsSigV4
                | CredentialPlacement::AzureEntra
                | CredentialPlacement::VertexAdc
        ) {
            return Err(CredentialError::InvalidRequest);
        }
        Ok(Self {
            account,
            credential_version,
            placement,
            material: Material::Header(secret),
            destination: None,
            messages: None,
            cloudflare: None,
        })
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    pub fn credential_version(&self) -> &CredentialVersion {
        &self.credential_version
    }

    pub fn placement(&self) -> CredentialPlacement {
        self.placement
    }

    /// Redact a bounded local diagnostic without exposing credential material.
    pub(crate) fn error_preview(&self, bytes: &[u8]) -> crate::MaskedErrorText {
        match &self.material {
            Material::Header(secret) => crate::transport::error_text::mask(bytes, secret.expose()),
            // A chain has several rotating secrets, none available to this
            // crate, so shape-based masking cannot vouch for the body: withhold it.
            Material::RequestAuthentication(_) => {
                crate::transport::error_text::mask(b"Credential error body withheld", "")
            }
        }
    }

    /// Resolve owner-held authentication after the complete request is staged.
    pub(crate) async fn authenticate(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, CredentialError> {
        let Material::RequestAuthentication(signer) = &self.material else {
            return Ok(request);
        };
        let request = self.place(request)?;
        let (client, request) = request.build_split();
        let request = signer
            .authenticate(request.map_err(|_| CredentialError::InvalidRequest)?)
            .await?;
        Ok(reqwest::RequestBuilder::from_parts(client, request))
    }

    /// Place this credential on a request.
    ///
    /// Crate-private and **consuming-by-borrow with no return of material**:
    /// the secret is read here, written into one header, and nothing that
    /// contains it comes back. There is no `expose`, no `as_str`, and no
    /// `authorization()` accessor returning a `String` a caller could log — the
    /// previous Codex-local design had exactly such a private accessor, and
    /// raw credential material still has only a header as a destination.
    /// `error_preview` compares it privately and returns masked diagnostic text,
    /// never the material or a credential accessor.
    pub(crate) fn place(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, CredentialError> {
        // Inspect the actual request, including renewal's already-built body,
        // before reading the secret. Production clients refuse all redirects.
        let (client, request) = request.build_split();
        let request = request.map_err(|_| CredentialError::InvalidRequest)?;
        if let Some(expected) = &self.destination {
            if &destination::origin(request.url().as_str())? != expected {
                return Err(CredentialError::DestinationMismatch);
            }
        }
        if let Some(binding) = &self.messages {
            binding.check_url(request.url())?;
        }
        if let Some(scope) = &self.cloudflare {
            destination::cloudflare(scope, request.url())?;
        }
        let request = reqwest::RequestBuilder::from_parts(client, request);
        let Material::Header(secret) = &self.material else {
            return Ok(request);
        };
        let (name, text) = match self.placement {
            CredentialPlacement::AwsSigV4
            | CredentialPlacement::AzureEntra
            | CredentialPlacement::VertexAdc => return Err(CredentialError::InvalidRequest),
            CredentialPlacement::BearerAuthorization => {
                ("authorization", format!("Bearer {}", secret.expose()))
            }
            CredentialPlacement::AnthropicApiKey => ("x-api-key", secret.expose().to_owned()),
            CredentialPlacement::CloudflareGateway => (
                "cf-aig-authorization",
                format!("Bearer {}", secret.expose()),
            ),
            CredentialPlacement::GoogleApiKey => ("x-goog-api-key", secret.expose().to_owned()),
        };
        Ok(match reqwest::header::HeaderValue::from_str(&text) {
            Ok(mut value) => {
                value.set_sensitive(true);
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(reqwest::header::HeaderName::from_static(name), value);
                // Replace the staged header: appending would send both the
                // startup credential and its renewed replacement.
                request.headers(headers)
            }
            // Injection already validates this. Retain the request builder's
            // normal refusal if that invariant is ever violated.
            Err(_) => request.header(name, text),
        })
    }
}

/// The shared injection check.
///
/// Header-safety is checked against the HTTP field-value rule rather than
/// against any vendor's key format: this crate must not encode what an OpenAI
/// or Anthropic key looks like, because a provider that changes its key format
/// would then be refused by a guess this crate had no business making. What it
/// legitimately knows is that a header value cannot contain a control byte.
fn validate(secret: &str) -> Result<(), CredentialError> {
    if secret.is_empty() {
        return Err(CredentialError::SecretEmpty);
    }
    // Visible ASCII plus space and horizontal tab, per the HTTP field-value
    // rule. A byte outside it can never be placed, so it is refused now.
    if secret
        .bytes()
        .any(|byte| (byte < 0x20 && byte != b'\t') || byte == 0x7f)
    {
        return Err(CredentialError::SecretNotHeaderSafe);
    }
    Ok(())
}

impl crate::authority::AccountBoundLease for CredentialLease {
    fn account(&self) -> &AccountId {
        self.account()
    }

    fn credential_version(&self) -> &CredentialVersion {
        self.credential_version()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> AccountId {
        match AccountId::new("acct-1") {
            Some(account) => account,
            None => panic!("test account"),
        }
    }

    fn version() -> CredentialVersion {
        match CredentialVersion::new("v1") {
            Some(version) => version,
            None => panic!("test credential version"),
        }
    }

    fn inject(secret: &str) -> Result<ProviderCredential, CredentialError> {
        ProviderCredential::inject(
            account(),
            version(),
            CredentialPlacement::BearerAuthorization,
            Secret::new(secret.to_owned()),
        )
    }

    #[test]
    fn an_empty_secret_is_refused_at_injection() {
        assert_eq!(inject("").err(), Some(CredentialError::SecretEmpty));
    }

    #[test]
    fn a_control_byte_is_refused_at_injection_not_at_send() {
        // A newline in a header value is the classic header-injection shape.
        // Refusing at injection means the failure is a configuration defect
        // with a name, not an opaque client error at the moment bytes leave.
        assert_eq!(
            inject("sk-live\nx-evil: 1").err(),
            Some(CredentialError::SecretNotHeaderSafe)
        );
        assert_eq!(
            inject("sk-live\r\nhost: evil").err(),
            Some(CredentialError::SecretNotHeaderSafe)
        );
        assert_eq!(
            inject("sk\u{7f}live").err(),
            Some(CredentialError::SecretNotHeaderSafe)
        );
    }

    #[test]
    fn an_ordinary_secret_injects_and_leases() {
        let credential = match inject("sk-test-abc123") {
            Ok(credential) => credential,
            Err(error) => panic!("ordinary secret refused: {error}"),
        };
        assert_eq!(credential.account().as_str(), "acct-1");
        assert_eq!(credential.credential_version().as_str(), "v1");
        assert_eq!(
            credential.placement(),
            CredentialPlacement::BearerAuthorization
        );

        let lease = match credential.lease() {
            Ok(lease) => lease,
            Err(error) => panic!("lease refused: {error}"),
        };
        // The lease carries the SAME coordinates: the carrier compares these
        // against the issuance, so a lease that renamed them would make the
        // account and credential-version checks compare a value with itself.
        assert_eq!(lease.account().as_str(), "acct-1");
        assert_eq!(lease.credential_version().as_str(), "v1");
        assert_eq!(lease.placement(), CredentialPlacement::BearerAuthorization);
    }

    #[test]
    fn the_placement_travels_with_the_capability_and_is_not_chosen_later() {
        let credential = match ProviderCredential::inject(
            account(),
            version(),
            CredentialPlacement::AnthropicApiKey,
            Secret::new("sk-ant-test".to_owned()),
        ) {
            Ok(credential) => credential,
            Err(error) => panic!("anthropic secret refused: {error}"),
        };
        let lease = match credential.lease() {
            Ok(lease) => lease,
            Err(error) => panic!("lease refused: {error}"),
        };
        // There is no setter. A route cannot re-place a credential it was
        // handed, so an Anthropic key cannot be sent as a bearer token by a
        // caller that picked the wrong adapter — the capability decides.
        assert_eq!(lease.placement(), CredentialPlacement::AnthropicApiKey);
    }
}
