// SPDX-License-Identifier: MIT OR Apache-2.0
//! The owner proof: an HMAC-SHA256 under the owner credential over the
//! challenge and this client's own native identity, as the host observes it.
use crate::peer::{field, PeerBinding, Principal};
use crate::wire::Version;
use crate::{IpcError, Sha256Hex};
use std::path::Path;
use zeroize::Zeroizing;

const OWNER_DOMAIN: &[u8] = b"se.ipc.owner/v1";
const CREDENTIAL_BYTES: usize = 32;

/// The owner's machine credential: exactly 32 bytes, held zeroized.
pub struct OwnerCredential(Zeroizing<[u8; CREDENTIAL_BYTES]>);

impl std::fmt::Debug for OwnerCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OwnerCredential(..)")
    }
}

impl OwnerCredential {
    /// Read `<state_home>/credential`: a regular file, not a link, that on
    /// Unix `principal` owns and no one else may read.
    pub fn read(state_home: &Path, principal: &Principal) -> Result<Self, IpcError> {
        let path = state_home.join("credential");
        let bytes = crate::private_file::read_private(&path, principal, CREDENTIAL_BYTES as u64)
            .map_err(IpcError::Credential)?;
        let secret: [u8; CREDENTIAL_BYTES] = bytes.as_slice().try_into().map_err(|_| {
            IpcError::Credential(format!(
                "{} holds {} bytes, not {CREDENTIAL_BYTES}",
                path.display(),
                bytes.len()
            ))
        })?;
        Ok(Self(Zeroizing::new(secret)))
    }
}

/// The inputs the owner proof covers, in their wire order.
pub(crate) struct ProofInput<'a> {
    pub(crate) version: Version,
    pub(crate) canonical_epoch: &'a [u8],
    pub(crate) image_sha256: &'a Sha256Hex,
    pub(crate) server_nonce: &'a [u8; 32],
    pub(crate) client_nonce: &'a [u8; 32],
    pub(crate) binding: &'a PeerBinding,
}

pub(crate) fn owner_mac(credential: &OwnerCredential, input: &ProofInput<'_>) -> [u8; 32] {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, credential.0.as_slice());
    let mut message = Vec::new();
    field(&mut message, OWNER_DOMAIN);
    let mut version = input.version.major.to_be_bytes().to_vec();
    version.extend_from_slice(&input.version.minor.to_be_bytes());
    field(&mut message, &version);
    field(&mut message, input.canonical_epoch);
    field(&mut message, &input.image_sha256.0);
    field(&mut message, input.server_nonce);
    field(&mut message, input.client_nonce);
    field(&mut message, &input.binding.bytes());
    let tag = ring::hmac::sign(&key, &message);
    let mut mac = [0; 32];
    mac.copy_from_slice(tag.as_ref());
    mac
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::StartIdentity;

    /// The binding layout spelled out byte by byte, and a MAC computed by hand
    /// over the same fields with a separate HMAC call.
    #[test]
    fn the_binding_and_mac_follow_the_published_layout() {
        let binding = PeerBinding {
            principal: Principal::Unix(1000),
            pid: 7,
            started: StartIdentity::Macos {
                start_seconds: 2,
                start_micros: 3,
            },
            image_sha256: Sha256Hex([9; 32]),
        };
        let mut expected = b"se.native.peer/v1".to_vec();
        for part in [
            &b"unix"[..],
            &1000_u32.to_be_bytes(),
            &7_u32.to_be_bytes(),
            b"macos",
            &2_u64.to_be_bytes(),
            &3_u32.to_be_bytes(),
            &[9; 32],
        ] {
            expected.extend_from_slice(&(part.len() as u32).to_be_bytes());
            expected.extend_from_slice(part);
        }
        assert_eq!(binding.bytes(), expected);
        let credential = OwnerCredential(Zeroizing::new([5; 32]));
        let input = ProofInput {
            version: Version { major: 1, minor: 0 },
            canonical_epoch: br#"{"generation":1}"#,
            image_sha256: &Sha256Hex([8; 32]),
            server_nonce: &[1; 32],
            client_nonce: &[2; 32],
            binding: &binding,
        };
        let mut message = Vec::new();
        for part in [
            &b"se.ipc.owner/v1"[..],
            &[0, 1, 0, 0],
            br#"{"generation":1}"#,
            &[8; 32],
            &[1; 32],
            &[2; 32],
            &expected,
        ] {
            message.extend_from_slice(&(part.len() as u32).to_be_bytes());
            message.extend_from_slice(part);
        }
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &[5; 32]);
        assert_eq!(
            owner_mac(&credential, &input),
            ring::hmac::sign(&key, &message).as_ref()
        );
    }
}
