// SPDX-License-Identifier: MIT OR Apache-2.0
//! Executable digests: a host binary checked against its pinned SHA-256, and
//! this client's own image for its peer binding. Both read in bounded chunks.
use crate::{IpcError, Sha256Hex};
use std::io::Read;
use std::path::Path;
use subtle::ConstantTimeEq;

const CHUNK_BYTES: usize = 64 * 1024;

/// Check that the file at `path`, at most `max_bytes` long, has the pinned
/// SHA-256. The digests are compared in constant time.
pub fn verify_binary(path: &Path, pinned: &Sha256Hex, max_bytes: u64) -> Result<(), IpcError> {
    if same_digest(&sha256_file(path, max_bytes)?, pinned) {
        Ok(())
    } else {
        Err(IpcError::BinaryMismatch)
    }
}

/// Whether two digests are equal, compared in constant time.
pub(crate) fn same_digest(left: &Sha256Hex, right: &Sha256Hex) -> bool {
    bool::from(left.0.ct_eq(&right.0))
}

/// The SHA-256 of a file of at most `max_bytes`, read in bounded chunks.
pub(crate) fn sha256_file(path: &Path, max_bytes: u64) -> Result<Sha256Hex, IpcError> {
    let unreadable =
        |error: std::io::Error| IpcError::Binary(format!("{}: {error}", path.display()));
    let mut file = std::fs::File::open(path).map_err(unreadable)?;
    if !file.metadata().map_err(unreadable)?.is_file() {
        return Err(IpcError::Binary(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0; CHUNK_BYTES];
    let mut total: u64 = 0;
    loop {
        let read = file.read(&mut buffer).map_err(unreadable)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if total > max_bytes {
            return Err(IpcError::Binary(format!(
                "{} is larger than its {max_bytes}-byte bound",
                path.display()
            )));
        }
        context.update(buffer.get(..read).unwrap_or_default());
    }
    let digest = context.finish();
    let bytes: [u8; 32] = digest
        .as_ref()
        .try_into()
        .map_err(|_| IpcError::Binary("SHA-256 did not produce 32 bytes".into()))?;
    Ok(Sha256Hex(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_matches_only_its_pinned_digest_within_its_bound(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("grapt-ipc-binary-{}", std::process::id()));
        std::fs::write(&path, b"abc")?;
        let abc =
            Sha256Hex::parse("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
                .ok_or("digest")?;
        let checked = verify_binary(&path, &abc, 3);
        let wrong = verify_binary(&path, &Sha256Hex([0; 32]), 3);
        let oversized = verify_binary(&path, &abc, 2);
        std::fs::remove_file(&path)?;
        checked?;
        assert!(matches!(wrong, Err(IpcError::BinaryMismatch)));
        assert!(matches!(oversized, Err(IpcError::Binary(_))));
        Ok(())
    }
}
