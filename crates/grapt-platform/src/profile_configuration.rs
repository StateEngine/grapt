//! A terminal's approved profile bytes, retained by the platform bootstrap owner.
//! Approval is checked against the captured bytes once, then every profile
//! configuration consumer reads that immutable view. No mutable pathname check
//! can authorize a later filesystem read. Other configuration keeps its reader.
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use grapt_config::{
    ConfigSourcePath, MaxDocumentBytes, SourceIoError, SourceProbe, SourceReader, StdSourceReader,
};
use grapt_types::profile_names::{Choice, Spelling};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CONFIGURATION: &[&str] = &[
    "grapt.json",
    "grapt.jsonc",
    "service.conf",
    "connections.json",
];
/// The same documents in a profile created before the product rename. Such a
/// profile is captured, approved and fingerprinted under these names exactly
/// as it was before, absent names included.
pub const LEGACY_CONFIGURATION: &[&str] = &[
    "secode.json",  // grapt-rename: keep
    "secode.jsonc", // grapt-rename: keep
    "service.conf",
    "connections.json",
];

/// The document names a profile of this spelling is captured under.
pub fn configuration_names(spelling: Spelling) -> &'static [&'static str] {
    match spelling {
        Spelling::Current => CONFIGURATION,
        Spelling::Legacy => LEGACY_CONFIGURATION,
    }
}

/// Which configuration spelling a profile directory uses: the current names
/// when either current file is present, the legacy names only when neither
/// current file is and a legacy one is, and the current names for a new one.
pub fn profile_spelling(profile: &Path) -> io::Result<Choice> {
    grapt_login::profile_files::profile_spelling(profile)
}
const DOCUMENT_BYTES: usize = 256 * 1024;
static ACTIVE: OnceLock<ProfileConfiguration> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileApproval {
    canonical: PathBuf,
    fingerprint: [u8; 32],
}

impl ProfileApproval {
    pub fn canonical(&self) -> &Path {
        &self.canonical
    }
    pub fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }
    pub fn encoded(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|_| "profile approval cannot be encoded".into())
    }
    pub fn decode(text: &str) -> Result<Self, String> {
        if text.len() > 32 * 1024 {
            return Err("profile approval exceeds its bound".into());
        }
        serde_json::from_str(text).map_err(|_| "profile approval is malformed".into())
    }
}

pub struct ProfileConfiguration {
    approval: ProfileApproval,
    #[cfg(windows)]
    directory: same_file::Handle,
    names: &'static [&'static str],
    documents: Vec<Option<Vec<u8>>>,
    connection_migration: OnceLock<Vec<u8>>,
    connection_changes: RwLock<Option<Vec<u8>>>,
}
impl std::fmt::Debug for ProfileConfiguration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProfileConfiguration(<captured>)")
    }
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn capture_file(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let before = match std::fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !before.is_file() || linked(&before) || before.len() > DOCUMENT_BYTES as u64 {
        return Err(io::Error::other(
            "profile trust requires bounded, regular, unlinked configuration files",
        ));
    }
    #[cfg(windows)]
    let before_identity = same_file::Handle::from_path(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000).share_mode(1);
    }
    let file = options.open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || linked(&opened) {
        return Err(io::Error::other(
            "profile configuration changed type while opening",
        ));
    }
    if path.file_name() == Some(std::ffi::OsStr::new("connections.json")) {
        grapt_login::profile_security::verify_owner_only_file(path, &file)?;
    }
    #[cfg(windows)]
    if before_identity != same_file::Handle::from_file(file.try_clone()?)? {
        return Err(io::Error::other(
            "profile configuration changed identity while opening",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != opened.dev() || before.ino() != opened.ino() {
            return Err(io::Error::other(
                "profile configuration changed identity while opening",
            ));
        }
    }
    let mut bytes = Vec::new();
    file.take(DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > DOCUMENT_BYTES {
        return Err(io::Error::other("profile configuration exceeds its bound"));
    }
    Ok(Some(bytes))
}

impl ProfileConfiguration {
    pub fn capture(profile: &Path) -> io::Result<Self> {
        let metadata = std::fs::symlink_metadata(profile)?;
        if !metadata.is_dir() || linked(&metadata) {
            return Err(io::Error::other(
                "profile trust requires a real profile directory",
            ));
        }
        let canonical = std::fs::canonicalize(profile)?;
        #[cfg(windows)]
        let directory = same_file::Handle::from_path(&canonical)?;
        let names = configuration_names(profile_spelling(&canonical)?.spelling);
        let mut hash = Sha256::new();
        hash.update(b"secode.repository-profile-trust.v1\0");
        let mut documents = Vec::with_capacity(names.len());
        for name in names {
            let bytes = capture_file(&canonical.join(name))?;
            hash.update(name.as_bytes());
            match &bytes {
                Some(bytes) => {
                    hash.update([1]);
                    hash.update((bytes.len() as u64).to_le_bytes());
                    hash.update(bytes);
                }
                None => hash.update([0]),
            }
            documents.push(bytes);
        }
        Ok(Self {
            approval: ProfileApproval {
                canonical,
                fingerprint: hash.finalize().into(),
            },
            names,
            documents,
            #[cfg(windows)]
            directory,
            connection_migration: OnceLock::new(),
            connection_changes: RwLock::new(None),
        })
    }
    pub fn approval(&self) -> &ProfileApproval {
        &self.approval
    }
    fn slot(&self, path: &Path) -> io::Result<Option<usize>> {
        let Some(parent) = path.parent() else {
            return Ok(None);
        };
        #[cfg(windows)]
        let bound = {
            let key = |path: &Path| {
                path.to_str()
                    .map(grapt_types::path::comparison_key)
                    .ok_or_else(|| io::Error::other("approved profile path is not UTF-8"))
            };
            if key(parent)? == key(self.approval.canonical())? {
                true
            } else {
                match same_file::Handle::from_path(parent) {
                    Ok(identity) => identity == self.directory,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                    Err(error) => return Err(error),
                }
            }
        };
        #[cfg(not(windows))]
        let bound = parent == self.approval.canonical();
        if !bound {
            return Ok(None);
        }
        let slot = |path: &Path| {
            self.names.iter().position(|name| {
                path.file_name().is_some_and(|filename| {
                    #[cfg(windows)]
                    {
                        filename
                            .to_str()
                            .is_some_and(|filename| filename.eq_ignore_ascii_case(name))
                    }
                    #[cfg(not(windows))]
                    {
                        filename == std::ffi::OsStr::new(name)
                    }
                })
            })
        };
        let index = slot(path);
        #[cfg(windows)]
        let index = match index {
            Some(index) => Some(index),
            // Resolve only a name, never live configuration bytes. A vanished
            // short alias refuses rather than falling back to a mutable reader.
            None => slot(&std::fs::canonicalize(path)?),
        };
        Ok(index)
    }

    fn read(&self, path: &Path, cap: usize) -> io::Result<FrozenRead> {
        let Some(index) = self.slot(path)? else {
            return Ok(FrozenRead::Unbound);
        };
        let connection;
        let bytes = if self.names[index] == "connections.json" {
            connection = self.effective_connections()?;
            connection.as_ref()
        } else {
            self.documents[index].as_ref()
        };
        match bytes {
            None => Ok(FrozenRead::Missing),
            Some(bytes) if bytes.len() > cap => Err(io::Error::other(
                "approved configuration exceeds consumer bound",
            )),
            Some(bytes) => Ok(FrozenRead::Bytes(bytes.clone())),
        }
    }

    fn effective_connections(&self) -> io::Result<Option<Vec<u8>>> {
        let changes = self
            .connection_changes
            .read()
            .map_err(|_| io::Error::other("connection configuration is unavailable"))?;
        Ok(changes
            .as_ref()
            .or(self.connection_migration.get())
            .cloned()
            .or_else(|| {
                self.names
                    .iter()
                    .position(|name| *name == "connections.json")
                    .and_then(|index| self.documents[index].clone())
            }))
    }
}

/// An authenticated connection operation may replace only the connection
/// document in the retained profile view. Other approved bytes remain frozen.
/// Prepare before staging secrets; consume only after the store's commit.
pub struct ConnectionChange {
    source: ConnectionSource,
    after: Vec<u8>,
}

struct ConnectionSource {
    active: Option<&'static ProfileConfiguration>,
    profile: PathBuf,
    before: Option<Vec<u8>>,
}

impl std::fmt::Debug for ConnectionChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ConnectionChange(<captured>)")
    }
}

impl ConnectionChange {
    /// Check the complete retained source before a submitted key can leave
    /// the service. Installation checks it again after network verification.
    pub fn check_source(
        profile: &Path,
        before: Option<&grapt_connections::store::Connections>,
    ) -> io::Result<()> {
        ConnectionSource::capture(profile, before).map(|_| ())
    }

    pub fn prepare(
        profile: &Path,
        before: Option<&grapt_connections::store::Connections>,
        after: &grapt_connections::store::Connections,
    ) -> io::Result<Self> {
        Ok(Self {
            source: ConnectionSource::capture(profile, before)?,
            after: after.to_vec()?,
        })
    }

    pub fn commit(self) -> io::Result<()> {
        let Some(active) = self.source.active else {
            return Ok(());
        };
        let current = ProfileConfiguration::capture(&self.source.profile)?;
        if !active.matches(&current, Some(&self.after)) {
            return Err(io::Error::other(
                "profile changed during connection installation",
            ));
        }
        let mut changes = active
            .connection_changes
            .write()
            .map_err(|_| io::Error::other("connection configuration is unavailable"))?;
        let initial = active
            .names
            .iter()
            .position(|name| *name == "connections.json")
            .and_then(|index| active.documents[index].as_ref());
        if changes
            .as_ref()
            .or(active.connection_migration.get())
            .or(initial)
            != self.source.before.as_ref()
        {
            return Err(io::Error::other("connection authorization was superseded"));
        }
        *changes = Some(self.after);
        Ok(())
    }
}

impl ConnectionSource {
    fn capture(
        profile: &Path,
        before: Option<&grapt_connections::store::Connections>,
    ) -> io::Result<Self> {
        let profile = std::fs::canonicalize(profile)?;
        let active = ACTIVE
            .get()
            .filter(|active| active.approval.canonical == profile);
        let captured = match active {
            Some(active) => {
                let captured = active.effective_connections()?;
                let expected = captured
                    .as_deref()
                    .map(grapt_connections::store::Connections::from_slice)
                    .transpose()?;
                if expected.as_ref() != before
                    || !active.matches(
                        &ProfileConfiguration::capture(&profile)?,
                        captured.as_deref(),
                    )
                {
                    return Err(io::Error::other(
                        "profile changed before connection authorization",
                    ));
                }
                captured
            }
            None => None,
        };
        Ok(Self {
            active,
            profile,
            before: captured,
        })
    }
}

/// Install before any configuration, store or service startup. A changed input
/// refuses; a successful install never consults those mutable files again.
pub fn install(profile: &Path, approval: &ProfileApproval) -> Result<(), String> {
    let captured = ProfileConfiguration::capture(profile)
        .map_err(|error| format!("approved profile capture refused: {:?}", error.kind()))?;
    if captured.approval() != approval {
        return Err("profile changed between authorization and consumption".into());
    }
    ACTIVE
        .set(captured)
        .map_err(|_| "profile configuration was already installed".into())
}

#[derive(Debug)]
pub enum FrozenRead {
    Unbound,
    Missing,
    Bytes(Vec<u8>),
}

/// Stable identity of a captured document, including captured absence.
/// Alias discovery never gives its mutable replacement a second catalog identity.
pub fn captured_identity(path: &Path) -> io::Result<Option<PathBuf>> {
    match ACTIVE.get() {
        Some(profile) => Ok(profile
            .slot(path)?
            .map(|index| profile.approval.canonical().join(profile.names[index]))),
        None => Ok(None),
    }
}

pub fn frozen(path: &Path, cap: usize) -> io::Result<FrozenRead> {
    match ACTIVE.get() {
        Some(profile) => profile.read(path, cap),
        None => Ok(FrozenRead::Unbound),
    }
}

/// The established SourceReader input shape, with a process's approved profile
/// view substituted only for the files covered by its user decision.
#[derive(Debug)]
pub struct ProfileSourceReader;
impl SourceReader for ProfileSourceReader {
    fn probe(&self, path: &ConfigSourcePath) -> Result<SourceProbe, SourceIoError> {
        match frozen(path.as_path(), DOCUMENT_BYTES)
            .map_err(|error| SourceIoError::Unreadable { kind: error.kind() })?
        {
            FrozenRead::Unbound => StdSourceReader.probe(path),
            FrozenRead::Missing => Err(SourceIoError::NotFound),
            FrozenRead::Bytes(bytes) => Ok(SourceProbe::new(bytes.len(), true)),
        }
    }
    fn read_bounded(
        &self,
        path: &ConfigSourcePath,
        bound: MaxDocumentBytes,
    ) -> Result<Vec<u8>, SourceIoError> {
        match frozen(path.as_path(), bound.value())
            .map_err(|error| SourceIoError::Unreadable { kind: error.kind() })?
        {
            FrozenRead::Unbound => StdSourceReader.read_bounded(path, bound),
            FrozenRead::Missing => Err(SourceIoError::NotFound),
            FrozenRead::Bytes(bytes) => Ok(bytes),
        }
    }
}

/// Project templates require this process's content-bound repository approval.
/// A globally configured service never grants trust to an opened repository.
pub fn repository_is_approved(repository: &Path, source: &Path) -> bool {
    ACTIVE
        .get()
        .is_some_and(|active| approved_source(active, repository, source))
}
fn approved_source(active: &ProfileConfiguration, repository: &Path, source: &Path) -> bool {
    let Ok(root) = std::fs::canonicalize(repository) else {
        return false;
    };
    let profile = active.approval.canonical();
    let project = |spelling: Spelling| root.join(spelling.project_dir());
    if profile != root
        && *profile != project(Spelling::Current)
        && *profile != project(Spelling::Legacy)
    {
        return false;
    }
    let Ok(source) = std::fs::canonicalize(source) else {
        return false;
    };
    if !source.starts_with(&root) {
        return false;
    }
    active.effective_connections().is_ok_and(|connection| {
        ProfileConfiguration::capture(profile)
            .is_ok_and(|current| active.matches(&current, connection.as_deref()))
    })
}

impl ProfileConfiguration {
    fn matches(&self, current: &Self, connection: Option<&[u8]>) -> bool {
        self.approval.canonical == current.approval.canonical
            && self.names == current.names
            && self.names.iter().enumerate().all(|(index, name)| {
                let expected = if *name == "connections.json" {
                    connection.or(self.documents[index].as_deref())
                } else {
                    self.documents[index].as_deref()
                };
                current.documents[index].as_deref() == expected
            })
    }
}

/// The configuration owner may authorize one derived migration from approved
/// absence. Verify the complete current profile against captured bytes plus
/// that exact generated document; unrelated edits never inherit the approval.
/// This crate-private seam accepts no caller-selected configuration source.
pub(crate) fn record_connection_migration(profile: &Path, bytes: &[u8]) -> io::Result<()> {
    let Some(active) = ACTIVE.get() else {
        return Ok(());
    };
    let canonical = std::fs::canonicalize(profile)?;
    if canonical != active.approval.canonical {
        return Ok(());
    }
    let index = active
        .names
        .iter()
        .position(|name| *name == "connections.json")
        .ok_or_else(|| io::Error::other("connection capture is unavailable"))?;
    if active.documents[index].is_some() || bytes.len() > 64 * 1024 {
        return Err(io::Error::other(
            "connection migration did not start from approved absence",
        ));
    }
    let current = ProfileConfiguration::capture(profile)?;
    if !active.matches(&current, Some(bytes)) {
        return Err(io::Error::other(
            "profile changed during approved connection migration",
        ));
    }
    if let Some(previous) = active.connection_migration.get() {
        if previous == bytes {
            return Ok(());
        }
        return Err(io::Error::other(
            "a different connection migration was already approved",
        ));
    }
    active
        .connection_migration
        .set(bytes.to_vec())
        .map_err(|_| io::Error::other("connection migration was already approved"))
}

#[cfg(test)]
mod command_trust_tests {
    use super::*;

    #[test]
    fn explicit_connection_changes_preserve_frozen_configuration_and_reject_unrelated_edits(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use grapt_connections::store::Store;
        let root =
            std::env::temp_dir().join(format!("grapt-connection-trust-{}", std::process::id()));
        let profile = root.join(".grapt");
        std::fs::create_dir_all(&profile)?;
        grapt_login::profile_security::protect_directory(&profile)?;
        // The approved-terminal entrypoint passes this canonical root to all
        // consumers; Windows adds its verbatim prefix during canonicalization.
        let profile = std::fs::canonicalize(&profile)?;
        let config = profile.join("grapt.jsonc");
        std::fs::write(&config, b"{\"commands\":{}}")?;
        let mut store = Store::open(&profile)?;
        let mut records = store.migrate(br#"{"provider":{"id":"openai","model":"model-a","protocol_family":"openai-responses","route":"responses","account":"account-a","credential_version":"v1","policy_version":"p1"}}"#)?;
        let captured = ProfileConfiguration::capture(&profile)?;
        install(&profile, captured.approval()).map_err(io::Error::other)?;
        let original = records.clone();
        ConnectionChange::check_source(&profile, Some(&original))?;
        records.connections[0].label = "New key".into();
        assert!(ConnectionChange::prepare(&profile, Some(&records), &records).is_err());
        let change = ConnectionChange::prepare(&profile, Some(&original), &records)?;
        std::fs::write(&config, b"{\"commands\":{\"unapproved\":{}}}")?;
        assert!(ConnectionChange::check_source(&profile, Some(&original)).is_err());
        assert!(ConnectionChange::prepare(&profile, Some(&original), &records).is_err());
        store.save(&records)?;
        assert!(change.commit().is_err());
        let FrozenRead::Bytes(bytes) = frozen(&profile.join("connections.json"), 64 * 1024)? else {
            return Err(io::Error::other("approved connection view is missing").into());
        };
        assert_eq!(
            grapt_connections::store::Connections::from_slice(&bytes)?,
            original
        );
        assert!(ConnectionChange::prepare(&profile, Some(&records), &records).is_err());
        std::fs::write(&config, b"{\"commands\":{}}")?;
        store.save(&original)?;
        let change = ConnectionChange::prepare(&profile, Some(&original), &records)?;
        let superseded = ConnectionChange::prepare(&profile, Some(&original), &records)?;
        store.save(&records)?;
        change.commit()?;
        assert!(superseded.commit().is_err());
        let first = records.clone();
        records.connections[0].label = "Replacement key".into();
        let change = ConnectionChange::prepare(&profile, Some(&first), &records)?;
        store.save(&records)?;
        change.commit()?;
        let FrozenRead::Bytes(bytes) = frozen(&profile.join("connections.json"), 64 * 1024)? else {
            return Err(io::Error::other("updated connection view is missing").into());
        };
        assert_eq!(
            grapt_connections::store::Connections::from_slice(&bytes)?,
            records
        );
        assert!(
            matches!(frozen(&config, DOCUMENT_BYTES)?, FrozenRead::Bytes(bytes) if bytes == b"{\"commands\":{}}")
        );
        drop(store);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn repository_commands_require_exact_profile_contents_and_source_scope(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("grapt-command-trust-{}", std::process::id()));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        let repository = root.join("repository");
        let profile = repository.join(".grapt");
        std::fs::create_dir_all(&profile)?;
        let config = profile.join("grapt.jsonc");
        std::fs::write(&config, b"{\"commands\":{}}")?;
        let command = profile.join("command.md");
        std::fs::write(&command, b"review $ARGUMENTS")?;
        let approved = ProfileConfiguration::capture(&profile)?;
        assert!(approved_source(&approved, &repository, &command));
        assert!(
            !approved_source(&approved, &root, &command),
            "parent repository inherited trust"
        );
        let outside = root.join("outside.md");
        std::fs::write(&outside, b"untrusted")?;
        assert!(!approved_source(&approved, &repository, &outside));
        std::fs::write(&config, b"{\"commands\":{\"changed\":{}}}")?;
        assert!(
            !approved_source(&approved, &repository, &command),
            "changed profile retained trust"
        );
        std::fs::write(&config, b"{\"commands\":{}}")?;
        assert!(approved_source(&approved, &repository, &command));
        let global = root.join("global");
        std::fs::create_dir(&global)?;
        assert!(!approved_source(
            &ProfileConfiguration::capture(&global)?,
            &repository,
            &command
        ));
        #[cfg(unix)]
        {
            let linked = profile.join("linked.md");
            std::os::unix::fs::symlink(&outside, &linked)?;
            assert!(!approved_source(&approved, &repository, &linked));
        }
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
