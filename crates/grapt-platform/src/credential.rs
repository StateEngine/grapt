//! Authority-owned conversion from one exact local dogfood credential source
//! into one opaque provider capability.
//!
//! This is deliberately not a general credential catalog or secure store. The
//! platform owns one fixed `provider.credential` input, binds it to the exact
//! canonical service root and to a typed [`CredentialGrant`], and releases the
//! resulting [`OpenerCredential`] only after a live catalog-issued target agrees
//! with every grant coordinate. The authority is consuming: a second release is
//! rejected by the Rust move checker rather than represented as a runtime race.

use std::ffi::OsStr;
use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
use std::path::Path;

use grapt_provider::{CredentialPlacement, OpenerCredential, ProtocolFamily};
use grapt_types::{
    AccountId, CatalogGeneration, CredentialRecordId, CredentialStorageMode, CredentialVersion,
    ModelRouteId, ProviderId, ResolvedModelTarget, Secret,
};

use crate::config::CredentialGrant;

/// Finite capture ceiling for provider credentials.
///
/// D0's first real-product launch proved that the former 256-byte ceiling
/// rejected a current 1,849-byte Codex OAuth access token before bind. 4 KiB
/// admits that bounded credential class while retaining the exact-size,
/// bounded-read, source-identity, and secret-containment checks below.
pub const MAX_CREDENTIAL_BYTES: usize = 4 * 1_024;
const MAX_STORED_CREDENTIAL_BYTES: usize =
    grapt_login::profile_security::stored_bound(MAX_CREDENTIAL_BYTES);
const CREDENTIAL_FILE: &str = "provider.credential";

mod adc;
mod aws;
mod azure;
mod connection_use;
mod renewal;
mod vault;
pub use renewal::ReloadableCredentialAuthority;

/// A validated credential source file name directly inside the service root.
///
/// Configuration may name WHICH bounded local input carries a second account's
/// credential; it may never name where the platform is allowed to read. A
/// separator, drive prefix, parent hop, dotfile or foreign suffix is refused
/// here, before any path is built, so the exact-source identity checks in
/// [`open_exact_source`] keep the same meaning they had for the one fixed name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSourceName(String);

impl CredentialSourceName {
    /// The established primary input, unchanged from the single-account form.
    pub(crate) fn primary() -> Self {
        Self(CREDENTIAL_FILE.to_owned())
    }

    pub(crate) fn new(name: &str) -> Option<Self> {
        let plain = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            && !name.starts_with('.')
            && !name.contains("..")
            && !name.contains([':', '/', '\\'])
            && name.ends_with(".credential");
        plain.then(|| Self(name.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum CredentialLoadBoundary {
    AfterOpenIdentity,
    AfterBoundedRead,
}

#[cfg(test)]
type TestLoadHook = Box<dyn FnMut(CredentialLoadBoundary)>;

#[cfg(test)]
std::thread_local! {
    static TEST_LOAD_HOOK: std::cell::RefCell<Option<TestLoadHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn observe_load_boundary(boundary: CredentialLoadBoundary) {
    TEST_LOAD_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(boundary);
        }
    });
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialRefusal {
    #[error("the credential service root must be absolute")]
    RootNotAbsolute,
    #[error("the credential service root metadata could not be read: {0}")]
    RootMetadata(std::io::Error),
    #[error("the credential service root is not a directory")]
    RootNotDirectory,
    #[error("the credential service root is a link or reparse point")]
    RootLinked,
    #[error("the credential service root could not be resolved: {0}")]
    RootResolve(std::io::Error),
    #[error("the credential service root changed after its source was bound")]
    RootChanged,
    #[error("provider credential is absent")]
    Absent,
    #[error("{0}")]
    GoogleAdc(#[source] grapt_login::vertex::adc::AdcError),
    #[error("provider credential is not a regular file")]
    NotAFile,
    #[error("provider credential is a link or reparse point")]
    SourceLinked,
    #[error("provider credential escaped the exact service root or fixed source name")]
    SourceEscaped,
    #[error("provider credential metadata could not be read: {0}")]
    Metadata(std::io::Error),
    #[error("provider credential changed while its bounded source was being acquired")]
    SourceChanged,
    #[error("provider credential exceeds the {bound}-byte capture bound")]
    TooLarge { bound: usize },
    #[error("provider credential could not be read: {0}")]
    Read(std::io::Error),
    #[error("provider credential is not UTF-8")]
    Encoding,
    #[error("provider credential is empty")]
    Empty,
    #[error("the environment variable {name} that holds this profile's credential is not set")]
    EnvironmentAbsent { name: String },
    #[error("the credential grant's declared placement cannot authenticate its protocol")]
    GrantPlacementMismatch,
    #[error("the credential grant is not the explicit plaintext local-dogfood source")]
    GrantStorageMismatch,
    #[error("credential resolution used another credential record")]
    RecordMismatch,
    #[error("credential resolution used another provider")]
    ProviderMismatch,
    #[error("credential resolution used another protocol family")]
    ProtocolMismatch,
    #[error("credential resolution used another model route")]
    RouteMismatch,
    #[error("credential resolution used another account")]
    AccountMismatch,
    #[error("credential resolution used another credential version")]
    VersionMismatch,
    #[error("credential resolution used another declared placement")]
    PlacementMismatch,
    #[error("credential resolution used stale catalog issuance {issued_at}; current is {current}")]
    StaleCapability {
        issued_at: CatalogGeneration,
        current: CatalogGeneration,
    },
    #[error("credential resolution used a target from another catalog issuance")]
    ForeignIssuance,
    #[error("the credential owner's revision limit was reached")]
    RenewalExhausted,
    #[error("store-free provider credentials cannot enable credential reload or profile renewal")]
    RenewalDisabled,
    #[error("credential vault refused: {0}")]
    Vault(grapt_login::vault::VaultError),
    #[error("provider credential capability injection was refused: {0}")]
    Capability(grapt_provider::CredentialError),
}

/// The independent use-time coordinates presented to the credential authority.
///
/// The target is the catalog owner's live issuance. Record, protocol and
/// placement come from the consuming provider route. This value grants nothing:
/// it is accepted only when the authority's held grant agrees exactly.
#[allow(
    missing_debug_implementations,
    reason = "the request borrows a non-formattable resolved-target authority proof"
)]
pub struct CredentialResolutionRequest<'a> {
    target: &'a ResolvedModelTarget,
    record: CredentialRecordId,
    protocol_family: ProtocolFamily,
    placement: CredentialPlacement,
}

impl<'a> CredentialResolutionRequest<'a> {
    pub fn new(
        target: &'a ResolvedModelTarget,
        record: CredentialRecordId,
        protocol_family: ProtocolFamily,
        placement: CredentialPlacement,
    ) -> Self {
        Self {
            target,
            record,
            protocol_family,
            placement,
        }
    }
}

struct HeldGrant {
    record: CredentialRecordId,
    provider: ProviderId,
    protocol_family: ProtocolFamily,
    route: ModelRouteId,
    account: AccountId,
    version: CredentialVersion,
    placement: CredentialPlacement,
    cloudflare: Option<grapt_types::cloudflare::CloudflareGatewayScope>,
    issuance: ResolvedModelTarget,
    messages_auth: grapt_provider::MessagesAuthMode,
}

/// One platform-owned credential grant and its opaque provider capability.
///
/// There is no public loader: downstream code cannot point the authority at an
/// arbitrary file. The only constructor is the platform bootstrap, after its
/// configuration owner has issued the [`CredentialGrant`].
///
/// Duplicate release is structurally refused because [`Self::resolve`] consumes
/// `self`:
///
/// ```compile_fail
/// use grapt_platform::{CredentialAuthority, CredentialResolutionRequest};
/// use grapt_provider::{CredentialPlacement, ProtocolFamily};
/// use grapt_types::{CredentialRecordId, ResolvedModelTarget};
///
/// fn release_twice(
///     authority: CredentialAuthority,
///     target: &ResolvedModelTarget,
///     record: CredentialRecordId,
/// ) {
///     let first = CredentialResolutionRequest::new(
///         target,
///         record.clone(),
///         ProtocolFamily::ChatGptCodex,
///         CredentialPlacement::BearerAuthorization,
///     );
///     let second = CredentialResolutionRequest::new(
///         target,
///         record,
///         ProtocolFamily::ChatGptCodex,
///         CredentialPlacement::BearerAuthorization,
///     );
///     let _capability = authority.resolve(first);
///     let _duplicate = authority.resolve(second);
/// }
/// ```
///
/// The source loader is platform-private, so authored coordinates plus an
/// arbitrary path cannot mint an authority:
///
/// ```compile_fail
/// use std::path::Path;
/// use grapt_platform::{CredentialAuthority, CredentialGrant};
///
/// fn forge(root: &Path, grant: &CredentialGrant) {
///     let _authority = CredentialAuthority::load(root, grant);
/// }
/// ```
#[allow(
    missing_debug_implementations,
    reason = "credential authority owns secret material and must have no formatter"
)]
pub struct CredentialAuthority {
    credential: OpenerCredential,
    grant: HeldGrant,
    source: Option<renewal::BoundSource>,
}

/// Compatibility name for the former custody-only surface. It now carries the
/// consuming authority resolver; the private loader remains private through the
/// alias.
pub type CredentialCustody = CredentialAuthority;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSettlement {
    record: CredentialRecordId,
    account: AccountId,
    version: CredentialVersion,
    settled: bool,
}

impl CredentialSettlement {
    pub fn record(&self) -> &CredentialRecordId {
        &self.record
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    pub fn version(&self) -> &CredentialVersion {
        &self.version
    }

    pub fn is_settled(&self) -> bool {
        self.settled
    }
}

impl CredentialAuthority {
    /// Explicit key replacement mints a new version. An accepted generation
    /// retains this captured key instead of re-reading an alternating vault slot.
    pub fn is_vaulted_api_key(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(renewal::BoundSource::is_vaulted_api_key)
    }

    /// Store-free composition may resolve the captured capability once, but
    /// must not enable a downstream profile-renewal owner through this source.
    pub(crate) fn disable_renewal(&mut self) {
        self.source = None;
    }

    pub(crate) fn load(
        service_root: &Path,
        grant: &CredentialGrant,
        issuance: ResolvedModelTarget,
    ) -> Result<Self, CredentialRefusal> {
        if matches!(
            grant.credential_format(),
            grapt_connections::store::CredentialFormat::GoogleAdc { .. }
        ) != (grant.placement() == CredentialPlacement::VertexAdc)
            || matches!(
                grant.credential_format(),
                grapt_connections::store::CredentialFormat::AwsChain { .. }
            ) != (grant.placement() == CredentialPlacement::AwsSigV4)
            || matches!(
                grant.credential_format(),
                grapt_connections::store::CredentialFormat::AzureCli { .. }
            ) != (grant.placement() == CredentialPlacement::AzureEntra)
        {
            return Err(CredentialRefusal::GrantPlacementMismatch);
        }
        if !grapt_provider::credential_can_authenticate(
            grant.protocol_family(),
            grant.credential_format().cloudflare(),
            messages_auth(grant.credential_format()),
            grant.placement(),
        ) {
            return Err(CredentialRefusal::GrantPlacementMismatch);
        }
        let source = renewal::BoundSource::new(service_root, grant)?;
        let credential = if let Some(credential) =
            source.request_credential(grant.account(), grant.version())?
        {
            credential
        } else {
            let mut material = source.capture()?;
            let retained = material.trim_end_matches(['\r', '\n']).len();
            material.truncate(retained);
            if material.is_empty() {
                return Err(CredentialRefusal::Empty);
            }
            let credential = OpenerCredential::inject(
                grant.account().clone(),
                grant.version().clone(),
                grant.placement(),
                Secret::new(material),
            )
            .map_err(CredentialRefusal::Capability)?;
            source.bind(credential)?
        };
        Ok(Self {
            credential,
            source: Some(source),
            grant: HeldGrant {
                record: grant.record().clone(),
                provider: grant.provider().clone(),
                protocol_family: grant.protocol_family(),
                route: grant.route().clone(),
                account: grant.account().clone(),
                version: grant.version().clone(),
                placement: grant.placement(),
                messages_auth: messages_auth(grant.credential_format()),
                cloudflare: grant.credential_format().cloudflare().cloned(),
                issuance,
            },
        })
    }

    /// The credential a `PlaintextFile` grant names: its exact bounded source,
    /// captured unchanged.
    fn file_material(
        service_root: &Path,
        source: &CredentialSourceName,
    ) -> Result<String, CredentialRefusal> {
        let mut file = open_exact_source(service_root, source)?;
        let before = file.metadata().map_err(CredentialRefusal::Metadata)?;
        if before.len() > MAX_STORED_CREDENTIAL_BYTES as u64 {
            return Err(CredentialRefusal::TooLarge {
                bound: MAX_CREDENTIAL_BYTES,
            });
        }
        let capture_capacity = usize::try_from(before.len())
            .map_or(MAX_STORED_CREDENTIAL_BYTES + 1, |length| {
                length.min(MAX_STORED_CREDENTIAL_BYTES + 1)
            });
        let mut bytes = Vec::with_capacity(capture_capacity);
        (&mut file)
            .take((MAX_STORED_CREDENTIAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(CredentialRefusal::Read)?;
        if bytes.len() > MAX_STORED_CREDENTIAL_BYTES {
            return Err(CredentialRefusal::TooLarge {
                bound: MAX_CREDENTIAL_BYTES,
            });
        }
        // Legacy plaintext is stored byte-for-byte, while the stored bound
        // carries DPAPI envelope headroom on Windows. An oversized legacy
        // source clears both stored checks and would surface as
        // Read(InvalidData) from the plaintext bound instead of TooLarge.
        if !grapt_login::profile_security::is_protected(&bytes)
            && bytes.len() > MAX_CREDENTIAL_BYTES
        {
            return Err(CredentialRefusal::TooLarge {
                bound: MAX_CREDENTIAL_BYTES,
            });
        }
        #[cfg(test)]
        observe_load_boundary(CredentialLoadBoundary::AfterBoundedRead);
        let after = file.metadata().map_err(CredentialRefusal::Metadata)?;
        if !capture_unchanged(&before, &after) || after.len() != bytes.len() as u64 {
            return Err(CredentialRefusal::SourceChanged);
        }
        let bytes = grapt_login::profile_security::open(&bytes, MAX_CREDENTIAL_BYTES)
            .map_err(CredentialRefusal::Read)?;
        String::from_utf8(bytes).map_err(|_| CredentialRefusal::Encoding)
    }

    /// The credential a `ProcessEnvironment` grant names: the user's exported
    /// variable, read now and bounded as a file is. Only its name is ever in
    /// configuration, and the value never enters an error (J01).
    fn environment_material(environment: Option<&str>) -> Result<String, CredentialRefusal> {
        let name = environment.ok_or(CredentialRefusal::GrantStorageMismatch)?;
        let value = std::env::var_os(name).ok_or_else(|| CredentialRefusal::EnvironmentAbsent {
            name: name.to_owned(),
        })?;
        if value.len() > MAX_CREDENTIAL_BYTES {
            return Err(CredentialRefusal::TooLarge {
                bound: MAX_CREDENTIAL_BYTES,
            });
        }
        value.into_string().map_err(|_| CredentialRefusal::Encoding)
    }

    pub fn record(&self) -> &CredentialRecordId {
        &self.grant.record
    }

    pub fn protocol_family(&self) -> ProtocolFamily {
        self.grant.protocol_family
    }

    pub fn placement(&self) -> CredentialPlacement {
        self.grant.placement
    }

    /// Resolve this grant once against independent live use-time coordinates.
    ///
    /// Staleness is checked first. Every mismatch destroys the authority on
    /// return, so a caller cannot probe one capability repeatedly until it finds
    /// a tuple that passes.
    pub fn resolve(
        self,
        request: CredentialResolutionRequest<'_>,
    ) -> Result<OpenerCredential, CredentialRefusal> {
        self.validate(&request)?;
        let credential = self.credential;
        Ok(match self.source {
            Some(source) => source.bind_use(credential),
            None => credential,
        })
    }

    fn validate(&self, request: &CredentialResolutionRequest<'_>) -> Result<(), CredentialRefusal> {
        if !request.target.is_current_issuance() {
            return Err(CredentialRefusal::StaleCapability {
                issued_at: request.target.issuance_generation(),
                current: request.target.current_catalog_generation(),
            });
        }
        if self.grant.record != request.record {
            return Err(CredentialRefusal::RecordMismatch);
        }
        if &self.grant.provider != request.target.provider() {
            return Err(CredentialRefusal::ProviderMismatch);
        }
        if self.grant.protocol_family != request.protocol_family {
            return Err(CredentialRefusal::ProtocolMismatch);
        }
        if &self.grant.route != request.target.route() {
            return Err(CredentialRefusal::RouteMismatch);
        }
        if &self.grant.account != request.target.account() {
            return Err(CredentialRefusal::AccountMismatch);
        }
        if &self.grant.version != request.target.credential_version() {
            return Err(CredentialRefusal::VersionMismatch);
        }
        if self.grant.placement != request.placement
            || !grapt_provider::credential_can_authenticate(
                request.protocol_family,
                self.grant.cloudflare.as_ref(),
                self.grant.messages_auth,
                request.placement,
            )
        {
            return Err(CredentialRefusal::PlacementMismatch);
        }
        if !self.grant.issuance.is_same_issuance(request.target) {
            return Err(CredentialRefusal::ForeignIssuance);
        }
        Ok(())
    }

    /// Settle an unused authority by dropping its opaque provider capability.
    /// Current Dogfood-1 composition uses this path until its consuming lane
    /// deliberately installs a real provider source.
    pub fn settle(self) -> CredentialSettlement {
        let Self {
            credential,
            grant,
            source: _,
        } = self;
        drop(credential);
        CredentialSettlement {
            record: grant.record,
            account: grant.account,
            version: grant.version,
            settled: true,
        }
    }
}

fn messages_auth(
    format: &grapt_connections::store::CredentialFormat,
) -> grapt_provider::MessagesAuthMode {
    match format {
        grapt_connections::store::CredentialFormat::MessagesBearer { .. } => {
            grapt_provider::MessagesAuthMode::Bearer
        }
        _ => grapt_provider::MessagesAuthMode::ApiKey,
    }
}
fn capture_unchanged(before: &Metadata, after: &Metadata) -> bool {
    before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
        && before.created().ok() == after.created().ok()
}

fn open_exact_source(
    service_root: &Path,
    name: &CredentialSourceName,
) -> Result<File, CredentialRefusal> {
    let name = name.as_str();
    if !service_root.is_absolute() {
        return Err(CredentialRefusal::RootNotAbsolute);
    }
    let root_metadata =
        std::fs::symlink_metadata(service_root).map_err(CredentialRefusal::RootMetadata)?;
    if link_or_reparse(service_root, &root_metadata)? {
        return Err(CredentialRefusal::RootLinked);
    }
    if !root_metadata.is_dir() {
        return Err(CredentialRefusal::RootNotDirectory);
    }
    let canonical_root =
        std::fs::canonicalize(service_root).map_err(CredentialRefusal::RootResolve)?;
    let source = canonical_root.join(name);
    if source.parent() != Some(canonical_root.as_path())
        || source.file_name() != Some(OsStr::new(name))
        || name.contains([':', '/', '\\'])
    {
        return Err(CredentialRefusal::SourceEscaped);
    }
    let before = match std::fs::symlink_metadata(&source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CredentialRefusal::Absent)
        }
        Err(error) => return Err(CredentialRefusal::Metadata(error)),
    };
    if link_or_reparse(&source, &before)? {
        return Err(CredentialRefusal::SourceLinked);
    }
    if !before.is_file() {
        return Err(CredentialRefusal::NotAFile);
    }
    if before.len() > MAX_STORED_CREDENTIAL_BYTES as u64 {
        return Err(CredentialRefusal::TooLarge {
            bound: MAX_CREDENTIAL_BYTES,
        });
    }
    let file = open_read_only(&source).map_err(CredentialRefusal::Read)?;
    let after_path = std::fs::symlink_metadata(&source).map_err(CredentialRefusal::Metadata)?;
    if link_or_reparse(&source, &after_path)? || !after_path.is_file() {
        return Err(CredentialRefusal::SourceChanged);
    }
    if !path_matches_open_file(&source, &file)? {
        return Err(CredentialRefusal::SourceChanged);
    }
    #[cfg(test)]
    observe_load_boundary(CredentialLoadBoundary::AfterOpenIdentity);
    let resolved = std::fs::canonicalize(&source).map_err(CredentialRefusal::Metadata)?;
    // The post-open rebinding check, against the SAME name the pre-open check
    // used. It compared against the one fixed file name until a second account
    // existed; leaving it that way would have refused every additional
    // credential as an escape, which is how the multi-account witnesses found it.
    if resolved.parent() != Some(canonical_root.as_path())
        || resolved.file_name() != Some(OsStr::new(name))
    {
        return Err(CredentialRefusal::SourceEscaped);
    }
    Ok(file)
}

fn open_read_only(path: &Path) -> Result<File, std::io::Error> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Permit other readers, but no writer/delete share while the bounded
        // capture is in progress. No ambient lock or retry loop is introduced.
        options.share_mode(0x0000_0001);
    }
    options.open(path)
}

fn path_matches_open_file(path: &Path, file: &File) -> Result<bool, CredentialRefusal> {
    let opened_identity =
        same_file::Handle::from_file(file.try_clone().map_err(CredentialRefusal::Read)?)
            .map_err(CredentialRefusal::Metadata)?;
    let path_identity = same_file::Handle::from_path(path).map_err(CredentialRefusal::Metadata)?;
    Ok(opened_identity == path_identity)
}

fn link_or_reparse(path: &Path, metadata: &Metadata) -> Result<bool, CredentialRefusal> {
    if metadata.file_type().is_symlink() {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let observed = std::fs::symlink_metadata(path).map_err(CredentialRefusal::Metadata)?;
        Ok(observed.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::PublishedConfig;

    struct LoadHookGuard;

    impl Drop for LoadHookGuard {
        fn drop(&mut self) {
            TEST_LOAD_HOOK.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }

    fn install_load_hook(hook: impl FnMut(CredentialLoadBoundary) + 'static) -> LoadHookGuard {
        TEST_LOAD_HOOK.with(|slot| {
            let prior = slot.borrow_mut().replace(Box::new(hook));
            assert!(
                prior.is_none(),
                "a credential load hook was already installed"
            );
        });
        LoadHookGuard
    }

    fn scratch(label: &str) -> Result<std::path::PathBuf, std::io::Error> {
        let root = std::env::temp_dir().join(format!(
            "grapt-credential-authority-unit-{label}-{}",
            std::process::id()
        ));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    fn write_loader_inputs(root: &Path, material: &[u8]) -> Result<(), std::io::Error> {
        std::fs::write(
            root.join("grapt.jsonc"),
            r#"{
  "store":{"authority":"open-ready"},
  "product":{"build":"credential-loader-witness"},
  "provider":{
    "id":"openai","model":"gpt-5.6","base_model":"gpt-5.6",
    "family":"gpt-5","route":"responses","account":"account-a",
    "credential_version":"credential-v1","policy_version":"df0-policy-v1",
    "variants":[]
  }
}"#,
        )?;
        std::fs::write(root.join(CREDENTIAL_FILE), material)
    }

    fn load_through_published_config(
        root: &Path,
    ) -> Result<CredentialAuthority, Box<dyn std::error::Error>> {
        let config = PublishedConfig::load(root)?;
        let issuance = config.issue_target()?;
        Ok(CredentialAuthority::load(
            root,
            config.credentials(),
            issuance,
        )?)
    }

    #[test]
    fn relative_root_and_ads_shaped_fixed_source_are_structurally_refused() {
        assert!(matches!(
            open_exact_source(
                Path::new("relative-service-root"),
                &CredentialSourceName::primary()
            ),
            Err(CredentialRefusal::RootNotAbsolute)
        ));
        assert!(
            !CREDENTIAL_FILE.contains([':', '/', '\\']),
            "the authority source name must not become an ADS or path"
        );
    }

    /// A configured second account may name WHICH bounded input carries its
    /// secret; it may never name where the platform reads. Every form that
    /// would leave the service root, reopen a dotfile, or address an alternate
    /// data stream is refused before a path is ever built.
    #[test]
    fn a_configured_credential_source_cannot_widen_where_the_platform_reads() {
        for name in [
            "../elsewhere.credential",
            "sub/dir.credential",
            "..\\escape.credential",
            "C:provider.credential",
            "provider.credential:$DATA",
            ".hidden.credential",
            "provider.txt",
            "",
        ] {
            assert!(
                CredentialSourceName::new(name).is_none(),
                "credential source {name:?} was admitted"
            );
        }
        assert_eq!(
            CredentialSourceName::new("openrouter.credential"),
            Some(CredentialSourceName("openrouter.credential".to_owned()))
        );
        assert_eq!(
            CredentialSourceName::new(CREDENTIAL_FILE),
            Some(CredentialSourceName::primary())
        );
    }

    #[test]
    fn an_open_handle_cannot_be_substituted_by_another_exact_path() {
        let root = match scratch("handle-substitution") {
            Ok(root) => root,
            Err(error) => panic!("scratch directory must exist: {error}"),
        };
        let held_path = root.join("held");
        let substituted_path = root.join("substituted");
        if let Err(error) = std::fs::write(&held_path, b"held") {
            panic!("held witness file must be written: {error}");
        }
        if let Err(error) = std::fs::write(&substituted_path, b"substituted") {
            panic!("substituted witness file must be written: {error}");
        }
        let held = match File::open(&held_path) {
            Ok(held) => held,
            Err(error) => panic!("held witness file must open: {error}"),
        };
        match path_matches_open_file(&substituted_path, &held) {
            Ok(false) => {}
            Ok(true) => panic!("a different path identity matched the held file"),
            Err(error) => panic!("identity comparison must execute: {error}"),
        }
        drop(held);
        if let Err(error) = std::fs::remove_dir_all(root) {
            panic!("scratch directory must be removed: {error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn same_length_post_read_source_change_is_refused() {
        const ORIGINAL: &[u8] = b"first-secret";
        const REPLACEMENT: &[u8] = b"other-secret";
        assert_eq!(ORIGINAL.len(), REPLACEMENT.len());

        let root = match scratch("same-length-post-read-change") {
            Ok(root) => root,
            Err(error) => panic!("scratch directory must exist: {error}"),
        };
        if let Err(error) = write_loader_inputs(&root, ORIGINAL) {
            panic!("loader inputs must be written: {error}");
        }
        let source = root.join(CREDENTIAL_FILE);
        let _hook = install_load_hook(move |boundary| {
            if boundary != CredentialLoadBoundary::AfterBoundedRead {
                return;
            }
            if let Err(error) = std::fs::write(&source, REPLACEMENT) {
                panic!("same-length post-read replacement must execute: {error}");
            }
            let changed = match OpenOptions::new().write(true).open(&source) {
                Ok(changed) => changed,
                Err(error) => panic!("changed credential must reopen: {error}"),
            };
            let fixed_modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
            let times = std::fs::FileTimes::new().set_modified(fixed_modified);
            if let Err(error) = changed.set_times(times) {
                panic!("changed credential timestamp must be deterministic: {error}");
            }
        });

        match load_through_published_config(&root) {
            Err(error)
                if error
                    .downcast_ref::<CredentialRefusal>()
                    .is_some_and(|refusal| matches!(refusal, CredentialRefusal::SourceChanged)) => {
            }
            Err(error) => panic!("same-length post-read source change produced {error}"),
            Ok(authority) => {
                drop(authority);
                panic!("same-length post-read source change did not refuse as SourceChanged");
            }
        }
        if let Err(error) = std::fs::remove_dir_all(root) {
            panic!("scratch directory must be removed: {error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn post_open_canonical_rebinding_is_refused() {
        let root = match scratch("post-open-rebinding-root") {
            Ok(root) => root,
            Err(error) => panic!("scratch directory must exist: {error}"),
        };
        let outside = match scratch("post-open-rebinding-outside") {
            Ok(root) => root,
            Err(error) => panic!("outside directory must exist: {error}"),
        };
        if let Err(error) = write_loader_inputs(&root, b"held-secret") {
            panic!("loader inputs must be written: {error}");
        }
        if let Err(error) = std::fs::write(outside.join(CREDENTIAL_FILE), b"outside-secret") {
            panic!("outside credential must be written: {error}");
        }
        let source = root.join(CREDENTIAL_FILE);
        let held = root.join("held.credential");
        let outside_source = outside.join(CREDENTIAL_FILE);
        let _hook = install_load_hook(move |boundary| {
            if boundary != CredentialLoadBoundary::AfterOpenIdentity {
                return;
            }
            if let Err(error) = std::fs::rename(&source, &held) {
                panic!("opened credential must move before canonical recheck: {error}");
            }
            if let Err(error) = std::os::unix::fs::symlink(&outside_source, &source) {
                panic!("outside credential must rebind the canonical path: {error}");
            }
        });

        match load_through_published_config(&root) {
            Err(error)
                if error
                    .downcast_ref::<CredentialRefusal>()
                    .is_some_and(|refusal| matches!(refusal, CredentialRefusal::SourceEscaped)) => {
            }
            Err(error) => panic!("post-open canonical rebinding produced {error}"),
            Ok(authority) => {
                drop(authority);
                panic!("post-open canonical rebinding did not refuse as SourceEscaped");
            }
        }
        if let Err(error) = std::fs::remove_dir_all(root) {
            panic!("scratch directory must be removed: {error}");
        }
        if let Err(error) = std::fs::remove_dir_all(outside) {
            panic!("outside directory must be removed: {error}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn held_handle_denies_windows_source_change_and_rebinding() {
        let root = match scratch("windows-held-handle") {
            Ok(root) => root,
            Err(error) => panic!("scratch directory must exist: {error}"),
        };
        if let Err(error) = write_loader_inputs(&root, b"held-secret") {
            panic!("loader inputs must be written: {error}");
        }
        let source = root.join(CREDENTIAL_FILE);
        let held = root.join("held.credential");
        let write_denied = std::rc::Rc::new(std::cell::Cell::new(false));
        let rebind_denied = std::rc::Rc::new(std::cell::Cell::new(false));
        let observed_write = std::rc::Rc::clone(&write_denied);
        let observed_rebind = std::rc::Rc::clone(&rebind_denied);
        let _hook = install_load_hook(move |boundary| match boundary {
            CredentialLoadBoundary::AfterOpenIdentity => {
                observed_rebind.set(std::fs::rename(&source, &held).is_err());
            }
            CredentialLoadBoundary::AfterBoundedRead => {
                observed_write.set(OpenOptions::new().write(true).open(&source).is_err());
            }
        });

        let authority = match load_through_published_config(&root) {
            Ok(authority) => authority,
            Err(error) => panic!("locked Windows credential must load unchanged: {error}"),
        };
        drop(authority);
        assert!(
            rebind_denied.get(),
            "Windows held credential allowed post-open path rebinding"
        );
        assert!(
            write_denied.get(),
            "Windows held credential allowed post-read source change"
        );
        if let Err(error) = std::fs::remove_dir_all(root) {
            panic!("scratch directory must be removed: {error}");
        }
    }
}
