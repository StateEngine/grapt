//! Restart acknowledgement must follow the interrupted owner's release.
use super::*;

#[tokio::test]
async fn restart_cannot_acknowledge_while_the_previous_owner_is_alive() -> Result<(), String> {
    let slots = WakeSlots::default();
    let session = "ses_interrupt_release"
        .parse::<SessionId>()
        .map_err(|error| error.to_string())?;
    let (started, _response) = Started::channel();
    let (_shutdown, stopped) = watch::channel(false);
    let owner = slots
        .reserve(&session)?
        .enter(&started, stopped.clone())
        .await?
        .ok_or("owner cancelled")?;
    let replacement = slots.reserve(&session)?.after_interrupt();
    let (waiting, mut accepted) = Started::channel();
    let enter = replacement.enter(&waiting, stopped);
    tokio::pin!(enter);
    tokio::select! {
        result = &mut enter => return Err(format!("restart entered before release: {}", result.is_ok())),
        _ = tokio::time::sleep(Duration::from_millis(20)) => {}
    }
    assert!(matches!(
        accepted.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    drop(owner);
    let guard = enter.await?.ok_or("restart cancelled")?;
    // The executor, rather than the mutex waiter, acknowledges actual startup.
    assert!(matches!(
        accepted.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    waiting.settle(Ok(()));
    assert_eq!(accepted.await.map_err(|error| error.to_string())??, None);
    drop(guard);
    assert!(slots.reserve_idle(&session).is_ok());
    Ok(())
}

#[tokio::test]
async fn restart_timeout_and_cancellation_release_capacity_without_a_success_ack(
) -> Result<(), String> {
    let slots = WakeSlots::default();
    let session = "ses_interrupt_timeout"
        .parse::<SessionId>()
        .map_err(|error| error.to_string())?;
    let (started, _response) = Started::channel();
    let (_shutdown, stopped) = watch::channel(false);
    let owner = slots
        .reserve(&session)?
        .enter(&started, stopped.clone())
        .await?
        .ok_or("owner cancelled")?;
    let mut replacement = slots.reserve(&session)?.after_interrupt();
    replacement.interrupt_wait = Some(Duration::from_millis(10));
    let (waiting, mut accepted) = Started::channel();
    let result = replacement.enter(&waiting, stopped.clone()).await;
    let error = match result {
        Err(error) => error,
        Ok(_) => return Err("restart ignored its deadline".into()),
    };
    assert!(error.contains("message remains queued"));
    assert!(matches!(
        accepted.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    let cancelled = slots.reserve(&session)?.after_interrupt();
    slots.cancel_waiters(&session);
    assert!(cancelled.enter(&waiting, stopped).await.is_err());
    drop(owner);
    assert!(slots.reserve_idle(&session).is_ok());
    Ok(())
}

#[test]
fn idle_release_keeps_a_successor_reservation_closed() -> Result<(), String> {
    let slots = Arc::new(WakeSlots::default());
    let session = "ses_idle_handoff"
        .parse::<SessionId>()
        .map_err(|error| error.to_string())?;
    let mut first = slots.reserve_idle(&session)?;
    let slot = Arc::clone(&first.slot);
    let probe = Arc::new(Mutex::new(None));
    let observed = Arc::clone(&probe);
    let contender = Arc::clone(&slots);
    let target = session.clone();
    // Force the release gap without depending on thread scheduling. An atomic
    // release may hold the state lock here; never block that correct handoff.
    first.after_capacity_release = Some(Box::new(move || {
        let replacement = match slot.state.try_lock() {
            Ok(state) => {
                drop(state);
                contender.reserve_idle(&target).map(Some)
            }
            Err(std::sync::TryLockError::WouldBlock) => Ok(None),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                Err("idle handoff state was poisoned".to_owned())
            }
        };
        *observed.lock().unwrap_or_else(|p| p.into_inner()) = Some(replacement);
    }));
    drop(first);
    let replacement = probe
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .ok_or("idle release probe did not run")??;
    let successor = match replacement {
        Some(reservation) => reservation,
        None => slots.reserve_idle(&session)?,
    };
    assert!(
        slots.reserve(&session).is_err(),
        "the old release cleared its successor's admission fence"
    );
    drop(successor);
    assert!(slots.reserve(&session).is_ok());
    Ok(())
}

#[tokio::test]
async fn stop_waits_for_a_cancelled_parked_owner_and_its_completion() -> Result<(), String> {
    use std::future::{poll_fn, Future};
    use std::task::Poll;

    let slots = WakeSlots::default();
    let session = "ses_parked_stop"
        .parse::<SessionId>()
        .map_err(|error| error.to_string())?;
    let waiting = slots.reserve(&session)?;
    let completion = waiting.completion();
    let stop = match slots.reserve_stop(&session, false, false, None, || Ok((None, ())))? {
        StopAdmission::Owner(stop, ()) => stop,
        StopAdmission::Joined(_) => return Err("unexpected joined stop".into()),
    };
    slots.cancel_waiters(&session);
    {
        let drained = stop.drained();
        tokio::pin!(drained);
        poll_fn(|cx| {
            assert!(
                drained.as_mut().poll(cx).is_pending(),
                "stop drained while its cancelled parked owner was alive"
            );
            Poll::Ready(())
        })
        .await;
        drop(waiting);
        poll_fn(|cx| {
            assert!(
                drained.as_mut().poll(cx).is_pending(),
                "stop drained before the parked owner's retained completion"
            );
            Poll::Ready(())
        })
        .await;
        drop(completion);
        drop(drained.await?);
    }
    stop.finish(Ok(()));
    Ok(())
}
