//! Verification receipt. Law (foundational report row `verification`): a
//! verification claim is one of `Asserted | Executed | Refused | Unavailable`,
//! and "asserted is never rendered as executed. A terminal verification claim
//! without exact provenance and evidence identity fails closed."
//!
//! F1A owns the disposition vocabulary, the receipt identity, the exact
//! verifier/tool/version/environment provenance, and the receipt binding all
//! of it to the effective graph and bounded evidence. An executed claim must
//! carry both its provenance and its evidence, so an assertion can never
//! masquerade as an executed verification.

use crate::artifact::ArtifactRef;
use crate::subagent::EffectiveGraphId;
use crate::tool::ToolDefinitionId;

crate::id::opaque_id! {
    /// A verification receipt's identity.
    VerificationReceiptId
}

/// How a verification claim was established. A closed vocabulary: an `Asserted`
/// claim (the model said so) is a different value from an `Executed` one (a tool
/// actually ran), so a projection can never present one as the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VerificationDisposition {
    /// The model asserted the property holds; nothing was executed.
    Asserted,
    /// A verifier actually executed and produced evidence.
    Executed,
    /// Verification was refused (e.g. policy or authority).
    Refused,
    /// Verification could not be performed (verifier/tool/environment absent).
    Unavailable,
}

impl VerificationDisposition {
    /// Whether an actual verifier ran. Only `Executed` counts as executed
    /// verification; assertion and unavailability never do.
    pub fn is_executed(self) -> bool {
        matches!(self, VerificationDisposition::Executed)
    }
}

crate::id::opaque_id! {
    /// The exact version of the verifier that ran.
    VerifierVersion
}

crate::id::opaque_id! {
    /// The environment a verifier ran in (host/container/toolchain identity).
    VerifierEnvironmentId
}

/// Exact provenance of an executed verification.
///
/// Law: "a terminal verification claim without exact provenance and evidence
/// identity fails closed." Only an executed verification has a verifier, so
/// provenance is a variant rather than a nullable field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationProvenance {
    /// A verifier actually ran, with its exact identity.
    Executed {
        tool: ToolDefinitionId,
        version: VerifierVersion,
        environment: VerifierEnvironmentId,
    },
    /// No verifier ran, so there is no verifier provenance to record.
    NotExecuted,
}

impl VerificationProvenance {
    pub fn names_a_verifier(&self) -> bool {
        matches!(self, VerificationProvenance::Executed { .. })
    }
}

/// A verification receipt binding its disposition to exact verifier
/// provenance, the effective graph and bounded evidence.
///
/// An `Executed` claim MUST carry both provenance and evidence; a terminal
/// executed verification missing either is not constructible, so it fails
/// closed rather than being asserted-as-executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReceipt {
    id: VerificationReceiptId,
    disposition: VerificationDisposition,
    provenance: VerificationProvenance,
    effective_graph: EffectiveGraphId,
    /// Bounded evidence for an executed verification. Required for `Executed`;
    /// `None` for the other dispositions, which produced no executed evidence.
    evidence: Option<ArtifactRef>,
}

impl VerificationReceipt {
    /// Build a receipt.
    ///
    /// Returns `None` unless the disposition, provenance and evidence agree:
    /// `Executed` requires both a named verifier and evidence, and every other
    /// disposition must carry neither. Any mismatch would let an assertion
    /// read as evidence.
    pub fn new(
        id: VerificationReceiptId,
        disposition: VerificationDisposition,
        provenance: VerificationProvenance,
        effective_graph: EffectiveGraphId,
        evidence: Option<ArtifactRef>,
    ) -> Option<Self> {
        let executed = disposition.is_executed();
        if executed != evidence.is_some() || executed != provenance.names_a_verifier() {
            return None;
        }
        Some(Self {
            id,
            disposition,
            provenance,
            effective_graph,
            evidence,
        })
    }

    pub fn provenance(&self) -> &VerificationProvenance {
        &self.provenance
    }

    pub fn id(&self) -> &VerificationReceiptId {
        &self.id
    }
    pub fn disposition(&self) -> VerificationDisposition {
        self.disposition
    }
    pub fn effective_graph(&self) -> &EffectiveGraphId {
        &self.effective_graph
    }
    pub fn evidence(&self) -> Option<&ArtifactRef> {
        self.evidence.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{ArtifactDigest, ArtifactId};

    fn graph() -> Option<EffectiveGraphId> {
        EffectiveGraphId::new("efg_1")
    }
    fn id() -> Option<VerificationReceiptId> {
        VerificationReceiptId::new("ver_1")
    }
    fn evidence() -> Option<ArtifactRef> {
        Some(ArtifactRef::new(
            ArtifactId::new("art_1")?,
            10,
            ArtifactDigest::new("sha-1")?,
        ))
    }
    fn ran() -> Option<VerificationProvenance> {
        Some(VerificationProvenance::Executed {
            tool: ToolDefinitionId::new("cargo-test")?,
            version: VerifierVersion::new("1.97.0")?,
            environment: VerifierEnvironmentId::new("host-1")?,
        })
    }

    #[test]
    fn executed_requires_evidence_and_asserted_forbids_it() -> Result<(), Box<dyn std::error::Error>>
    {
        // Executed with provenance and evidence is valid.
        assert!(VerificationReceipt::new(
            id().ok_or("id")?,
            VerificationDisposition::Executed,
            ran().ok_or("provenance")?,
            graph().ok_or("graph")?,
            Some(evidence().ok_or("evidence")?)
        )
        .is_some());
        // Executed WITHOUT evidence fails closed.
        assert!(VerificationReceipt::new(
            id().ok_or("id")?,
            VerificationDisposition::Executed,
            ran().ok_or("provenance")?,
            graph().ok_or("graph")?,
            None
        )
        .is_none());
        // Asserted with evidence is refused — an assertion never carries executed
        // evidence.
        assert!(VerificationReceipt::new(
            id().ok_or("id")?,
            VerificationDisposition::Asserted,
            VerificationProvenance::NotExecuted,
            graph().ok_or("graph")?,
            Some(evidence().ok_or("evidence")?)
        )
        .is_none());
        // Asserted without evidence is valid, and is never "executed".
        let asserted = VerificationReceipt::new(
            id().ok_or("id")?,
            VerificationDisposition::Asserted,
            VerificationProvenance::NotExecuted,
            graph().ok_or("graph")?,
            None,
        )
        .ok_or("asserted receipt")?;
        assert!(!asserted.disposition().is_executed());
        Ok(())
    }

    #[test]
    fn an_executed_claim_without_a_named_verifier_fails_closed(
    ) -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            VerificationReceipt::new(
                id().ok_or("id")?,
                VerificationDisposition::Executed,
                VerificationProvenance::NotExecuted,
                graph().ok_or("graph")?,
                Some(evidence().ok_or("evidence")?)
            )
            .is_none(),
            "evidence without exact verifier provenance is not executed verification"
        );
        Ok(())
    }

    #[test]
    fn a_non_executed_claim_cannot_borrow_verifier_provenance(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for disposition in [
            VerificationDisposition::Asserted,
            VerificationDisposition::Refused,
            VerificationDisposition::Unavailable,
        ] {
            assert!(
                VerificationReceipt::new(
                    id().ok_or("id")?,
                    disposition,
                    ran().ok_or("provenance")?,
                    graph().ok_or("graph")?,
                    None
                )
                .is_none(),
                "{disposition:?} must not name a verifier that never ran"
            );
        }
        Ok(())
    }
}
