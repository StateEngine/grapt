use super::*;
use grapt_types::timing::TimingSample;
use grapt_types::{EventVersion, Timestamp};
use std::sync::{
    atomic::{AtomicI64, Ordering},
    mpsc::{sync_channel, Receiver, SyncSender},
};
#[derive(Debug)]
struct Observer {
    clock: AtomicI64,
    samples: SyncSender<TimingSample>,
}
impl TimingObserver for Observer {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.clock.fetch_add(5, Ordering::Relaxed))
    }
    fn record(&self, sample: TimingSample) {
        assert!(
            self.samples.try_send(sample).is_ok(),
            "test observation lost"
        );
    }
}
fn observer() -> (Arc<Observer>, Receiver<TimingSample>) {
    let (samples, receiver) = sync_channel(32);
    (
        Arc::new(Observer {
            clock: AtomicI64::new(100),
            samples,
        }),
        receiver,
    )
}
#[test]
fn four_tail_commits_keep_their_owner_and_injected_boundaries(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let (observer, samples) = observer();
    store.install_timing_observer(observer);
    let session = "ses_timing_commits".parse()?;
    let aggregate = AggregateId::Session(session);
    for (name, class) in [
        ("session.content.appended", TimingClass::Content),
        ("session.usage.observed", TimingClass::Usage),
        ("session.step.ended", TimingClass::Step),
        ("session.execution.succeeded", TimingClass::Execution),
    ] {
        let tx = store.governed_transaction()?;
        tx.attribute_timing(&aggregate, name);
        // Later operational facts (including receipt settlement) cannot erase
        // the terminal that owns this transaction.
        tx.attribute_timing(&aggregate, "session.input.admitted");
        tx.execute_batch("CREATE TABLE IF NOT EXISTS timing_witness (value INTEGER)")?;
        tx.commit()?.discard();
        let begin = samples.try_recv()?;
        let commit = samples.try_recv()?;
        assert_eq!(begin.stage, TimingStage::TransactionBegin);
        assert_eq!(commit.stage, TimingStage::SqlCommit);
        assert_eq!(begin.scope.class, class);
        assert_eq!(commit.scope, begin.scope);
        assert_eq!(
            commit.scope.session.as_ref(),
            match &aggregate {
                AggregateId::Session(id) => Some(id),
                _ => None,
            }
        );
        assert_eq!(begin.end.as_millis() - begin.start.as_millis(), 5);
        assert_eq!(commit.start.as_millis(), begin.end.as_millis() + 5);
        assert_eq!(commit.end.as_millis() - commit.start.as_millis(), 5);
    }
    assert!(samples.try_recv().is_err());
    Ok(())
}
#[derive(Debug)]
struct Projection;
#[derive(Debug)]
struct Prepared;
impl crate::commit_projection::CommittedReadProjection for Projection {
    fn affected(&self, _: grapt_event::EventKind) -> bool {
        true
    }
    fn prepare(
        &self,
        _: &crate::commit_projection::ProjectionConnection,
    ) -> Box<dyn crate::commit_projection::PreparedReadProjection> {
        Box::new(Prepared)
    }
}
impl crate::commit_projection::PreparedReadProjection for Prepared {
    fn publish(self: Box<Self>) {}
}
#[test]
fn actual_append_projection_and_commit_are_separate_ordered_spans(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let aggregate = AggregateId::Session("ses_timing_append".parse()?);
    store.elect_writer(&aggregate)?;
    store.install_read_projection(Arc::new(Projection));
    let writer = store.fencing_token(&aggregate)?;
    let (observer, samples) = observer();
    store.install_timing_observer(observer);
    let id = "evt_timing_append".parse()?;
    let data = serde_json::json!({"sessionID":"ses_timing_append","inputID":"msg_timing"});
    let (_, notices) = store.append(
        crate::AppendInput {
            aggregate: aggregate.clone(),
            writer,
            id: &id,
            kind_name: "session.input.admitted",
            version: EventVersion::new(1).ok_or("version refused")?,
            created_ms: 1,
            data: &data,
        },
        &mut [],
    )?;
    notices.discard();
    let observed: Vec<_> = samples.try_iter().collect();
    assert_eq!(
        observed
            .iter()
            .map(|sample| sample.stage)
            .collect::<Vec<_>>(),
        [
            TimingStage::Append,
            TimingStage::TransactionBegin,
            TimingStage::ProjectionPrepare,
            TimingStage::SqlCommit,
            TimingStage::ProjectionPublish,
        ]
    );
    for sample in &observed {
        assert_eq!(sample.end.as_millis() - sample.start.as_millis(), 5);
    }
    // Reporting order follows completion, but the timestamps recover the real
    // begin -> append -> prepare -> COMMIT -> publish order.
    assert!(observed[1].end <= observed[0].start);
    assert!(observed[0].end <= observed[2].start);
    assert!(observed[2].end <= observed[3].start);
    assert!(observed[3].end <= observed[4].start);
    Ok(())
}
