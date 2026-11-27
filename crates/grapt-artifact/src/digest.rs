//! Content addressing and the chunk map.
//!
//! Two rules, both taken from the corrected-F1 house rule in
//! `grapt-schema::canonical`:
//!
//! * A digest over RAW STORED BYTES is bare lowercase hex SHA-256 with no domain
//!   label, so any external tool can reproduce it. Only digests over derived or
//!   canonicalized structures carry a `{DOMAIN}/{VERSION}:` prefix. An artifact
//!   digest is the former — F2B canonicalizes nothing.
//! * A secret-bearing capture is digested under a per-store key, because a bare
//!   SHA-256 of guessable material is exactly the "stable offline equality
//!   oracle" the secret law forbids. Hiding the bytes does not help when the
//!   input space is small.

use sha2::Digest as _;
use std::io::Read as _;

use crate::Sensitivity;

/// The smallest chunk the map will use. Below this the per-chunk digests cost
/// more than they buy.
pub(crate) const MIN_CHUNK_BYTES: u64 = 64 * 1024;

/// The hard ceiling on chunk-map length. A count-bounded container of unbounded
/// values is not bounded, and neither is an unbounded list of bounded values —
/// so the map is capped and the chunk size grows instead.
pub(crate) const MAX_CHUNKS: u64 = 1024;

/// The chunk size for an artifact of `length` bytes.
///
/// `max(MIN_CHUNK_BYTES, ceil(length / MAX_CHUNKS))` keeps the map at most
/// `MAX_CHUNKS` entries whatever the artifact size, so `stat` stays O(1) in
/// response bytes. Saturating arithmetic throughout: a `u64::MAX`-shaped length
/// must yield a chunk size, never a panic.
pub(crate) fn chunk_size_for(length: u64) -> u64 {
    let even = length.div_ceil(MAX_CHUNKS);
    if even > MIN_CHUNK_BYTES {
        even
    } else {
        MIN_CHUNK_BYTES
    }
}

/// An incremental SHA-256 over the whole artifact plus one digest per chunk.
///
/// Both are computed as the bytes stream past, so nothing is ever re-read to
/// digest it and the payload is never held whole.
pub(crate) struct RollingDigest {
    whole: sha2::Sha256,
    chunk: sha2::Sha256,
    chunk_size: u64,
    filled: u64,
    chunks: Vec<String>,
    key: Option<Vec<u8>>,
}

impl core::fmt::Debug for RollingDigest {
    /// Redacted: the in-progress state is derived from payload bytes and, for a
    /// secret-bearing capture, from the store key. Neither may reach a log.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RollingDigest")
            .field("chunks_sealed", &self.chunks.len())
            .finish_non_exhaustive()
    }
}

impl RollingDigest {
    pub(crate) fn new(chunk_size: u64, key: Option<Vec<u8>>) -> Self {
        let mut whole = sha2::Sha256::new();
        let mut chunk = sha2::Sha256::new();
        // Domain-separate the keyed form so a keyed digest can never collide
        // with the bare digest of the same bytes.
        if let Some(key) = key.as_deref() {
            whole.update(b"secode-artifact/keyed/1");
            whole.update(key);
            chunk.update(b"secode-artifact/keyed/1");
            chunk.update(key);
        }
        Self {
            whole,
            chunk,
            chunk_size,
            filled: 0,
            chunks: Vec::new(),
            key,
        }
    }

    /// Absorb one run of bytes, sealing chunk digests at every boundary it
    /// crosses. `bytes` is a borrowed slice the caller already holds bounded;
    /// this never copies it.
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.whole.update(bytes);
        let mut rest = bytes;
        while !rest.is_empty() {
            let room = self.chunk_size.saturating_sub(self.filled);
            if room == 0 {
                self.seal_chunk();
                continue;
            }
            let take = core::cmp::min(room, rest.len() as u64);
            // `take <= rest.len()` by construction, so the split cannot panic.
            let (head, tail) = rest.split_at(take as usize);
            self.chunk.update(head);
            self.filled = self.filled.saturating_add(take);
            rest = tail;
        }
    }

    fn seal_chunk(&mut self) {
        let mut fresh = sha2::Sha256::new();
        if let Some(key) = self.key.as_deref() {
            fresh.update(b"secode-artifact/keyed/1");
            fresh.update(key);
        }
        let done = core::mem::replace(&mut self.chunk, fresh);
        self.chunks.push(to_hex(&done.finalize()));
        self.filled = 0;
    }

    /// Finish, returning `(whole_digest_hex, per_chunk_digest_hex)`.
    pub(crate) fn finish(mut self) -> (String, Vec<String>) {
        if self.filled > 0 || self.chunks.is_empty() {
            self.seal_chunk();
        }
        let whole = to_hex(&self.whole.finalize());
        (whole, self.chunks)
    }
}

/// The digest of a complete in-memory value, for callers that already hold one
/// bounded buffer (a canonical Campaign plan artifact, for instance).
pub(crate) fn digest_of(bytes: &[u8], sensitivity: Sensitivity, key: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    if sensitivity.is_secret_bearing() {
        hasher.update(b"secode-artifact/keyed/1");
        hasher.update(key);
    }
    hasher.update(bytes);
    to_hex(&hasher.finalize())
}

/// The digest of a stored object by bounded streaming: exactly `length`
/// bytes are read from `path` and digested the way [`digest_of`] digests
/// them in memory. The payload is never held whole, and a file that ends
/// short of `length` is an error rather than a silent under-read.
pub(crate) fn streamed_digest_of(
    path: &std::path::Path,
    length: u64,
    sensitivity: Sensitivity,
    key: &[u8],
) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    if sensitivity.is_secret_bearing() {
        hasher.update(b"secode-artifact/keyed/1");
        hasher.update(key);
    }
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut remaining = length;
    while remaining > 0 {
        let want = core::cmp::min(remaining, buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..want])?;
        hasher.update(&buffer[..want]);
        remaining -= want as u64;
    }
    Ok(to_hex(&hasher.finalize()))
}

fn to_hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        // Writing to a String is infallible; the Result is discarded rather
        // than unwrapped because `unwrap_used` is denied workspace-wide.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_digest_is_reproducible_lowercase_hex() {
        // The law: an artifact digest is over RAW bytes with no domain label,
        // so an external tool computing sha256 of the same bytes agrees.
        let digest = digest_of(b"abc", Sensitivity::Ordinary, b"unused");
        assert_eq!(
            digest, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "an ordinary artifact digest must be the plain SHA-256 of its bytes"
        );
    }

    #[test]
    fn a_secret_bearing_digest_is_not_the_plain_hash() {
        // A bare SHA-256 of guessable material is an offline equality oracle.
        // The keyed form must not equal it.
        let bare = digest_of(b"hunter2", Sensitivity::Ordinary, b"k");
        let keyed = digest_of(b"hunter2", Sensitivity::SecretBearing, b"k");
        assert_ne!(
            bare, keyed,
            "a secret-bearing digest must not be the guessable plain hash"
        );
    }

    #[test]
    fn the_chunk_map_is_bounded_however_large_the_artifact() {
        // A count-bounded map of bounded values is only bounded if the count is
        // capped. Chunk size grows instead.
        for length in [0_u64, 1, MIN_CHUNK_BYTES, u64::MAX] {
            let size = chunk_size_for(length);
            assert!(size >= MIN_CHUNK_BYTES, "chunk size must not collapse");
            let chunks = length.div_ceil(size);
            assert!(
                chunks <= MAX_CHUNKS,
                "length {length} would need {chunks} chunks, above the {MAX_CHUNKS} cap"
            );
        }
    }

    #[test]
    fn rolling_and_whole_value_digests_agree() {
        // The streaming path and the buffered path must address the same bytes
        // to the same identity, or dedup silently splits.
        let mut rolling = RollingDigest::new(chunk_size_for(3), None);
        rolling.update(b"a");
        rolling.update(b"bc");
        let (whole, chunks) = rolling.finish();
        assert_eq!(whole, digest_of(b"abc", Sensitivity::Ordinary, b""));
        assert_eq!(chunks.len(), 1, "three bytes is one chunk");
    }

    #[test]
    fn chunk_boundaries_do_not_depend_on_write_arrival() {
        // Chunking is a property of the bytes, not of how the producer happened
        // to flush. Otherwise a page digest depends on network timing.
        let size = MIN_CHUNK_BYTES;
        let body = vec![7_u8; (size as usize).saturating_mul(2)];

        let mut one_shot = RollingDigest::new(size, None);
        one_shot.update(&body);
        let (whole_a, chunks_a) = one_shot.finish();

        let mut dribbled = RollingDigest::new(size, None);
        for piece in body.chunks(7) {
            dribbled.update(piece);
        }
        let (whole_b, chunks_b) = dribbled.finish();

        assert_eq!(whole_a, whole_b);
        assert_eq!(
            chunks_a, chunks_b,
            "chunk digests must not depend on flushes"
        );
        assert_eq!(chunks_a.len(), 2);
    }
}
