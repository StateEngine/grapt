// SPDX-License-Identifier: MIT OR Apache-2.0
//! Discovery: `<state_home>/endpoint.json`, read only through the verified
//! private state directory and checked against its closed schema before
//! anything trusts it.
use crate::peer::Principal;
use crate::state_dir::StateDir;
use crate::wire::{Address, EndpointFile, NativePath};
use crate::{IpcError, MAX_ENDPOINT_BYTES};
use std::path::Path;

const PIPE_PREFIX: &str = r"\\.\pipe\";
const MAX_PIPE_UNITS: usize = 1024;
const MAX_VERSION_BYTES: usize = 128;

/// A host's endpoint, checked against its schema and its state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub file: EndpointFile,
}

impl Endpoint {
    /// The socket to connect to, when the host listens on a Unix socket.
    #[cfg(unix)]
    pub fn unix_socket(&self) -> Option<&Path> {
        use std::os::unix::ffi::OsStrExt;
        match &self.file.address {
            Address::UnixSocket {
                path: NativePath::UnixBytes { units },
            } => Some(Path::new(std::ffi::OsStr::from_bytes(units))),
            _ => None,
        }
    }
}

/// Read and check `<state_home>/endpoint.json`. The state directory must be
/// `principal`'s alone (0700) and the file owner-only (0600), both checked
/// on the opened handles, and a Unix socket must be a socket of the
/// principal's directly inside that directory. Windows refuses until its
/// access lists can be verified.
pub fn read_endpoint(state_home: &Path, principal: &Principal) -> Result<Endpoint, IpcError> {
    let directory = StateDir::open(state_home, principal).map_err(IpcError::Endpoint)?;
    let bytes = directory
        .read("endpoint.json", u64::from(MAX_ENDPOINT_BYTES))
        .map_err(IpcError::Endpoint)?;
    let file: EndpointFile = crate::frame::decode(&bytes)?;
    validate(&file).map_err(IpcError::Endpoint)?;
    match &file.address {
        Address::UnixSocket { path } => unix_socket(path, state_home, &directory),
        Address::WindowsPipe { name } => pipe_name(name),
    }
    .map_err(IpcError::Endpoint)?;
    Ok(Endpoint { file })
}

fn validate(file: &EndpointFile) -> Result<(), String> {
    if file.schema != 1 {
        return Err(format!("schema {} is not 1", file.schema));
    }
    if file.api.major != crate::API_VERSION.major {
        return Err(format!(
            "API {}.{} is not version 1",
            file.api.major, file.api.minor
        ));
    }
    if file.package_version.is_empty() || file.package_version.len() > MAX_VERSION_BYTES {
        return Err("package_version is empty or too long".into());
    }
    Ok(())
}

#[cfg(unix)]
fn unix_socket(path: &NativePath, state_home: &Path, directory: &StateDir) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let NativePath::UnixBytes { units } = path else {
        return Err("a Unix socket path is not in unix_bytes".into());
    };
    if units.is_empty() || units.contains(&0) {
        return Err("the socket path is empty or holds NUL".into());
    }
    let socket = Path::new(std::ffi::OsStr::from_bytes(units));
    let name = socket
        .file_name()
        .filter(|_| socket.parent() == Some(state_home))
        .ok_or("the socket is not directly inside the state directory")?;
    directory.socket(name)
}

#[cfg(not(unix))]
fn unix_socket(
    _path: &NativePath,
    _state_home: &Path,
    _directory: &StateDir,
) -> Result<(), String> {
    Err("a Unix socket endpoint cannot be reached from this platform".into())
}

fn pipe_name(name: &str) -> Result<(), String> {
    let segment = name
        .strip_prefix(PIPE_PREFIX)
        .ok_or_else(|| format!("the pipe name does not start with {PIPE_PREFIX}"))?;
    if segment.is_empty()
        || segment == "."
        || segment == ".."
        || segment.contains(['\0', '/', '\\'])
        || name.encode_utf16().count() > MAX_PIPE_UNITS
    {
        return Err("the pipe name is not one valid segment within 1024 UTF-16 units".into());
    }
    Ok(())
}
