use super::*;
use grapt_artifact::{ArtifactKind, ArtifactStore, CaptureBound, Sensitivity};
use grapt_types::timing::TimingStage;
use grapt_types::Timestamp;
#[derive(Debug)]
struct TickClock(AtomicU64);
impl ClockAuthority for TickClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.fetch_add(5, Ordering::Relaxed) as i64)
    }
}
fn context() -> Result<(TimingContext, TailSamples), String> {
    let (observer, samples) = TailObserver::bounded(Arc::new(TickClock(AtomicU64::new(100))));
    let session = "ses_tail_witness"
        .parse()
        .map_err(|error| format!("{error}"))?;
    let attempt =
        PhysicalAttemptId::new("att_tail_witness").ok_or("fixture attempt identity is invalid")?;
    Ok((
        TimingContext {
            observer,
            scope: TimingScope {
                session: Some(session),
                attempt: Some(attempt),
                class: TimingClass::Content,
            },
        },
        samples,
    ))
}
#[test]
fn each_span_boundary_is_attributed_to_its_injected_clock_and_owner() -> Result<(), String> {
    let (context, samples) = context()?;
    let stages = [
        TimingStage::LiveText,
        TimingStage::TextClosed,
        TimingStage::ProviderTerminal,
        TimingStage::ReceiptSettle,
        TimingStage::ReceiptRelease,
        TimingStage::AcpTerminal,
        TimingStage::AcpFinalGet,
        TimingStage::AcpEnqueue,
        TimingStage::AcpWrite,
        TimingStage::AcpFlush,
        TimingStage::ArtifactCapture,
        TimingStage::ArtifactObjectSync,
        TimingStage::ArtifactMetadataSync,
        TimingStage::StoreLock,
        TimingStage::TransactionBegin,
        TimingStage::Append,
        TimingStage::SqlCommit,
        TimingStage::ProjectionPrepare,
        TimingStage::ProjectionPublish,
        TimingStage::ReaderJoin,
        TimingStage::ToolDrain,
        TimingStage::ExecutionSettle,
    ];
    for (index, stage) in stages.into_iter().enumerate() {
        let span = context.span(stage);
        assert!(matches!(
            samples.receiver.try_recv(),
            Err(TryRecvError::Empty)
        ));
        drop(span);
        let sample = samples
            .receiver
            .try_recv()
            .map_err(|error| error.to_string())?;
        assert_eq!(sample.scope, context.scope);
        assert_eq!(sample.stage, stage);
        assert_eq!(sample.start.as_millis(), 100 + index as i64 * 10);
        assert_eq!(sample.end.as_millis(), 105 + index as i64 * 10);
    }
    assert_eq!(samples.lost.load(Ordering::Relaxed), 0);
    Ok(())
}
#[test]
fn overflow_is_lossy_and_a_drained_queue_accepts_again() -> Result<(), String> {
    let (context, samples) = context()?;
    for _ in 0..CAPACITY + 3 {
        drop(context.span(TimingStage::SqlCommit));
    }
    assert_eq!(samples.lost.load(Ordering::Relaxed), 3);
    for _ in 0..CAPACITY {
        samples
            .receiver
            .try_recv()
            .map_err(|error| error.to_string())?;
    }
    assert!(matches!(
        samples.receiver.try_recv(),
        Err(TryRecvError::Empty)
    ));
    drop(context.span(TimingStage::ReaderJoin));
    assert_eq!(
        samples
            .receiver
            .try_recv()
            .map_err(|error| error.to_string())?
            .stage,
        TimingStage::ReaderJoin
    );
    Ok(())
}
#[test]
fn artifact_sync_boundaries_include_duplicate_object_but_not_duplicate_metadata(
) -> Result<(), String> {
    let (context, samples) = context()?;
    let root = std::env::temp_dir().join(format!("grapt-tail-artifacts-{}", std::process::id()));
    let store = FilesystemArtifactStore::open(
        &root,
        b"test-key".to_vec(),
        Box::new(TickClock(AtomicU64::new(0))),
    )
    .map_err(|error| error.to_string())?
    .with_timing(context.clone());
    let bound = CaptureBound::new(64, 16).ok_or("capture bound refused")?;
    for _ in 0..2 {
        store
            .capture(
                &b"ACK"[..],
                bound,
                ArtifactKind::CapturedOutput,
                Sensitivity::Ordinary,
            )
            .map_err(|error| error.to_string())?;
    }
    let observed: Vec<_> = samples.receiver.try_iter().collect();
    assert_eq!(
        observed
            .iter()
            .map(|sample| sample.stage)
            .collect::<Vec<_>>(),
        [
            TimingStage::ArtifactObjectSync,
            TimingStage::ArtifactMetadataSync,
            TimingStage::ArtifactCapture,
            TimingStage::ArtifactObjectSync,
            TimingStage::ArtifactCapture,
        ]
    );
    assert!(observed
        .iter()
        .all(|sample| sample.scope == context.scope && sample.end > sample.start));
    std::fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
    Ok(())
}

#[tokio::test]
async fn stopped_stderr_drops_samples_and_releases_session_wake(
) -> Result<(), Box<dyn std::error::Error>> {
    use super::super::wake_custody::WakeSlots;
    use crate::dogfood::prompt_driver::Started;
    use std::io::Read;
    let (mut reader, writer) = std::io::pipe()?;
    let (context, mut samples) = context()?;
    samples.output = LossyOutput::start(writer, Arc::clone(&samples.lost), LOSS_FIELD)?;
    let slots = WakeSlots::default();
    let session: SessionId = "ses_stopped_stderr".parse()?;
    let (started, _accepted) = Started::channel();
    let (_stop, shutdown) = tokio::sync::watch::channel(false);
    let owner = slots
        .reserve(&session)?
        .enter(&started, shutdown.clone())
        .await?
        .ok_or("wake cancelled")?;
    // Fill the real pipe and its bounded output queue without a reader. This
    // must return even if the detached writer's first write remains blocked.
    for _ in 0..512 {
        samples.output.send("x".repeat(8192));
    }
    let before = samples.lost.load(Ordering::Relaxed);
    assert!(before > 0, "stopped pipe did not saturate output");
    let (completed, finish) = tokio::sync::oneshot::channel();
    let _finisher = std::thread::spawn(move || {
        for _ in 0..CAPACITY {
            drop(context.span(TimingStage::ExecutionSettle));
        }
        samples.report(); // same production tail drain before execution return
        let lost = samples.lost.load(Ordering::Relaxed);
        drop(owner); // same custody release after execution return
        drop(samples);
        let _sent = completed.send(lost);
    });
    // The pipe stays blocked until custody assertions finish. Allow thread
    // scheduling delay while still refusing a writer that waits for its sink.
    let lost = tokio::time::timeout(std::time::Duration::from_secs(5), finish).await??;
    assert!(lost > before, "dropped tail samples were not counted");
    let successor = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        slots.reserve(&session)?.enter(&started, shutdown),
    )
    .await??
    .ok_or("successor cancelled")?;
    drop(successor);
    // Recover the reader only after reply/custody assertions. A resumed sink
    // reports the lifetime loss; blocked diagnostics never join the executor.
    let retained = tokio::task::spawn_blocking(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map(|_| bytes)
    });
    let retained = tokio::time::timeout(std::time::Duration::from_secs(3), retained).await???;
    assert!(String::from_utf8_lossy(&retained)
        .contains("GRAPT_PROVIDER_TAIL_LOSS tail_count_at_least="));
    Ok(())
}
