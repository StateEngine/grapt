//! Content proof for live blocks on a positions stream (B3 S2c-1a).
//!
//! Every identity-tagged block keeps a running SHA-256 over every byte it
//! admitted: the text a snapshot seeded it with and every delta tail, hashed
//! before the slice bound discards any of it. A block's end is then judged
//! against that digest, never against the kept text alone, so bytes that were
//! counted but discarded are proven rather than assumed. The digest lives with
//! the identity it belongs to, so eviction and window shifts move or drop both
//! together.
use super::*;
use sha2::Digest as _;

/// A running digest over every byte one block admitted.
#[derive(Clone, Debug, Default)]
pub(crate) struct BlockDigest(sha2::Sha256);

impl BlockDigest {
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn hex(&self) -> String {
        self.0
            .clone()
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Whether `text`'s first `received` bytes are exactly what was admitted.
    fn proves_prefix(&self, text: &str, received: usize) -> bool {
        text.as_bytes()
            .get(..received)
            .is_some_and(|prefix| sha2::Sha256::digest(prefix)[..] == self.0.clone().finalize()[..])
    }
}

/// One open block's identity and the digest of what it has admitted.
#[derive(Clone, Debug)]
pub(crate) struct Wired {
    pub(crate) identity: SliceWire,
    pub(crate) digest: BlockDigest,
}

impl Wired {
    pub(crate) fn new(identity: SliceWire) -> Self {
        Self {
            identity,
            digest: BlockDigest::default(),
        }
    }
}

impl PartialEq for Wired {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity && self.digest.hex() == other.digest.hex()
    }
}

/// How the producer ended one block.
enum End<'a> {
    /// The block's whole text.
    Full(&'a str),
    /// Too large for one frame: only its length and digest.
    Elided { bytes: u64, sha256: &'a str },
}

fn end_of(envelope: &EventEnvelope, kind: SliceKind) -> Result<End<'_>, ProjectionError> {
    let data = &envelope.data;
    let text = data
        .get("text")
        .and_then(serde_json::Value::as_str)
        .ok_or(ProjectionError::EndUnproven { kind })?;
    match (data.get("textBytes"), data.get("textSha256")) {
        (None, None) => Ok(End::Full(text)),
        (Some(bytes), Some(sha256)) => Ok(End::Elided {
            bytes: bytes
                .as_u64()
                .ok_or(ProjectionError::EndUnproven { kind })?,
            sha256: sha256
                .as_str()
                .ok_or(ProjectionError::EndUnproven { kind })?,
        }),
        _ => Err(ProjectionError::EndUnproven { kind }),
    }
}

impl SessionView {
    /// End one block on a positions stream: verify what it admitted against
    /// the producer's end, complete it, or materialize a block that opened and
    /// closed between P and the snapshot's capture. An end that cannot be
    /// proven is a re-read, never a quiet seal.
    pub(super) fn close_positioned(
        &mut self,
        kind: SliceKind,
        envelope: &EventEnvelope,
        bounds: Bounds,
    ) -> Result<Applied, ProjectionError> {
        let identity = wire_identity(envelope, kind)?;
        if self.holds_durably(&identity.0, kind, identity.1) {
            return Ok(Applied::Ignored);
        }
        let end = end_of(envelope, kind)?;
        let Some(index) = self.open_by_identity(kind, &identity) else {
            return match end {
                End::Full(text) => {
                    self.push_open(kind, identity, String::new(), 0, bounds);
                    let index = self.slices.len().saturating_sub(1);
                    self.complete(index, text, bounds);
                    Ok(Applied::Changed)
                }
                End::Elided { .. } => Err(ProjectionError::EndUnproven { kind }),
            };
        };
        let slice = &self.slices[index];
        let received = slice.text.len().saturating_add(slice.truncated_bytes);
        let digest = self
            .slice_wires
            .get(&slice.ordinal)
            .map(|wired| &wired.digest)
            .ok_or(ProjectionError::EndUnproven { kind })?;
        match end {
            End::Full(text) if digest.proves_prefix(text, received) => {
                self.complete(index, text, bounds);
                Ok(Applied::Changed)
            }
            End::Elided { bytes, sha256 }
                if u64::try_from(received).is_ok_and(|received| received == bytes)
                    && digest.hex() == sha256 =>
            {
                self.seal(index);
                Ok(Applied::Changed)
            }
            _ => Err(ProjectionError::EndUnproven { kind }),
        }
    }

    /// Replace a proven block's text with its whole text, kept within the
    /// slice bound on a character boundary with the rest counted, then close.
    fn complete(&mut self, index: usize, text: &str, bounds: Bounds) {
        let mut cut = text.len().min(bounds.max_slice_bytes);
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        if let Some(slice) = self.slices.get_mut(index) {
            slice.text = text.get(..cut).unwrap_or_default().to_owned();
            slice.truncated_bytes = text.len() - cut;
        }
        self.seal(index);
    }

    fn seal(&mut self, index: usize) {
        let Some(slice) = self.slices.get_mut(index) else {
            return;
        };
        slice.closed = true;
        let kind = slice.kind;
        let ordinal = slice.ordinal;
        self.slice_wires.remove(&ordinal);
        if self.adopted == Some(kind) {
            self.adopted = None;
        }
    }
}
