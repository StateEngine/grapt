//! Governed compaction and prepublication-failure accounting through the shipped command.
use grapt_event::compaction_call::{self, CallCoordinates, CallEnd};
use grapt_event::session_message::UsageObservedFact;
use grapt_store::{
    AggregateId, AppendInput, AttemptRecordCeiling, EventStore, ReserveInput, TerminalCoordinates,
};
use grapt_types::*;
use std::num::NonZeroU32;
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn sc_acct01_failed_compactions_and_unpublished_failures_remain_reconciled() -> TestResult {
    let root = std::env::temp_dir().join(format!("acct01-failed-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let session: SessionId = "ses_acct01_failed".parse()?;
    {
        let mut store = EventStore::open(&root.join("database.sqlite3"), "acct01-failed")?;
        store.elect_writer(&AggregateId::Session(session.clone()))?;
        let writer = store.fencing_token(&AggregateId::Session(session.clone()))?;
        let bound = NonZeroU32::new(4096).ok_or("bound")?;
        let ceiling = AttemptRecordCeiling::new(bound);
        for (index, name) in [
            (1, "summary_observed"),
            (2, "summary_unreported"),
            (3, "prepublication"),
        ] {
            let step = StepNumber::new(index).ok_or("step")?;
            let attempt = PhysicalAttemptId::new(name).ok_or("attempt")?;
            let terminal = TerminalCoordinates::new(format!("evt_failed_{index}").parse()?, 500);
            let receipt = DurableAttemptReceipt::reserved(
                BoundedAttemptId::new(attempt.clone(), bound).map_err(|e| format!("{e:?}"))?,
                BoundedSessionId::new(session.clone(), bound).map_err(|e| format!("{e:?}"))?,
                step,
                AttemptOrdinal::new(1).ok_or("ordinal")?,
                ReservedMarker::new(Timestamp::from_millis(100)),
            );
            store.reserve_attempt_receipt(&ReserveInput {
                receipt: &receipt,
                writer,
                terminal: &terminal,
                ceiling,
            })?;
            let tx = store.governed_transaction()?;
            EventStore::permit_attempt_send_within(
                &tx,
                &attempt,
                writer,
                ceiling,
                RequestSentMarker::new(Timestamp::from_millis(120)),
            )?;
            tx.commit()?.discard();
            let tx = store.governed_transaction()?;
            if index <= 2 {
                let coordinates = CallCoordinates {
                    compaction: CompactionId::new(format!("compact_{index}"))
                        .ok_or("compaction")?,
                    attempt: attempt.clone(),
                    step,
                };
                let mut facts = vec![("started", compaction_call::encode_started(&coordinates))];
                if index == 1 {
                    let field = |n| TokenCount::new(n).map(UsageField::reported);
                    let usage = AttemptUsage::observed(
                        field(40)?,
                        field(30)?,
                        field(2)?,
                        UsageField::ABSENT,
                        field(10)?,
                        UsageField::ABSENT,
                        UsageField::ABSENT,
                        UsageObservationPhase::Terminal,
                    );
                    facts.push((
                        "usage",
                        grapt_event::session_message::encode_observed(&UsageObservedFact::new(
                            attempt.clone(),
                            usage,
                        )),
                    ));
                }
                facts.push((
                    "ended",
                    compaction_call::encode_ended(&coordinates, CallEnd::Failed),
                ));
                for (suffix, data) in facts {
                    EventStore::append_within(
                        &tx,
                        AppendInput {
                            id: &format!("evt_{name}_{suffix}").parse()?,
                            aggregate: AggregateId::Session(session.clone()),
                            writer,
                            kind_name: &format!("session.compaction.call.{suffix}"),
                            version: EventVersion::new(1).ok_or("version")?,
                            created_ms: 200,
                            data: &data,
                        },
                        &mut [],
                    )?;
                }
            }
            let fact = grapt_event::session_step_failed::StepFailedFact::new(
                step,
                StepTerminalReason::ProviderFailure {
                    class: ProviderFailureClass::Transport,
                },
            )?;
            grapt_store::step_failed::record(
                &tx,
                &session,
                terminal.event(),
                writer,
                500,
                &fact,
                &mut [],
            )?;
            EventStore::settle_attempt_ordinary_terminal_within(
                &tx,
                &attempt,
                writer,
                ceiling,
                SettledMarker::new(Timestamp::from_millis(600)),
            )?;
            tx.commit()?.discard();
            assert!(store.attempt_receipt(&attempt, ceiling)?.is_none());
        }
    }
    let before = std::fs::read(root.join("database.sqlite3"))?;
    let output = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["usage", "--service"])
        .arg(&root)
        .args(["--session", session.as_str()])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["session_totals"]["status"], "complete");
    assert_eq!(report["session_totals"]["dispatched_attempts"], 3);
    assert_eq!(report["session_totals"]["observed_usage_attempts"], 1);
    assert_eq!(report["session_totals"]["unavailable_usage_attempts"], 2);
    assert_eq!(report["observed_subtotals"]["input_total"]["tokens"], 40);
    let rows = report["attempts"].as_array().ok_or("attempts")?;
    assert_eq!(
        rows.iter()
            .filter(|a| a["operation"] == "compaction")
            .count(),
        2
    );
    assert!(rows.iter().all(|a| a["step_terminal_observed"] == true));
    assert_eq!(before, std::fs::read(root.join("database.sqlite3"))?);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
