use super::*;
use crate::{AttemptOwner, CompactionContext};

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn scc_summary_shares_receipt_custody_without_reusing_attempt_ordinals(
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = ModelRegistry::install(catalog("scc-summary"));
    let opener = Arc::new(AttemptOpener::deterministic(
        [
            transcript_script(),
            transcript_script(),
            transcript_script(),
            transcript_script(),
            transcript_script(),
            transcript_script(),
        ],
        route(),
        credential(),
    ));
    let mut harness = harness(
        "ses_scc_summary",
        opener,
        Box::new(RegistryPlan { registry }),
    );
    let mut port = harness.source.compaction_call();
    let (_handle, cancellation) = step_interrupt();
    let context = CompactionContext {
        trigger: grapt_types::CompactionTrigger::ContextOverflow,
        step: harness.step,
        preceding_ordinal: 1,
        generation: HistoryGeneration::new(0),
        cancellation,
        max_attempts: crate::step::MAX_TOTAL_ATTEMPTS,
    };
    let mut summary_planner = RegistryPlan {
        registry: ModelRegistry::install(catalog("scc-summary-plan")),
    };
    let before = summary_planner.plan(harness.step, 1, context.generation)?;
    assert!(port
        .run(context.clone(), before, RecordingOwner::new())
        .await
        .is_err());
    assert_eq!(harness.source.ledger().held(), 0);
    // The first coding attempt was admitted; its stream need not be polled to
    // witness that the separate summary cannot reuse its receipt identity.
    let first = harness.source.prepare(harness.step, 1, context.generation);
    assert_eq!(harness.source.ledger().held(), 1);
    let planned = summary_planner.plan(harness.step, 2, context.generation)?;
    let (_, summary) = port
        .run(context.clone(), planned, RecordingOwner::new())
        .await?;
    assert!(matches!(
        summary.outcome,
        StepOutcome::Ended {
            finish: FinishReason::Stop
        }
    ));
    assert_eq!(harness.source.ledger().held(), 2);
    {
        let inbox = super::super::compaction::lock(&harness.inbox);
        let receipt = inbox
            .store()
            .attempt_receipt(&summary.attempt, ceiling())?
            .ok_or("missing summary receipt")?;
        assert_eq!(receipt.receipt().ordinal().value(), 2);
    }
    // The first call committed nothing, so ONE fallback call is admitted, at
    // the next receipt ordinal.
    let fallback = summary_planner.plan(harness.step, 3, context.generation)?;
    let (_, fallback) = port
        .run(context.clone(), fallback, RecordingOwner::new())
        .await?;
    assert_eq!(harness.source.ledger().held(), 3);
    {
        let inbox = super::super::compaction::lock(&harness.inbox);
        let receipt = inbox
            .store()
            .attempt_receipt(&fallback.attempt, ceiling())?
            .ok_or("missing fallback receipt")?;
        assert_eq!(receipt.receipt().ordinal().value(), 3);
        assert_ne!(fallback.attempt, summary.attempt);
    }
    // The SE fallback may need one repair, with its own receipt.
    let repair = summary_planner.plan(harness.step, 4, context.generation)?;
    let (_, repair) = port
        .run(context.clone(), repair, RecordingOwner::new())
        .await?;
    assert_eq!(harness.source.ledger().held(), 4);
    // A fourth summary call, the last the compaction may make.
    let fourth = summary_planner.plan(harness.step, 5, context.generation)?;
    let (_, fourth) = port
        .run(context.clone(), fourth, RecordingOwner::new())
        .await?;
    assert_eq!(harness.source.ledger().held(), 5);
    let fifth = summary_planner.plan(harness.step, 6, context.generation)?;
    let refused = port
        .run(context.clone(), fifth, RecordingOwner::new())
        .await
        .err()
        .ok_or("a fifth summary call was admitted")?;
    assert_eq!(refused, "summary call is repeated or names stale history");
    let rebuilt = harness
        .source
        .prepare(harness.step, 2, HistoryGeneration::new(1));
    assert_eq!(harness.source.ledger().held(), 6);
    {
        let entries = harness.source.ledger().lock();
        let rebuilt_id = &entries.last().ok_or("missing rebuilt custody")?.attempt;
        let inbox = super::super::compaction::lock(&harness.inbox);
        let receipt = inbox
            .store()
            .attempt_receipt(rebuilt_id, ceiling())?
            .ok_or("missing rebuilt receipt")?;
        // The next coding attempt follows the last summary call's receipt.
        assert_eq!(receipt.receipt().ordinal().value(), 6);
        assert_ne!(rebuilt_id, &summary.attempt);
        assert_ne!(rebuilt_id, &fallback.attempt);
        assert_ne!(rebuilt_id, &repair.attempt);
        assert_ne!(rebuilt_id, &fourth.attempt);
    }
    drop(first);
    drop(rebuilt);
    let mut owner = harness.take_owner();
    owner.commit(DurableFact::StepEnded {
        finish: FinishReason::Stop,
    })?;
    assert_eq!(harness.source.ledger().held(), 0);
    assert!(receipt_row(&harness.inbox, &summary.attempt).is_none());
    Ok(())
}

/// A first summary call whose compaction COMMITTED leaves no room for a
/// fallback: the second call is refused.
#[tokio::test]
async fn scc_no_fallback_summary_call_after_a_committed_compaction(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_types::{
        ArtifactDigest, ArtifactId, ArtifactRef, CompactionDescriptor, CompactionDescriptorParts,
        CompactionId, CompactionSummary, CompactionTrigger, ContextEpoch, ContextWindowId,
        NextRequestRef, RetainedHistory, RetentionPolicyVersion,
    };
    let opener = Arc::new(AttemptOpener::deterministic(
        [
            transcript_script(),
            transcript_script(),
            transcript_script(),
        ],
        route(),
        credential(),
    ));
    let mut harness = harness(
        "ses_scc_committed",
        opener,
        Box::new(RegistryPlan {
            registry: ModelRegistry::install(catalog("scc-committed")),
        }),
    );
    let mut port = harness.source.compaction_call();
    let (_handle, cancellation) = step_interrupt();
    let context = CompactionContext {
        trigger: CompactionTrigger::ContextOverflow,
        step: harness.step,
        preceding_ordinal: 1,
        generation: HistoryGeneration::new(0),
        cancellation,
        max_attempts: crate::step::MAX_TOTAL_ATTEMPTS,
    };
    let mut planner = RegistryPlan {
        registry: ModelRegistry::install(catalog("scc-committed-plan")),
    };
    let first = harness.source.prepare(harness.step, 1, context.generation);
    let planned = planner.plan(harness.step, 2, context.generation)?;
    port.run(context.clone(), planned, RecordingOwner::new())
        .await?;
    // The compaction the first call fed commits its descriptor.
    {
        let artifact = ArtifactRef::new(
            ArtifactId::new("scc-committed").ok_or("artifact")?,
            12,
            ArtifactDigest::new("fixture-committed").ok_or("digest")?,
        );
        let descriptor = CompactionDescriptor::new(CompactionDescriptorParts {
            id: CompactionId::new("scc-committed").ok_or("compaction")?,
            source_session: harness.session.clone(),
            source_epoch: ContextEpoch::FIRST,
            target_window: ContextWindowId::new("scc-committed").ok_or("window")?,
            target_epoch: ContextEpoch::new(1),
            trigger: CompactionTrigger::ContextOverflow,
            retention_policy: RetentionPolicyVersion::new("fixture").ok_or("policy")?,
            summary: CompactionSummary::ClientReadable {
                artifact: artifact.clone(),
            },
            retained: RetainedHistory::DeterministicSelection {
                descriptor: artifact.clone(),
            },
            baseline: artifact.clone(),
            next_request: NextRequestRef::new(harness.step, artifact),
        })
        .map_err(|error| format!("{error:?}"))?;
        let mut inbox = harness.inbox.lock().map_err(|_| "lock")?;
        let aggregate = AggregateId::Session(harness.session.clone());
        let writer = inbox.store().fencing_token(&aggregate)?;
        let tx = inbox.store_mut().governed_transaction()?;
        grapt_store::compaction::record(
            &tx,
            &"evt_scc_committed".parse()?,
            writer,
            1_724_000_000,
            grapt_store::compaction::CompactionAttempt::Settled(&descriptor),
            &mut [],
        )?;
        tx.commit()?.discard();
    }
    let fallback = planner.plan(harness.step, 3, context.generation)?;
    let refused = port
        .run(context.clone(), fallback, RecordingOwner::new())
        .await
        .err()
        .ok_or("a fallback after a committed compaction was admitted")?;
    assert_eq!(refused, "summary call is repeated or names stale history");
    drop(first);
    Ok(())
}
