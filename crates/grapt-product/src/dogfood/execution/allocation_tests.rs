//! Step allocation resumes from persisted terminal facts across event versions.
use super::*;
use grapt_event::session_step_ended::StepEndedFact;
use grapt_event::session_step_failed::StepFailedFact;
use grapt_store::{AggregateId, EventStore};
use grapt_types::{
    InterruptionSource, ProviderCompletionDisposition, StepTerminalReason, Timestamp,
};

#[test]
fn allocation_resumes_after_both_terminal_versions() -> Result<(), Box<dyn std::error::Error>> {
    for version in [2, 3] {
        for name in ["session.step.ended", "session.step.failed"] {
            let session: SessionId = "ses_allocator".parse()?;
            let aggregate = AggregateId::Session(session.clone());
            let mut store = EventStore::open_in_memory()?;
            store.elect_writer(&aggregate)?;
            let writer = store.fencing_token(&aggregate)?;
            let step = StepNumber::new(7).ok_or("Step7")?;
            let at = grapt_event::session_step_observed::CompletionObservedAt::new(
                Timestamp::from_millis(200),
            )
            .ok_or("completion observation")?;
            let data = if name == "session.step.ended" {
                let fact =
                    StepEndedFact::new(step, ProviderCompletionDisposition::AssertedComplete);
                if version == 3 {
                    grapt_event::session_step_observed::encode_ended(
                        &grapt_event::session_step_observed::ObservedStep::new(fact, at),
                    )
                } else {
                    grapt_event::session_step_ended::encode(&fact)
                }
            } else {
                let fact = StepFailedFact::new(
                    step,
                    StepTerminalReason::Interrupted {
                        by: InterruptionSource::User,
                    },
                )?;
                if version == 3 {
                    grapt_event::session_step_observed::encode_failed(
                        &grapt_event::session_step_observed::ObservedStep::new(fact, at),
                    )?
                } else {
                    grapt_event::session_step_failed::encode(&fact)?
                }
            };
            let (_, notices) = store.append(
                grapt_store::AppendInput {
                    id: &"evt_step_version".parse()?,
                    aggregate,
                    writer,
                    kind_name: name,
                    version: grapt_types::EventVersion::new(version).ok_or("version")?,
                    created_ms: 100,
                    data: &data,
                },
                &mut [],
            )?;
            notices.discard();
            let inbox = Arc::new(Mutex::new(Inbox::new(store)?));
            assert_eq!(
                next_step(&inbox, &session)?,
                StepNumber::new(8).ok_or("Step8")?,
                "{name}.{version} was ignored by Step allocation"
            );
        }
    }
    Ok(())
}
