//! Step cancellation reaches the actual reader without outer-driver forwarding.
use super::*;

struct ObservedReader {
    polls: Arc<AtomicUsize>,
    fragments: std::vec::IntoIter<AttemptFragment>,
}
impl ProviderReader for ObservedReader {
    async fn next(&mut self) -> Option<AttemptFragment> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.fragments.next()
    }
}

async fn current_step_value_is_observed(sender_closed: bool, published: bool) {
    let value = published.then_some(InterruptionSource::User);
    let (step_tx, step_rx) = watch::channel(value);
    let step_owner = if sender_closed {
        None
    } else {
        Some(step_tx.clone())
    };
    drop(step_tx);
    let (attempt_tx, attempt_rx) = watch::channel(None);
    let (tx, _rx) = mpsc::channel(1);
    let polls = Arc::new(AtomicUsize::new(0));
    let reader = ObservedReader {
        polls: Arc::clone(&polls),
        fragments: Vec::new().into_iter(),
    };
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        pump_reader(
            reader,
            tx,
            attempt_rx,
            step_rx,
            Arc::new(SharedDiagnostics::default()),
        ),
    )
    .await;
    assert!(result.is_ok(), "reader did not settle");
    assert_eq!(
        polls.load(Ordering::SeqCst),
        usize::from(!published),
        "a published Step cancellation must prevent the first provider poll"
    );
    drop(step_owner);
    drop(attempt_tx);
}

#[tokio::test]
async fn current_step_cancel_prevents_first_provider_poll() {
    current_step_value_is_observed(false, true).await;
}

#[tokio::test]
async fn published_step_cancel_survives_sender_closure() {
    current_step_value_is_observed(true, true).await;
}

#[tokio::test]
async fn closed_uncancelled_step_keeps_provider_healthy() {
    current_step_value_is_observed(true, false).await;
}

#[tokio::test]
async fn step_cancel_releases_reader_blocked_by_fragment_backpressure() {
    let (step_tx, step_rx) = watch::channel(None);
    let (attempt_tx, attempt_rx) = watch::channel(None);
    let (tx, _rx) = mpsc::channel(1);
    let shared = Arc::new(SharedDiagnostics::default());
    let reader = ObservedReader {
        polls: Arc::new(AtomicUsize::new(0)),
        fragments: vec![
            AttemptFragment::AttemptOpened,
            AttemptFragment::AttemptOpened,
        ]
        .into_iter(),
    };
    let mut task = tokio::spawn(pump_reader(
        reader,
        tx,
        attempt_rx,
        step_rx,
        Arc::clone(&shared),
    ));
    let blocked = tokio::time::timeout(Duration::from_secs(1), async {
        while shared.provider_contention_events.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    if blocked.is_err() {
        task.abort();
        let _ = task.await;
        panic!("reader never reached the full fragment channel");
    }
    step_tx.send_replace(Some(InterruptionSource::User));
    let settled = tokio::time::timeout(Duration::from_millis(250), &mut task).await;
    let stopped = matches!(settled, Ok(Ok(())));
    if !stopped {
        task.abort();
        let _ = task.await;
    }
    drop(attempt_tx);
    assert!(
        stopped,
        "Step cancellation did not release the backpressured reader"
    );
}

struct DeltaThenPending(bool);
impl ProviderReader for DeltaThenPending {
    async fn next(&mut self) -> Option<AttemptFragment> {
        if !self.0 {
            self.0 = true;
            return Some(AttemptFragment::TextDelta {
                block: grapt_provider::BlockOrdinal::new(0),
                text: "pending output".to_owned(),
                state: grapt_provider::AdapterState::none(),
            });
        }
        std::future::pending().await
    }
}

#[tokio::test]
async fn step_cancel_during_coalesced_flush_drops_the_pending_provider_read() {
    let (step_tx, step_rx) = watch::channel(None);
    let (attempt_tx, attempt_rx) = watch::channel(None);
    let (tx, _rx) = mpsc::channel(1);
    assert!(tx.send(AttemptFragment::AttemptOpened).await.is_ok());
    let shared = Arc::new(SharedDiagnostics::default());
    let mut task = tokio::spawn(pump_reader(
        DeltaThenPending(false),
        tx,
        attempt_rx,
        step_rx,
        Arc::clone(&shared),
    ));
    let blocked = tokio::time::timeout(Duration::from_secs(1), async {
        while shared.provider_contention_events.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    if blocked.is_err() {
        task.abort();
        let _ = task.await;
        panic!("coalesced output never reached backpressure");
    }
    step_tx.send_replace(Some(InterruptionSource::User));
    let stopped = matches!(
        tokio::time::timeout(Duration::from_millis(250), &mut task).await,
        Ok(Ok(()))
    );
    if !stopped {
        task.abort();
        let _ = task.await;
    }
    drop(attempt_tx);
    assert!(
        stopped,
        "coalesced flush consumed cancellation but resumed the pending provider read"
    );
}
