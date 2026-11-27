//! Strict immutable-build selection format shared by the Windows installer
//! and product. This codec performs no I/O or installation state transitions.

use crate::GitCommitId;

pub const POINTER_BYTES: usize = 41;

pub fn decode_pointer(bytes: &[u8]) -> Result<GitCommitId, &'static str> {
    if bytes.len() != POINTER_BYTES || bytes.last() != Some(&b'\n') {
        return Err("installation current must contain one build identity and newline");
    }
    let identity = std::str::from_utf8(&bytes[..POINTER_BYTES - 1])
        .map_err(|_| "installation current is not a build identity")?;
    GitCommitId::new(identity).ok_or("installation current names an invalid build identity")
}

pub fn encode_pointer(identity: &str) -> Result<Vec<u8>, &'static str> {
    if identity.len() != POINTER_BYTES - 1 || GitCommitId::new(identity).is_none() {
        return Err("installation current names an invalid build identity");
    }
    let mut bytes = Vec::with_capacity(POINTER_BYTES);
    bytes.extend_from_slice(identity.as_bytes());
    bytes.push(b'\n');
    Ok(bytes)
}
