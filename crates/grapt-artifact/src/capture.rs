//! Bounded capture: the reader side of "bound before allocation".
//!
//! The capture owns the ONLY buffer, sized by [`crate::CaptureBound`], and hands
//! bytes to the sink as they arrive. There is deliberately no public entry point
//! that accepts an already-materialized owned buffer of unbounded length — the
//! bound has to be supplied before the first read or it is not a
//! pre-allocation bound at all.

use grapt_types::{ArtifactRef, CaptureDisposition};

/// One completed capture: the reference AND the disposition that describes it.
///
/// F1A ships `ArtifactRef` and `CaptureDisposition` as independent types with
/// nothing pairing them, so a reference alone cannot say whether it names the
/// whole value or a bounded prefix. This is the pairing, and it is the only
/// thing a capture returns — a caller cannot obtain one without the other.
///
/// The disposition is also recorded in the store's metadata, so a fetch can
/// refuse a reference whose claimed disposition disagrees with what was actually
/// captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedArtifact {
    reference: ArtifactRef,
    disposition: CaptureDisposition,
}

impl CapturedArtifact {
    pub(crate) fn new(reference: ArtifactRef, disposition: CaptureDisposition) -> Self {
        Self {
            reference,
            disposition,
        }
    }

    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    /// Whether the stored bytes are the whole value or a bounded prefix.
    pub fn disposition(&self) -> CaptureDisposition {
        self.disposition
    }

    /// True only when the stored bytes are the complete value. A projection that
    /// presents a truncated capture as the whole value is the defect this
    /// exists to prevent.
    pub fn is_complete(&self) -> bool {
        self.disposition.is_complete()
    }
}

/// Why a capture could not be completed.
///
/// Note what is NOT here: exceeding the bound is not an error. A source larger
/// than the bound produces a successful capture with
/// `CaptureDisposition::TruncatedAtBoundary`, because the law's "complete" means
/// complete WITHIN the bound.
#[derive(Debug)]
pub enum CaptureError {
    /// The source could not be read to the bound.
    Source(std::io::Error),
    /// The store could not commit the bytes to its selected backend (including
    /// volatile capacity refusal). Nothing partial is visible.
    Store(std::io::Error),
    /// The digest did not yield a usable identity. Unreachable in practice —
    /// a 64-character hex string is never empty — but represented rather than
    /// unwrapped, because `unwrap_used` is denied.
    UnusableIdentity,
}

impl core::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CaptureError::Source(error) => write!(f, "the capture source failed: {error}"),
            CaptureError::Store(error) => write!(f, "the artifact store failed: {error}"),
            CaptureError::UnusableIdentity => {
                f.write_str("the capture produced no usable identity")
            }
        }
    }
}

impl std::error::Error for CaptureError {}

/// The streaming sink a capture writes into.
///
/// Implemented by the store. The capture drives it; it never sees the whole
/// value, which is what keeps the working set at one buffer.
pub(crate) trait CaptureSink {
    fn write(&mut self, bytes: &[u8]) -> Result<(), std::io::Error>;
}

/// Reads `source` into `sink` under `bound`, stopping at the boundary.
///
/// Returns `(bytes_stored, disposition)`. The disposition is
/// `TruncatedAtBoundary` exactly when the source had at least one more byte than
/// the bound admits — which is detected by attempting one read BEYOND the bound,
/// never by reading the remainder. How much more there was is deliberately not
/// reported: the capture stopped, so it does not know, and claiming a number it
/// never counted would be the lie.
pub(crate) fn drain_bounded<R, S>(
    source: &mut R,
    sink: &mut S,
    bound: crate::CaptureBound,
) -> Result<(u64, CaptureDisposition), std::io::Error>
where
    R: std::io::Read,
    S: CaptureSink,
{
    let mut buffer = vec![0_u8; bound.read_buffer_bytes()];
    let mut stored: u64 = 0;

    loop {
        let remaining = bound.max_bytes().saturating_sub(stored);
        if remaining == 0 {
            break;
        }
        // Never read more than the bound still admits, so the buffer cannot
        // hold bytes we are not entitled to keep.
        let want = core::cmp::min(remaining, buffer.len() as u64) as usize;
        let slice = buffer.get_mut(..want).unwrap_or_default();
        if slice.is_empty() {
            break;
        }
        let read = source.read(slice)?;
        if read == 0 {
            // Source ended at or under the bound: the whole value fit.
            return Ok((stored, CaptureDisposition::Complete));
        }
        let filled = buffer.get(..read).unwrap_or_default();
        sink.write(filled)?;
        stored = stored.saturating_add(read as u64);
    }

    // The bound is full. One more read decides the disposition: exactly at the
    // bound is Complete, one byte over is TruncatedAtBoundary. This is the
    // off-by-one that decides whether a bounded capture may be presented as the
    // whole value.
    let mut probe = [0_u8; 1];
    let over = source.read(&mut probe)?;
    if over == 0 {
        Ok((stored, CaptureDisposition::Complete))
    } else {
        Ok((
            stored,
            CaptureDisposition::TruncatedAtBoundary { captured: stored },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CaptureBound;

    struct Counting {
        written: Vec<u8>,
    }

    impl CaptureSink for Counting {
        fn write(&mut self, bytes: &[u8]) -> Result<(), std::io::Error> {
            self.written.extend_from_slice(bytes);
            Ok(())
        }
    }

    fn drain(source: &[u8], max: u64, buf: usize) -> (u64, CaptureDisposition, Vec<u8>) {
        let bound = match CaptureBound::new(max, buf) {
            Some(bound) => bound,
            None => panic!("test bound must be valid"),
        };
        let mut sink = Counting {
            written: Vec::new(),
        };
        let mut cursor = source;
        let (stored, disposition) = match drain_bounded(&mut cursor, &mut sink, bound) {
            Ok(result) => result,
            Err(error) => panic!("draining a slice cannot fail: {error}"),
        };
        (stored, disposition, sink.written)
    }

    #[test]
    fn a_source_under_the_bound_is_complete() {
        let (stored, disposition, written) = drain(b"abc", 16, 4);
        assert_eq!(stored, 3);
        assert_eq!(disposition, CaptureDisposition::Complete);
        assert_eq!(written, b"abc");
    }

    #[test]
    fn a_source_exactly_at_the_bound_is_complete_not_truncated() {
        // The off-by-one that decides whether a bounded capture may be presented
        // as the whole value. Using `>=` where `>` belongs reports a complete
        // capture as truncated and permanently mislabels every exact-fit value.
        let (stored, disposition, written) = drain(b"abcd", 4, 3);
        assert_eq!(stored, 4);
        assert_eq!(
            disposition,
            CaptureDisposition::Complete,
            "a source that exactly fills the bound is COMPLETE"
        );
        assert_eq!(written, b"abcd");
    }

    #[test]
    fn a_source_one_byte_over_the_bound_is_truncated_at_it() {
        let (stored, disposition, written) = drain(b"abcde", 4, 3);
        assert_eq!(stored, 4);
        assert_eq!(
            disposition,
            CaptureDisposition::TruncatedAtBoundary { captured: 4 },
            "one byte past the bound must be recorded as truncation"
        );
        assert_eq!(written, b"abcd", "no byte beyond the bound is ever stored");
    }

    #[test]
    fn the_working_set_is_the_buffer_not_the_bound() {
        // A large bound with a tiny buffer must still stream. If the capture
        // ever allocated `max_bytes` up front this would be a 64 MiB allocation
        // for four bytes of payload.
        let (stored, disposition, written) = drain(b"abcd", 64 * 1024 * 1024, 2);
        assert_eq!(stored, 4);
        assert_eq!(disposition, CaptureDisposition::Complete);
        assert_eq!(written, b"abcd");
    }

    #[test]
    fn an_empty_source_is_a_complete_empty_capture() {
        let (stored, disposition, written) = drain(b"", 8, 4);
        assert_eq!(stored, 0);
        assert_eq!(disposition, CaptureDisposition::Complete);
        assert!(written.is_empty());
    }
}
