use super::*;
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, CompactionDescriptor, CompactionDescriptorParts,
    CompactionId, CompactionSummary, CompactionTrigger, ContextEpoch, ContextWindowId,
    NextRequestRef, RequestContextIdentity, RetainedHistory, RetentionPolicyVersion,
};

struct WindowedPlan {
    planner: RegistryPlan,
    context: RequestContextIdentity,
}
impl AttemptPlan for WindowedPlan {
    fn plan(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        let mut planned = self.planner.plan(step, ordinal, generation)?;
        planned.context = Some(self.context.clone());
        Ok(planned)
    }
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn scc_request_context_joins_receipt_atomically_and_rejects_stale_windows(
) -> Result<(), Box<dyn std::error::Error>> {
    for mode in ["matching", "missing", "wrong-epoch"] {
        let context = RequestContextIdentity {
            window: ContextWindowId::new("scc-window").ok_or("window")?,
            epoch: ContextEpoch::new(1),
            descriptor: "evt_scc_descriptor".parse()?,
        };
        let planner = WindowedPlan {
            planner: RegistryPlan {
                registry: ModelRegistry::install(catalog("scc-context")),
            },
            context: context.clone(),
        };
        let opener = Arc::new(AttemptOpener::deterministic(
            [transcript_script()],
            route(),
            credential(),
        ));
        let mut harness = harness("ses_scc_context", opener, Box::new(planner));
        if mode != "missing" {
            let artifact = ArtifactRef::new(
                ArtifactId::new("scc-reference").ok_or("artifact")?,
                12,
                ArtifactDigest::new("fixture-reference").ok_or("digest")?,
            );
            let descriptor = CompactionDescriptor::new(CompactionDescriptorParts {
                id: CompactionId::new("scc-compaction").ok_or("compaction")?,
                source_session: harness.session.clone(),
                source_epoch: ContextEpoch::FIRST,
                target_window: context.window.clone(),
                target_epoch: ContextEpoch::new(if mode == "wrong-epoch" { 2 } else { 1 }),
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
                &context.descriptor,
                writer,
                1_724_000_000,
                grapt_store::compaction::CompactionAttempt::Settled(&descriptor),
                &mut [],
            )?;
            tx.commit()?.discard();
        }
        let (finished, report) = run(harness, policy(Duration::ZERO)).await;
        harness = finished;
        let inbox = harness.inbox.lock().map_err(|_| "lock")?;
        let rows: Vec<serde_json::Value> = {
            let mut statement = inbox
                .store()
                .connection()
                .prepare("SELECT data FROM event WHERE type='session.request.context.1'")?;
            let values = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            values
                .into_iter()
                .map(|value| serde_json::from_str(&value))
                .collect::<Result<Vec<_>, _>>()?
        };
        if mode == "matching" {
            assert_eq!(rows.len(), 1);
            let (observed, attempt, step) = grapt_event::request_context::decode(&rows[0])?;
            assert_eq!(observed, context);
            assert_eq!(step, harness.step);
            assert!(matches!(report.result, StepResult::Ended { .. }));
            assert_eq!(report.receipts.len(), 1);
            assert_eq!(report.receipts[0].attempt, attempt);
            assert!(
                inbox
                    .store()
                    .attempt_receipt(&attempt, ceiling())?
                    .is_none(),
                "successful Step settlement releases its transient receipt"
            );
        } else {
            assert!(matches!(report.result, StepResult::AdmissionRefused { .. }));
            assert!(
                rows.is_empty(),
                "refused receipt must not leave a context fact"
            );
            assert!(inbox
                .store()
                .attempt_receipt(&attempt("pat_u2_1"), ceiling())?
                .is_none());
        }
        assert_eq!(harness.source.ledger().held(), 0);
    }
    Ok(())
}
