//! Artifact identity and capture disposition. Law (foundational report row
//! `artifact`; supersession matrix "'Full output'" and "Large feed payload"
//! rows): a durable artifact is a content-addressed immutable reference with an
//! exact length and digest, captured once inside a hard pre-allocation bound.
//! "Complete means complete inside the hard pre-allocation capture bound. Fetch
//! verifies identity/length/digest before return; event feeds do not
//! materialize payload merely to choose a reference."
//!
//! F1A owns the identities and the capture-disposition vocabulary; the bounded
//! store/fetch implementation is a later crate.

crate::id::opaque_id! {
    /// A content-addressed immutable artifact identity.
    ArtifactId
}

crate::id::opaque_id! {
    /// The integrity digest an [`ArtifactRef`] pins. A fetch verifies the fetched
    /// bytes against this digest (and the length) before returning them.
    ArtifactDigest
}

/// A reference to a durable artifact carrying the exact length and digest a
/// fetch must verify. The reference is chosen WITHOUT materializing the payload;
/// verification happens at fetch, not at reference time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtifactRef {
    id: ArtifactId,
    length: u64,
    digest: ArtifactDigest,
}

impl ArtifactRef {
    pub fn new(id: ArtifactId, length: u64, digest: ArtifactDigest) -> Self {
        Self { id, length, digest }
    }

    pub fn id(&self) -> &ArtifactId {
        &self.id
    }
    /// The exact byte length a fetch must observe.
    pub fn length(&self) -> u64 {
        self.length
    }
    pub fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }
}

/// Whether a capture is the complete value or was truncated at the hard
/// pre-allocation bound. There is no unbounded "full" capture: `Complete` means
/// complete WITHIN the bound. A truncated capture records how many bytes were
/// kept, so a projection never presents a bounded capture as the whole value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureDisposition {
    /// The whole value fit inside the pre-allocation bound and was captured.
    Complete,
    /// The value exceeded the bound; exactly `captured` bytes were kept at the
    /// boundary and the remainder was not materialized.
    TruncatedAtBoundary { captured: u64 },
}

impl CaptureDisposition {
    pub fn is_complete(self) -> bool {
        matches!(self, CaptureDisposition::Complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_ref_carries_exact_length_and_digest() -> Result<(), Box<dyn std::error::Error>> {
        let id = ArtifactId::new("art_1").ok_or("artifact id")?;
        let digest = ArtifactDigest::new("sha256-abc").ok_or("digest")?;
        let reference = ArtifactRef::new(id.clone(), 4096, digest.clone());
        assert_eq!(reference.id(), &id);
        assert_eq!(reference.length(), 4096);
        assert_eq!(reference.digest(), &digest);
        Ok(())
    }

    #[test]
    fn a_truncated_capture_is_not_complete() {
        assert!(CaptureDisposition::Complete.is_complete());
        assert!(!CaptureDisposition::TruncatedAtBoundary { captured: 1024 }.is_complete());
    }
}
