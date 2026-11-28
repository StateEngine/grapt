// SPDX-License-Identifier: MIT OR Apache-2.0
//! This client's own native identity: the account, process and executable
//! the host observes on the connection, in its canonical binding bytes.
use crate::wire::StartIdentity;
use crate::Sha256Hex;

const PEER_HEADER: &[u8] = b"se.native.peer/v1";

/// The operating-system account a process runs as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// A Unix user id.
    Unix(u32),
    /// A Windows SID in its string form, such as `S-1-5-21-...`.
    Windows(String),
}

/// This client's own process identity: the one the host observes on the
/// connection and binds the proof to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerBinding {
    pub principal: Principal,
    pub pid: u32,
    pub started: StartIdentity,
    /// The SHA-256 of this client's own executable.
    pub image_sha256: Sha256Hex,
}

impl PeerBinding {
    /// The canonical binding bytes: an unprefixed header, then every
    /// component length-prefixed.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut out = PEER_HEADER.to_vec();
        match &self.principal {
            Principal::Unix(uid) => {
                field(&mut out, b"unix");
                field(&mut out, &uid.to_be_bytes());
            }
            Principal::Windows(sid) => {
                field(&mut out, b"windows");
                field(&mut out, sid.as_bytes());
            }
        }
        field(&mut out, &self.pid.to_be_bytes());
        match self.started {
            StartIdentity::Linux { start_ticks } => {
                field(&mut out, b"linux");
                field(&mut out, &start_ticks.to_be_bytes());
            }
            StartIdentity::Windows { creation_filetime } => {
                field(&mut out, b"windows");
                field(&mut out, &creation_filetime.to_be_bytes());
            }
            StartIdentity::Macos {
                start_seconds,
                start_micros,
            } => {
                field(&mut out, b"macos");
                field(&mut out, &start_seconds.to_be_bytes());
                field(&mut out, &start_micros.to_be_bytes());
            }
        }
        field(&mut out, &self.image_sha256.0);
        out
    }
}

/// One component, prefixed with its length as a big-endian `u32`. Every
/// component here is far below 4 GiB, so the length always fits.
pub(crate) fn field(out: &mut Vec<u8>, bytes: &[u8]) {
    let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(bytes);
}
