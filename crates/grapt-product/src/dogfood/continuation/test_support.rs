//! Fixtures for the continuation tests: a promoted input and an empty
//! durable continuation over scratch storage.
use super::*;

#[cfg(test)]
pub(super) fn promote_test_input(
    inbox: &Arc<Mutex<Inbox>>,
    session: &SessionId,
    input: &MessageId,
    text: &str,
) -> Result<(), String> {
    let admitted = format!("evt_admit_{}", input.as_str())
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    let promoted: grapt_types::EventId = format!("evt_promote_{}", input.as_str())
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    let mut inbox = lock(inbox);
    inbox
        .admit(grapt_inbox::AdmitRequest {
            event_id: &admitted,
            session,
            input,
            text,
            delivery: grapt_types::DeliveryMode::Queue,
            resume: true,
            created_ms: 1234,
        })
        .map_err(|error| error.to_string())?;
    let result = inbox
        .promote_next(
            session,
            grapt_inbox::PromoteScope::InputAtIdle,
            &mut || promoted.clone(),
            1234,
        )
        .map_err(|error| error.to_string())?;
    if result.is_none() {
        return Err("fixture input was not promoted".to_owned());
    }
    Ok(())
}

#[cfg(test)]
#[derive(Debug)]
struct TestClock;

#[cfg(test)]
impl grapt_types::ClockAuthority for TestClock {
    fn now(&self) -> grapt_types::Timestamp {
        grapt_types::Timestamp::from_millis(1_788_220_800_000)
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(in crate::dogfood) struct TestContinuationScratch(std::path::PathBuf);

#[cfg(test)]
impl Drop for TestContinuationScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
impl DurableContinuation {
    /// This continuation after an earlier input's compaction made no progress:
    /// its next request is the trimmed recovery request.
    pub(in crate::dogfood) fn after_no_progress_for_test(
        self,
        target: &grapt_types::ResolvedModelTarget,
    ) -> Result<Self, String> {
        let registry = compaction_recovery::Registry::default();
        let earlier = "msg_d2a_no_progress"
            .parse()
            .map_err(|e: grapt_types::IdError| e.to_string())?;
        let recovery = compaction_recovery::Session::new(
            registry.clone(),
            self.session.clone(),
            earlier,
            target,
            None,
        );
        recovery.failed(compaction_recovery::Cause::NoProgress, &lock(&self.inbox))?;
        Ok(self.with_compaction_recovery(registry, target, None))
    }

    pub(in crate::dogfood) fn empty_for_test(
        label: &str,
    ) -> Result<(Self, TestContinuationScratch), String> {
        Self::with_prompt_for_test(label, "hello")
    }

    pub(in crate::dogfood) fn with_prompt_for_test(
        label: &str,
        prompt: &str,
    ) -> Result<(Self, TestContinuationScratch), String> {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use grapt_store::AggregateId;

        static ORDINAL: AtomicUsize = AtomicUsize::new(0);

        let ordinal = ORDINAL.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "grapt-d2a-continuation-{}-{label}-{ordinal}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let scratch = TestContinuationScratch(root.clone());
        let session = "ses_d2a_plan"
            .parse::<SessionId>()
            .map_err(|error| error.to_string())?;
        let input = "msg_d2a_plan"
            .parse::<MessageId>()
            .map_err(|error| error.to_string())?;
        let mut store = EventStore::open_in_memory().map_err(|error| error.to_string())?;
        store
            .elect_writer(&AggregateId::Session(session.clone()))
            .map_err(|error| error.to_string())?;
        let inbox = Arc::new(Mutex::new(
            Inbox::new(store).map_err(|error| error.to_string())?,
        ));
        let artifacts = Arc::new(
            FilesystemArtifactStore::open(
                root.join("artifacts"),
                b"d2a-continuation-test-key".to_vec(),
                Box::new(TestClock),
            )
            .map_err(|error| error.to_string())?,
        );
        promote_test_input(&inbox, &session, &input, prompt)?;
        Ok((Self::new(inbox, artifacts, session, input), scratch))
    }
}
