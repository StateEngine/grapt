use super::tests::{execution, hub, session};
use super::*;
use grapt_api::u3a::{DogfoodEventSubscription, SessionStatusInfo};
use grapt_runtime::step::RetryNotice;
use grapt_store::{OsRandomIds, SystemClock};
use std::time::Duration;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn drain(sub: &DogfoodEventSubscription) -> Result<Vec<String>> {
    sub.cancel();
    let mut rows = Vec::new();
    while let Some(frame) = sub.next_frame()? {
        assert!(rows.len() < 16);
        rows.push(String::from_utf8(frame.to_vec())?);
    }
    Ok(rows)
}
fn setup() -> Result<(
    Arc<LiveActiveExecutions>,
    Arc<super::super::super::session_status::SessionStatusEmitter>,
    Arc<grapt_api::u3a::DogfoodEventHub>,
)> {
    let hub = hub();
    let emitter = Arc::new(
        super::super::super::session_status::SessionStatusEmitter::new(
            Arc::new(OsRandomIds::seed()?),
            Arc::new(SystemClock),
            Arc::clone(&hub),
        ),
    );
    let active = Arc::new(LiveActiveExecutions::with_status(Arc::clone(&emitter)));
    let (a, e) = (Arc::clone(&active), Arc::clone(&emitter));
    assert!(hub.install_status_hydrator(Box::new(move || {
        let mut frames = Vec::new();
        let listed = a.offer_status_hydration(&e, &mut frames);
        e.finish_hydration(&listed, &mut frames);
        frames
    })));
    Ok((active, emitter, hub))
}
fn waiting(
    emitter: &super::super::super::session_status::SessionStatusEmitter,
) -> SessionStatusInfo {
    emitter
        .waiting_info(
            &grapt_types::ProviderId::new("fixture").unwrap_or_else(|| panic!("fixture provider")),
            RetryNotice {
                attempt: 1,
                after: Duration::from_secs(7),
                class: Some(grapt_types::ProviderFailureClass::RateLimit),
                preparation_stage: None,
                capacity_wait: None,
            },
        )
        .unwrap_or_else(|| panic!("fixture retry deadline"))
}
#[test]
fn hydration_retains_current_retry_but_settle_and_successor_refuse_old_notices() -> Result {
    let (active, emitter, hub) = setup()?;
    let sub = hub.subscribe_host()?;
    let guard = active.register(session(), execution("exe_first"), 4)?;
    active.retrying(&session(), &execution("exe_first"), waiting(&emitter));
    let reopened = hub.subscribe_host()?;
    let rows = drain(&reopened)?;
    assert!(rows.iter().any(|row| row.contains("\"type\":\"retry\"")));
    active.retrying(
        &session(),
        &execution("exe_first"),
        SessionStatusInfo::busy(),
    );
    drop(guard);
    let next = active.register(session(), execution("exe_second"), 4)?;
    active.retrying(&session(), &execution("exe_first"), waiting(&emitter));
    let rows = drain(&sub)?;
    assert_eq!(rows.len(), 4);
    assert!(rows[1].contains("rate_limited"));
    assert!(!rows[1].contains("exhaust"));
    assert!(rows
        .last()
        .is_some_and(|row| row.contains("\"type\":\"idle\"")));
    let mut frames = Vec::new();
    active.offer_status_hydration(&emitter, &mut frames);
    assert!(frames.iter().all(|event| !serde_json::to_string(event)
        .unwrap_or_else(|error| panic!("{error}"))
        .contains("\"type\":\"retry\"")));
    drop(next);
    Ok(())
}
#[test]
fn racing_retry_and_settle_never_publish_retry_after_idle() -> Result {
    for _ in 0..24 {
        let (active, emitter, hub) = setup()?;
        let sub = hub.subscribe_host()?;
        let guard = active.register(session(), execution("exe_race"), 4)?;
        emitter.busy(&session());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let (done, rx) = std::sync::mpsc::sync_channel(2);
        let (a, b, done_one, status) = (
            Arc::clone(&active),
            Arc::clone(&barrier),
            done.clone(),
            waiting(&emitter),
        );
        let one = std::thread::spawn(move || {
            b.wait();
            a.retrying(&session(), &execution("exe_race"), status);
            done_one.send(()).unwrap_or_else(|error| panic!("{error}"));
        });
        let (a, b) = (Arc::clone(&active), Arc::clone(&barrier));
        let two = std::thread::spawn(move || {
            b.wait();
            a.settle(&session(), &execution("exe_race"));
            done.send(()).unwrap_or_else(|error| panic!("{error}"));
        });
        rx.recv_timeout(Duration::from_secs(5))?;
        rx.recv_timeout(Duration::from_secs(5))?;
        one.join().unwrap_or_else(|_| panic!("retry thread"));
        two.join().unwrap_or_else(|_| panic!("settle thread"));
        let rows = drain(&sub)?;
        assert!(rows
            .last()
            .is_some_and(|row| row.contains("\"type\":\"idle\"")));
        drop(guard);
    }
    Ok(())
}

#[test]
fn compaction_phase_hydrates_and_clears_on_success_failure_and_cancellation() -> Result {
    use std::{
        future::Future,
        task::{Context, Poll},
    };
    for outcome in ["success", "failure", "cancellation"] {
        let (active, _emitter, hub) = setup()?;
        let owner = active.register(session(), execution("exe_compacting"), 4)?;
        let feedback = crate::dogfood::session_status::RetryFeedback::new(
            Arc::clone(&active),
            session(),
            execution("exe_compacting"),
            grapt_types::ProviderId::new("fixture").ok_or("provider")?,
        );
        let sub = hub.subscribe_host()?;
        let mut began = false;
        let mut future = Box::pin(async {
            let _phase = feedback.compacting();
            std::future::poll_fn(|_| {
                if !began {
                    began = true;
                    Poll::Pending
                } else if outcome == "cancellation" {
                    Poll::Pending
                } else {
                    Poll::Ready(if outcome == "failure" {
                        Err(())
                    } else {
                        Ok(())
                    })
                }
            })
            .await
        });
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending));
        let hydrate = hub.subscribe_host()?;
        assert!(drain(&hydrate)?
            .iter()
            .any(|row| row.contains("\"phase\":\"compacting\"")));
        if outcome != "cancellation" {
            assert!(matches!(future.as_mut().poll(&mut cx), Poll::Ready(_)));
        }
        drop(future);
        let frames = drain(&sub)?;
        let last = frames.last().ok_or("phase clear missing")?;
        assert!(last.contains("\"type\":\"busy\"") && !last.contains("\"phase\""));
        let old_phase = feedback.compacting();
        drop(owner);
        let successor = active.register(session(), execution("exe_compact_successor"), 4)?;
        let feedback = crate::dogfood::session_status::RetryFeedback::new(
            Arc::clone(&active),
            session(),
            execution("exe_compact_successor"),
            grapt_types::ProviderId::new("fixture").ok_or("provider")?,
        );
        let phase = feedback.compacting();
        drop(old_phase);
        let hydrate = hub.subscribe_host()?;
        assert!(
            drain(&hydrate)?
                .iter()
                .any(|row| row.contains("\"phase\":\"compacting\"")),
            "an old owner cannot clear its successor's phase"
        );
        drop(phase);
        drop(successor);
    }
    Ok(())
}

#[test]
fn an_active_retry_loop_remains_visible_in_projected_active_sessions() -> Result {
    let (active, emitter, _hub) = setup()?;
    let session = session();
    let execution = execution("exe_retrying");
    let _guard = active.register(session.clone(), execution.clone(), 4)?;
    active.retrying(&session, &execution, waiting(&emitter));
    assert_eq!(
        active.projected(Vec::new()),
        vec![(session.clone(), execution.clone())],
        "active retry loops must remain visible in /api/session/active"
    );
    active.settle(&session, &execution);
    assert!(active.projected(Vec::new()).is_empty());
    Ok(())
}
