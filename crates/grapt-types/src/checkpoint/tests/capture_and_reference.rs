use super::super::*;
use super::support::*;

#[test]
fn skipped_capture_is_evidence_not_absence() {
    let skipped = CheckpointCaptureOutcome::Skipped {
        reason: CaptureSkipReason::PolicyDeclined,
    };
    assert!(skipped.checkpoint().is_none());
    assert!(
        !skipped.proves_exact_coverage(),
        "a skipped capture never proves recovery coverage"
    );
}

#[test]
fn head_change_refuses_rather_than_guessing_a_baseline() {
    let refused = CheckpointCaptureOutcome::Refused {
        reason: CaptureRefusalReason::HeadChangedDuringCapture,
    };
    assert!(refused.checkpoint().is_none());
    assert!(!refused.proves_exact_coverage());
}

#[test]
fn degraded_capture_does_not_prove_exact_coverage() -> Result<(), Box<dyn std::error::Error>> {
    let degraded = CheckpointCaptureOutcome::Captured {
        anchor: Box::new(
            anchor_with(CheckpointFidelity::Degraded {
                reason: FidelityLoss::PolicyExcluded,
            })
            .ok_or("anchor")?,
        ),
        lineage: CheckpointLineage::Root,
    };
    assert!(
        degraded.checkpoint().is_some(),
        "a degraded capture still produced a checkpoint"
    );
    assert!(
        !degraded.proves_exact_coverage(),
        "but it is not exact recovery coverage"
    );
    Ok(())
}

#[test]
fn exact_capture_proves_coverage() -> Result<(), Box<dyn std::error::Error>> {
    let captured = CheckpointCaptureOutcome::Captured {
        anchor: Box::new(anchor().ok_or("anchor")?),
        lineage: CheckpointLineage::Root,
    };
    assert!(captured.proves_exact_coverage());
    Ok(())
}

#[test]
fn the_same_opaque_id_under_another_provider_is_another_checkpoint(
) -> Result<(), Box<dyn std::error::Error>> {
    let a = WorkspaceCheckpointRef::new(
        CheckpointProviderId::new("provider-a").ok_or("provider")?,
        CheckpointProviderVersion::new("1.0.0").ok_or("version")?,
        WorkspaceCheckpointId::new("ckpt-1").ok_or("checkpoint")?,
    );
    let b = WorkspaceCheckpointRef::new(
        CheckpointProviderId::new("provider-b").ok_or("provider")?,
        CheckpointProviderVersion::new("1.0.0").ok_or("version")?,
        WorkspaceCheckpointId::new("ckpt-1").ok_or("checkpoint")?,
    );
    assert_ne!(a, b);
    Ok(())
}

#[test]
fn a_capture_cannot_pair_one_checkpoint_with_another_anchor(
) -> Result<(), Box<dyn std::error::Error>> {
    // `Captured` carries only the anchor, so the checkpoint reference and
    // the fidelity are read back from that one source. There is no second
    // field to disagree with it.
    let captured = CheckpointCaptureOutcome::Captured {
        anchor: Box::new(anchor().ok_or("anchor")?),
        lineage: CheckpointLineage::Root,
    };
    let anchor = anchor().ok_or("anchor")?;
    assert_eq!(captured.checkpoint(), Some(anchor.checkpoint()));
    assert!(captured.proves_exact_coverage());
    Ok(())
}

#[test]
fn lineage_names_an_exact_parent_rather_than_a_movable_head(
) -> Result<(), Box<dyn std::error::Error>> {
    let forked = CheckpointLineage::ForkedFrom {
        checkpoint: WorkspaceCheckpointId::new("parent").ok_or("parent")?,
        head: CheckpointHeadId::new("line-main").ok_or("head")?,
    };
    assert_ne!(forked, CheckpointLineage::Root);
    Ok(())
}

// -----------------------------------------------------------------------
// The governed restore application boundary
// -----------------------------------------------------------------------
