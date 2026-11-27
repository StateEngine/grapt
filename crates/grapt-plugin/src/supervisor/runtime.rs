//! The runtime and bridge bytes a supervisor is allowed to execute.
//!
//! Admission verifies the packaged bytes before anything runs, copies them into
//! a service-owned private directory, and verifies the copy. Every spawn runs
//! only that copy and re-verifies it first. On Windows the copies are held open
//! without write or delete sharing for the supervisor's lifetime. Admission
//! counts each supervisor's claim on the directory; when the last claim of the
//! process is released the private copies and the empty directory levels the
//! admission created are removed, leaving the host's root standing if it still
//! holds other content. Release happens at supervisor drop: an explicit
//! shutdown retires the generation but keeps the claim, because the host can
//! prepare a successor generation from the private copies. At removal time
//! the generation is already retired, the Windows deny-delete holds are
//! already dropped by field order, and only then the directory goes; a
//! removal failure is reported on stderr, never swallowed.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::validation::required_environment;
use super::*;
use crate::source::sha256;

pub(super) struct VerifiedRuntime {
    // Declared first: fields drop in declaration order, so the Windows
    // deny-delete handles are released before the admission cleanup that
    // follows them removes the directory.
    #[cfg(windows)]
    _held: [std::fs::File; 2],
    _admission: AdmissionGuard,
    executable: PathBuf,
    bridge: PathBuf,
    pin: RuntimePin,
    identity: RuntimeIdentity,
}

impl core::fmt::Debug for VerifiedRuntime {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("VerifiedRuntime")
            .field("executable", &self.executable)
            .field("bridge", &self.bridge)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl VerifiedRuntime {
    pub(super) fn admit(runtime: &BridgeRuntime) -> Result<Self, PluginError> {
        validate_pin(&runtime.expected)?;
        let executable_bytes = read_verified(
            &runtime.executable,
            &runtime.expected.executable_sha256,
            "runtime executable",
        )?;
        let bridge_bytes = read_verified(
            &runtime.bridge,
            &runtime.expected.bridge_sha256,
            "bridge asset",
        )?;
        let directory = runtime
            .private_root
            .join(&runtime.expected.executable_sha256[..16])
            .join(&runtime.expected.bridge_sha256[..16]);
        register_admission(&directory)?;
        let admitted = admit_into(
            &directory,
            &runtime.expected,
            &executable_bytes,
            &bridge_bytes,
        );
        match admitted {
            Ok(value) => Ok(value),
            Err(error) => {
                if release_admission(&directory) {
                    if let Err(removal) = remove_private_runtime(&directory) {
                        report_removal_failure(&directory, &removal);
                    }
                }
                Err(error)
            }
        }
    }

    pub(super) fn identity(&self) -> &RuntimeIdentity {
        &self.identity
    }

    pub(super) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(super) fn bridge(&self) -> &Path {
        &self.bridge
    }

    /// Prove the private copies still hold the pinned bytes immediately
    /// before a spawn uses them.
    pub(super) fn verify_before_spawn(&self) -> Result<(), PluginError> {
        read_verified(
            &self.executable,
            &self.pin.executable_sha256,
            "runtime executable",
        )?;
        read_verified(&self.bridge, &self.pin.bridge_sha256, "bridge asset")?;
        Ok(())
    }
}

fn admit_into(
    directory: &Path,
    pin: &RuntimePin,
    executable_bytes: &[u8],
    bridge_bytes: &[u8],
) -> Result<VerifiedRuntime, PluginError> {
    create_private_directory(directory)?;
    let executable_name = if cfg!(windows) { "node.exe" } else { "node" };
    let executable = install_copy(
        &directory.join(executable_name),
        executable_bytes,
        &pin.executable_sha256,
        true,
    )?;
    let bridge = install_copy(
        &directory.join("bridge.mjs"),
        bridge_bytes,
        &pin.bridge_sha256,
        false,
    )?;
    #[cfg(windows)]
    let held = [
        hold(&executable, &pin.executable_sha256, "runtime executable")?,
        hold(&bridge, &pin.bridge_sha256, "bridge asset")?,
    ];
    let version = probe_version(&executable)?;
    if version != pin.version {
        return Err(PluginError::Runtime(format!(
            "runtime version {version} does not match the packaged pin {}",
            pin.version
        )));
    }
    Ok(VerifiedRuntime {
        #[cfg(windows)]
        _held: held,
        _admission: AdmissionGuard(directory.to_path_buf()),
        identity: RuntimeIdentity {
            executable: executable.clone(),
            version,
            sha256: pin.executable_sha256.clone(),
        },
        executable: child_path(&executable),
        bridge: child_path(&bridge),
        pin: pin.clone(),
    })
}

/// The process-wide admission count per private runtime directory. Every
/// supervisor that admits the same pin into the same root shares the same
/// copies, so the copies are removed only when the last admission of this
/// process for that directory is released.
static ADMISSIONS: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();

fn admissions() -> &'static Mutex<HashMap<PathBuf, u64>> {
    ADMISSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_admission(directory: &Path) -> Result<(), PluginError> {
    let mut held = admissions().lock().map_err(|_| PluginError::Poisoned)?;
    let count = held.entry(directory.to_path_buf()).or_insert(0);
    *count = count.checked_add(1).ok_or_else(|| {
        PluginError::Runtime("plugin runtime admission counter overflowed".to_owned())
    })?;
    Ok(())
}

/// Release one admission of `directory`; true when this was the last
/// admission of the process for that directory.
fn release_admission(directory: &Path) -> bool {
    let Ok(mut held) = admissions().lock() else {
        return false;
    };
    let Some(count) = held.get_mut(directory) else {
        return false;
    };
    *count = count.saturating_sub(1);
    if *count == 0 {
        held.remove(directory);
        true
    } else {
        false
    }
}

/// Remove the private copies and the directory levels an admission created,
/// stopping at the first level that still holds other content or is absent:
/// the host's root survives when it is shared. A real I/O failure is
/// returned so the caller can report it.
fn remove_private_runtime(directory: &Path) -> Result<(), std::io::Error> {
    std::fs::remove_dir_all(directory)?;
    for parent in directory.ancestors().skip(1).take(2) {
        match std::fs::remove_dir(parent) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn report_removal_failure(directory: &Path, error: &std::io::Error) {
    grapt_diagnostics::report(format!(
        "grapt-plugin: failed to remove the admitted private runtime directory {}: {error}",
        directory.display()
    ));
}

/// Releases the admission claim on the private directory this runtime
/// occupies, removing the copies when the last claim goes away.
struct AdmissionGuard(PathBuf);

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        if release_admission(&self.0) {
            if let Err(error) = remove_private_runtime(&self.0) {
                report_removal_failure(&self.0, &error);
            }
        }
    }
}

pub(super) fn validate_pin(pin: &RuntimePin) -> Result<(), PluginError> {
    if !pin.version.starts_with("v24.") {
        return Err(PluginError::Runtime(
            "plugin runtime must use the supported Node 24 release line".to_owned(),
        ));
    }
    for (label, value) in [
        ("runtime", pin.executable_sha256.as_str()),
        ("bridge", pin.bridge_sha256.as_str()),
    ] {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(PluginError::Runtime(format!(
                "{label} digest pin is malformed"
            )));
        }
    }
    Ok(())
}

fn read_verified(path: &Path, expected: &str, label: &str) -> Result<Vec<u8>, PluginError> {
    let bytes = std::fs::read(path)
        .map_err(|error| PluginError::Runtime(format!("{label} could not be read: {error}")))?;
    if sha256(&bytes) != expected {
        return Err(PluginError::Runtime(format!(
            "{label} digest does not match the packaged pin"
        )));
    }
    Ok(bytes)
}

fn create_private_directory(directory: &Path) -> Result<(), PluginError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(directory).map_err(|error| PluginError::Runtime(error.to_string()))?;
    Ok(())
}

fn install_copy(
    path: &Path,
    bytes: &[u8],
    expected: &str,
    executable: bool,
) -> Result<PathBuf, PluginError> {
    if path.exists() {
        match std::fs::read(path) {
            Ok(existing) if sha256(&existing) == expected => {
                return std::fs::canonicalize(path)
                    .map_err(|error| PluginError::Runtime(error.to_string()));
            }
            _ => remove_private_file(path)?,
        }
    }
    let temporary = path.with_extension(format!("partial-{}", std::process::id()));
    let _ = remove_private_file(&temporary);
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o500 } else { 0o400 };
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
    }
    // Windows relies on the deny-write, deny-delete handles held for the
    // supervisor's lifetime instead of a read-only attribute.
    #[cfg(not(unix))]
    let _ = executable;
    std::fs::rename(&temporary, path).map_err(|error| PluginError::Runtime(error.to_string()))?;
    read_verified(path, expected, "private runtime copy")?;
    std::fs::canonicalize(path).map_err(|error| PluginError::Runtime(error.to_string()))
}

fn remove_private_file(path: &Path) -> Result<(), PluginError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PluginError::Runtime(format!(
            "stale private runtime copy could not be replaced: {error}"
        ))),
    }
}

#[cfg(windows)]
fn hold(path: &Path, expected: &str, label: &str) -> Result<std::fs::File, PluginError> {
    use std::io::Read as _;
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .map_err(|error| PluginError::Runtime(format!("{label} could not be held: {error}")))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| PluginError::Runtime(error.to_string()))?;
    if sha256(&bytes) != expected {
        return Err(PluginError::Runtime(format!(
            "{label} changed while it was being held"
        )));
    }
    Ok(file)
}

fn probe_version(executable: &Path) -> Result<String, PluginError> {
    let mut command = Command::new(child_path(executable));
    command
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    for (name, value) in required_environment() {
        command.env(name, value);
    }
    let output = command
        .output()
        .map_err(|error| PluginError::Runtime(error.to_string()))?;
    if !output.status.success() {
        return Err(PluginError::Runtime(
            "runtime version probe returned non-zero".to_owned(),
        ));
    }
    String::from_utf8(output.stdout)
        .map(|version| version.trim().to_owned())
        .map_err(|error| PluginError::Runtime(error.to_string()))
}
