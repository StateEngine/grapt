use super::*;
use crate::residency::Residency;
use std::future::Future;
use std::task::{Context, Poll, Waker};
use tokio::sync::oneshot;

#[derive(Clone, Copy)]
enum End {
    Idle,
    Error,
    Interrupted,
    Held,
}

struct App {
    end: End,
    entered: Semaphore,
    release: Semaphore,
    residency: Mutex<Option<oneshot::Sender<Residency>>>,
}

impl App {
    fn new(end: End) -> Self {
        Self {
            end,
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            residency: Mutex::new(None),
        }
    }
}

impl SessionApplication for App {
    type Drain = bool;

    fn session_exists(&self, _: &SessionId) -> Result<bool, CoordinatorError> {
        Ok(true)
    }

    fn begin_drain(&self, _: &SessionId) -> Result<bool, CoordinatorError> {
        Ok(matches!(self.end, End::Interrupted | End::Held))
    }

    fn continuation_owed(&self, drain: &bool) -> bool {
        *drain
    }

    fn next_eligible(
        &self,
        _: &mut bool,
        _: Boundary,
    ) -> Result<Option<Promotion>, CoordinatorError> {
        if matches!(self.end, End::Error) {
            return Err(CoordinatorError::Inbox {
                session: "ses_retirement".into(),
                reason: "fixture failure".into(),
            });
        }
        Ok(None)
    }

    fn advance<'a>(
        &'a self,
        drain: &'a mut bool,
        cancellation: Cancellation,
    ) -> BoxFuture<'a, Result<RunOutcome, CoordinatorError>> {
        Box::pin(async move {
            let sender = match self.residency.lock() {
                Ok(mut sender) => sender.take(),
                Err(_) => panic!("fixture residency lock poisoned"),
            };
            if let Some(sender) = sender {
                assert!(sender.send(cancellation.residency()).is_ok());
            }
            self.entered.add_permits(1);
            if matches!(self.end, End::Held) {
                match self.release.acquire().await {
                    Ok(permit) => permit.forget(),
                    Err(_) => panic!("fixture gate closed"),
                }
            }
            drop(cancellation);
            *drain = false;
            Ok(if matches!(self.end, End::Interrupted) {
                RunOutcome::Interrupted
            } else {
                RunOutcome::Idle
            })
        })
    }
}

fn retained(coordinator: &SessionCoordinator<App>) -> usize {
    match coordinator.slots.lock() {
        Ok(slots) => slots.len(),
        Err(_) => panic!("coordinator lock poisoned"),
    }
}

fn session(index: usize) -> SessionId {
    match format!("ses_retirement_{index}").parse() {
        Ok(session) => session,
        Err(error) => panic!("valid fixture session: {error}"),
    }
}

async fn within<F: Future>(future: F) -> F::Output {
    match tokio::time::timeout(std::time::Duration::from_secs(5), future).await {
        Ok(result) => result,
        Err(_) => panic!("coordinator fixture did not reach its barrier"),
    }
}

#[tokio::test]
async fn completed_sessions_do_not_accumulate_slots() {
    for end in [End::Idle, End::Error, End::Interrupted] {
        let coordinator = SessionCoordinator::new(Arc::new(App::new(end)));
        for index in 0..2048 {
            let result = coordinator.wake(session(index)).await;
            assert_eq!(result.is_err(), matches!(end, End::Error));
            assert_eq!(retained(&coordinator), 0, "retained Session {index}");
        }
    }
}

#[tokio::test]
async fn a_joiner_keeps_the_terminal_result_after_slot_retirement() {
    let app = Arc::new(App::new(End::Held));
    let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&app)));
    let owner = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move { coordinator.wake(session(0)).await })
    };
    let permit = match within(app.entered.acquire()).await {
        Ok(permit) => permit,
        Err(_) => panic!("fixture gate closed"),
    };
    permit.forget();
    let mut joiner = Box::pin(coordinator.wake(session(0)));
    assert!(matches!(
        joiner
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    app.release.add_permits(1);
    assert!(matches!(within(owner).await, Ok(Ok(_))));
    assert_eq!(retained(&coordinator), 0);
    assert_eq!(within(joiner).await, Ok(WakeOutcome::Joined));
}

#[tokio::test]
async fn abandonment_keeps_resident_work_but_retires_after_its_last_token() {
    let (sender, receiver) = oneshot::channel();
    let mut app = App::new(End::Held);
    app.residency = Mutex::new(Some(sender));
    let coordinator = Arc::new(SessionCoordinator::new(Arc::new(app)));
    let owner = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move { coordinator.wake(session(0)).await })
    };
    let token = match within(receiver).await {
        Ok(token) => token,
        Err(_) => panic!("owner never handed out its residency"),
    };
    let (release, held) = oneshot::channel();
    let resident = tokio::spawn(async move {
        let _token = token;
        let _ = held.await;
    });
    let mut joiner = Box::pin(coordinator.wake(session(0)));
    assert!(matches!(
        joiner
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    owner.abort();
    assert!(within(owner).await.is_err());
    assert!(matches!(
        within(joiner).await,
        Err(CoordinatorError::OwnerAbandoned { .. })
    ));
    assert_eq!(retained(&coordinator), 1);
    assert!(coordinator.active_sessions().is_empty());
    assert_eq!(retained(&coordinator), 1, "resident work was discarded");
    assert!(release.send(()).is_ok());
    assert!(within(resident).await.is_ok());
    assert!(coordinator.active_sessions().is_empty());
    assert_eq!(retained(&coordinator), 0);
}
