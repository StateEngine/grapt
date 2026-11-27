//! Admission inactivity supervision; transport owns FIFO and progress signals.
use std::future::Future;
use std::time::Duration;
use tokio::sync::watch;

/// A busy adapter may serve arbitrarily many finite attempts. Only a lack of
/// normalized progress bounds a waiter; queue movement alone cannot extend it.
pub(crate) async fn until_stalled<F: Future>(
    future: F,
    mut progress: Option<watch::Receiver<()>>,
    bound: Duration,
) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    let mut deadline = std::pin::pin!(tokio::time::sleep(bound));
    loop {
        tokio::select! {
            biased;
            result = &mut future => return Some(result),
            changed = async {
                match &mut progress {
                    Some(receiver) => receiver.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                if changed.is_err() { progress = None; }
                else { deadline.as_mut().reset(tokio::time::Instant::now() + bound); }
            }
            () = &mut deadline => {
                // Polling the reservation above may itself have observed a
                // holder's progress. Consume that edge before declaring a stall.
                if let Some(receiver) = &mut progress {
                    if matches!(receiver.has_changed(), Ok(true)) {
                        receiver.borrow_and_update();
                        deadline.as_mut().reset(tokio::time::Instant::now() + bound);
                        continue;
                    }
                }
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn active_holders_extend_the_wait_but_a_wedged_holder_does_not() {
        let (sender, receiver) = watch::channel(());
        let progress = async {
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_secs(9)).await;
                sender.send_replace(());
            }
        };
        let wait = until_stalled(
            std::future::pending::<()>(),
            Some(receiver),
            Duration::from_secs(10),
        );
        let started = tokio::time::Instant::now();
        let (result, ()) = tokio::join!(wait, progress);
        assert!(result.is_none());
        assert_eq!(started.elapsed(), Duration::from_secs(46));
    }

    #[tokio::test(start_paused = true)]
    async fn an_acquired_slot_wins_a_simultaneous_inactivity_deadline() {
        let result = until_stalled(async { 7 }, None, Duration::ZERO).await;
        assert_eq!(result, Some(7));
    }
}
