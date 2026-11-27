//! The keystore envelope: credential bytes sealed with AES-256-GCM under a key
//! the operating system's keystore holds. The envelope's own prefix is the
//! authenticated data, so a body cannot be moved under another format.
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use std::io;

/// Marks bytes sealed by this envelope.
pub(super) const PREFIX: &[u8] = super::KEYSTORE_ENVELOPE;
/// The sealing key's length.
pub(super) const KEY_BYTES: usize = 32;
/// The per-seal nonce's length.
pub(super) const NONCE_BYTES: usize = NONCE_LEN;
const TAG_BYTES: usize = 16;

/// Seal `plaintext` under `key` with a nonce never used with this key before.
pub(super) fn seal(
    key: &[u8; KEY_BYTES],
    nonce: [u8; NONCE_BYTES],
    plaintext: &[u8],
) -> io::Result<Vec<u8>> {
    let mut body = zeroize::Zeroizing::new(plaintext.to_vec());
    cipher(key)?
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(PREFIX),
            &mut *body,
        )
        .map_err(|_| io::Error::other("credential sealing failed"))?;
    let mut sealed = Vec::with_capacity(PREFIX.len() + NONCE_BYTES + body.len());
    sealed.extend_from_slice(PREFIX);
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&body);
    Ok(sealed)
}

/// Open what follows [`PREFIX`]. Bytes altered, or sealed under another key,
/// fail authentication and are refused.
pub(super) fn open(key: &[u8; KEY_BYTES], sealed: &[u8]) -> io::Result<Vec<u8>> {
    if sealed.len() < NONCE_BYTES + TAG_BYTES {
        return Err(refused());
    }
    let (nonce, body) = sealed.split_at(NONCE_BYTES);
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| refused())?;
    let mut body = zeroize::Zeroizing::new(body.to_vec());
    let plaintext = cipher(key)?
        .open_in_place(nonce, Aad::from(PREFIX), &mut body)
        .map_err(|_| refused())?;
    Ok(plaintext.to_vec())
}

fn cipher(key: &[u8; KEY_BYTES]) -> io::Result<LessSafeKey> {
    UnboundKey::new(&AES_256_GCM, key)
        .map(LessSafeKey::new)
        .map_err(|_| io::Error::other("credential sealing key was refused"))
}

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "sealed credential failed authentication: it was altered, or sealed under another key",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_credential_opens_only_unaltered_and_under_its_own_key() -> io::Result<()> {
        let (key, other) = ([7; KEY_BYTES], [8; KEY_BYTES]);
        let sealed = seal(&key, [1; NONCE_BYTES], b"fixture-secret")?;
        let body = sealed
            .strip_prefix(PREFIX)
            .ok_or_else(|| io::Error::other("prefix"))?;
        assert!(!body.windows(14).any(|bytes| bytes == b"fixture-secret"));
        assert_eq!(open(&key, body)?, b"fixture-secret");
        assert!(open(&other, body).is_err());
        let mut altered = body.to_vec();
        if let Some(last) = altered.last_mut() {
            *last ^= 1;
        }
        assert!(open(&key, &altered).is_err());
        assert!(open(&key, &body[..NONCE_BYTES]).is_err());
        Ok(())
    }
}
