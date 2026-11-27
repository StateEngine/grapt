use super::task_drivers::pump_reader;
use super::*;

mod step_cancellation;

/// A Step watch with no sender and no published value. Losing the Step sender
/// removes an interrupt source; it must never stop a healthy reader.
fn quiet_step() -> watch::Receiver<Option<grapt_types::InterruptionSource>> {
    let (sender, receiver) = watch::channel(None);
    drop(sender);
    receiver
}

struct BurstThenWait {
    ready: std::collections::VecDeque<AttemptFragment>,
    tail: mpsc::Receiver<AttemptFragment>,
}

impl ProviderReader for BurstThenWait {
    async fn next(&mut self) -> Option<AttemptFragment> {
        match self.ready.pop_front() {
            Some(fragment) => Some(fragment),
            None => self.tail.recv().await,
        }
    }
}

#[tokio::test]
async fn coalesced_output_reaches_owner_before_provider_closes_the_block() {
    let delta = |text: &str| AttemptFragment::TextDelta {
        block: grapt_provider::BlockOrdinal::new(0),
        text: text.to_owned(),
        state: grapt_provider::AdapterState::none(),
    };
    let (tail_tx, tail_rx) = mpsc::channel(2);
    let (tx, mut rx) = mpsc::channel(PROVIDER_CHANNEL_CAPACITY);
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let shared = Arc::new(SharedDiagnostics::default());
    let pump = tokio::spawn(pump_reader(
        BurstThenWait {
            ready: [delta("first "), delta("second")].into(),
            tail: tail_rx,
        },
        tx,
        cancel_rx,
        quiet_step(),
        Arc::clone(&shared),
    ));
    // The provider's tail stays open and silent. Waiting for block closure
    // before forwarding this burst must fail the witness.
    match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
        Ok(Some(AttemptFragment::TextDelta { text, .. })) => assert_eq!(text, "first second"),
        other => panic!("visible burst was withheld until provider closure: {other:?}"),
    }
    assert_eq!(
        shared.provider_fragments_coalesced.load(Ordering::Relaxed),
        1
    );
    let closed = AttemptFragment::TextClosed {
        block: grapt_provider::BlockOrdinal::new(0),
        state: grapt_provider::AdapterState::none(),
    };
    if tail_tx.send(closed).await.is_err() {
        panic!("reader dropped the in-flight tail");
    }
    drop(tail_tx);
    match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
        Ok(Some(AttemptFragment::TextClosed { .. })) => {}
        other => panic!("pending read did not preserve block closure: {other:?}"),
    }
    match tokio::time::timeout(Duration::from_secs(5), pump).await {
        Ok(Ok(())) => {}
        other => panic!("reader did not settle after EOF: {other:?}"),
    }
    drop(cancel_tx);
}

/// The at-bound ACP refusal under load: a burst of ready deltas coalesced
/// into one delta too large for a single live event. Every emitted delta stays
/// within the coalescing ceiling (sized to one event), and together they carry
/// the burst exactly. The burst is ready before the first poll, so coalescing
/// is forced rather than left to load.
#[tokio::test]
async fn a_ready_burst_coalesces_only_up_to_the_ceiling_and_loses_nothing() {
    let delta = |text: String| AttemptFragment::TextDelta {
        block: grapt_provider::BlockOrdinal::new(0),
        text,
        state: grapt_provider::AdapterState::none(),
    };
    // Forty 4 KiB deltas: 160 KiB, five ceilings' worth.
    let pieces: Vec<String> = (0..40u8)
        .map(|index| char::from(b'a' + index % 26).to_string().repeat(4 * 1024))
        .collect();
    let (tail_tx, tail_rx) = mpsc::channel(2);
    let (tx, mut rx) = mpsc::channel(PROVIDER_CHANNEL_CAPACITY);
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let shared = Arc::new(SharedDiagnostics::default());
    let pump = tokio::spawn(pump_reader(
        BurstThenWait {
            ready: pieces.iter().cloned().map(delta).collect(),
            tail: tail_rx,
        },
        tx,
        cancel_rx,
        quiet_step(),
        Arc::clone(&shared),
    ));
    let expected: String = pieces.concat();
    let mut received = String::new();
    while received.len() < expected.len() {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(AttemptFragment::TextDelta { text, .. })) => {
                assert!(
                    text.len() <= AttemptFragment::MAX_COALESCED_DELTA_BYTES,
                    "a coalesced delta of {} bytes exceeds the {}-byte ceiling",
                    text.len(),
                    AttemptFragment::MAX_COALESCED_DELTA_BYTES
                );
                received.push_str(&text);
            }
            other => panic!("the burst did not arrive as text deltas: {other:?}"),
        }
    }
    assert_eq!(received, expected, "coalescing lost or reordered output");
    assert!(
        shared.provider_fragments_coalesced.load(Ordering::Relaxed) > 0,
        "the burst never coalesced, so the ceiling was not exercised"
    );
    drop(tail_tx);
    match tokio::time::timeout(Duration::from_secs(5), pump).await {
        Ok(Ok(())) => {}
        other => panic!("reader did not settle after EOF: {other:?}"),
    }
    drop(cancel_tx);
}

/// Marks its own destruction, so a witness can watch a spawned task die
/// rather than infer it.
struct DropFlag(Arc<AtomicUsize>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// The other half of the same defect: `tokio::spawn` DETACHES. A bare
/// `JoinHandle` dropped by an aborted frame leaves its task running, which
/// for the provider reader means a stream still live on a Session the
/// coordinator has already handed to a replacement drain.
///
/// Written against [`OwnedTask`] directly because the end-to-end witness
/// cannot isolate it: `pump_reader` stopping on its closed cancel channel
/// would also collect a detached reader, so only the two together prove the
/// reader dies for the right reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owned_task_dies_with_the_frame_that_spawned_it() {
    let dropped = Arc::new(AtomicUsize::new(0));
    {
        let flag = DropFlag(Arc::clone(&dropped));
        let _owned = OwnedTask(tokio::spawn(async move {
            let _flag = flag;
            std::future::pending::<()>().await;
        }));
        assert_eq!(dropped.load(Ordering::SeqCst), 0, "the task is parked");
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while dropped.load(Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the task outlived the frame that owned it"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Never yields a fragment and never ends. If [`pump_reader`] returns while
/// driving this, it is because the pump decided to stop — never because the
/// stream did.
struct HungReader;

impl ProviderReader for HungReader {
    async fn next(&mut self) -> Option<AttemptFragment> {
        std::future::pending::<Option<AttemptFragment>>().await
    }
}

/// The defect this pins: a closed `watch` is permanently ready, so the
/// biased cancel branch is chosen on every pass. Treating that as "not
/// cancelled, carry on" loops without ever polling the reader again — a
/// live task burning a core and holding the provider stream open, on a
/// Session the coordinator has already released for a replacement drain.
///
/// It is spawned rather than awaited in place because the failure mode is a
/// loop that never yields: on a single worker it would starve the timer it
/// is supposed to trip and hang the run instead of failing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn losing_every_cancel_sender_ends_the_reader_rather_than_spinning() {
    let (fragment_tx, _fragment_rx) = mpsc::channel(PROVIDER_CHANNEL_CAPACITY);
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let pump = tokio::spawn(pump_reader(
        HungReader,
        fragment_tx,
        cancel_rx,
        quiet_step(),
        Arc::new(SharedDiagnostics::default()),
    ));

    // The attempt frame that owned this reader is gone.
    drop(cancel_tx);

    match tokio::time::timeout(Duration::from_secs(5), pump).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("the reader task did not end cleanly: {error}"),
        Err(_) => panic!("the reader spun on a closed cancellation channel"),
    }
}
