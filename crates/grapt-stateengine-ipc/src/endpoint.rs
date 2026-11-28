// SPDX-License-Identifier: MIT OR Apache-2.0
//! Discovery: `<state_home>/endpoint.json` and the owner credential, read
//! through one opened and verified private state directory and checked
//! against their closed schemas before anything trusts them.
use crate::peer::Principal;
use crate::proof::OwnerCredential;
use crate::state_dir::StateDir;
use crate::wire::{Address, EndpointFile, NativePath};
use crate::{IpcError, MAX_ENDPOINT_BYTES};
use std::path::Path;
use std::sync::Arc;

const PIPE_PREFIX: &str = r"\\.\pipe\";
const MAX_PIPE_UNITS: usize = 1024;
const MAX_VERSION_BYTES: usize = 128;

/// A host's endpoint and the owner credential beside it, both read from the
/// same verified state directory. Only [`discover`] makes one, and nothing
/// can change it afterwards, so a connection is checked and authenticated
/// against exactly what discovery verified.
#[derive(Clone)]
pub struct Endpoint(Arc<Discovered>);

struct Discovered {
    file: EndpointFile,
    credential: OwnerCredential,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Endpoint")
            .field("file", &self.0.file)
            .finish_non_exhaustive()
    }
}

impl Endpoint {
    /// The checked contents of `endpoint.json`.
    pub fn file(&self) -> &EndpointFile {
        &self.0.file
    }

    pub(crate) fn credential(&self) -> &OwnerCredential {
        &self.0.credential
    }

    /// The socket to connect to, when the host listens on a Unix socket.
    #[cfg(unix)]
    pub fn unix_socket(&self) -> Option<&Path> {
        use std::os::unix::ffi::OsStrExt;
        match &self.0.file.address {
            Address::UnixSocket {
                path: NativePath::UnixBytes(units),
            } => Some(Path::new(std::ffi::OsStr::from_bytes(units))),
            _ => None,
        }
    }
}

/// Read and check `<state_home>/endpoint.json` and `<state_home>/credential`
/// through one opened handle on the state directory. The directory must be
/// `principal`'s alone (0700) and each file owner-only (0600), all checked on
/// the opened handles. A Unix socket must be a socket of the principal's
/// directly inside a directory that is the principal's alone: the state
/// directory or the host's private runtime directory. Windows refuses until
/// its access lists can be verified.
pub fn discover(state_home: &Path, principal: &Principal) -> Result<Endpoint, IpcError> {
    let directory = StateDir::open(state_home, principal).map_err(IpcError::Endpoint)?;
    let bytes = directory
        .read("endpoint.json", u64::from(MAX_ENDPOINT_BYTES))
        .map_err(IpcError::Endpoint)?;
    let file: EndpointFile = crate::frame::decode(&bytes)?;
    validate(&file).map_err(IpcError::Endpoint)?;
    match &file.address {
        Address::UnixSocket { path } => unix_socket(path, principal),
        Address::WindowsPipe { name } => pipe_name(name),
    }
    .map_err(IpcError::Endpoint)?;
    let credential = OwnerCredential::read(&directory, state_home)?;
    Ok(Endpoint(Arc::new(Discovered { file, credential })))
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
    file.epoch.owner.check().map_err(String::from)
}

#[cfg(unix)]
fn unix_socket(path: &NativePath, principal: &Principal) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let NativePath::UnixBytes(units) = path else {
        return Err("a Unix socket path is not in unix_bytes".into());
    };
    path.check()?;
    let socket = Path::new(std::ffi::OsStr::from_bytes(units));
    let (Some(parent), Some(name)) = (socket.parent(), socket.file_name()) else {
        return Err("the socket path names no file in a directory".into());
    };
    // The connected server is still verified by user, process and image.
    StateDir::open(parent, principal)?.socket(name)
}

#[cfg(not(unix))]
fn unix_socket(_path: &NativePath, _principal: &Principal) -> Result<(), String> {
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
