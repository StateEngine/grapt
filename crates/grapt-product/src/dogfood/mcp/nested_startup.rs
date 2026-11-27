//! Startup observations belong to a VM's active nested calls, not a location.
use grapt_mcp_client::{HttpError, HumanWait};
use std::future::Future;
use std::sync::Mutex;
use tokio::sync::watch;

tokio::task_local! {
    static NESTED_STARTUP: watch::Sender<Option<HumanWait>>;
}

type StartupObserver = watch::Receiver<Option<HumanWait>>;

/// Only the worker's bounded nested-call set can register observers. These
/// retain clocks, never startup reservation leases or a registry handle.
#[derive(Default)]
pub(in crate::dogfood) struct NestedStartups {
    calls: Mutex<Vec<StartupObserver>>,
}

impl NestedStartups {
    pub(in crate::dogfood) fn observe<T>(
        &self,
        operation: impl Future<Output = T>,
    ) -> Result<impl Future<Output = T>, HttpError> {
        let mut calls = self.calls.lock().map_err(|_| HttpError::State)?;
        calls.retain(|call| call.has_changed().is_ok());
        if calls.len() >= grapt_codemode::IN_FLIGHT {
            return Err(HttpError::RuntimeOrBudget);
        }
        let (owner, observer) = watch::channel(None);
        calls.push(observer);
        Ok(NESTED_STARTUP.scope(owner, operation))
    }

    pub(in crate::dogfood) fn is_waiting(&self) -> Result<bool, HttpError> {
        let calls = self.calls.lock().map_err(|_| HttpError::State)?;
        Ok(calls.iter().any(|call| {
            call.has_changed().is_ok() && call.borrow().as_ref().is_some_and(HumanWait::is_waiting)
        }))
    }
}

/// Called only after recovery selected the exact server's reservation. A
/// task-local scope crosses inline nested execution, but is not inherited by
/// independent catalog/startup tasks or another VM's nested calls.
pub(super) fn join(clock: &HumanWait) -> JoinedStartup {
    JoinedStartup(
        NESTED_STARTUP
            .try_with(|owner| {
                let _previous = owner.send_replace(Some(clock.clone()));
                owner.clone()
            })
            .ok(),
    )
}

pub(super) struct JoinedStartup(Option<watch::Sender<Option<HumanWait>>>);

impl Drop for JoinedStartup {
    fn drop(&mut self) {
        if let Some(owner) = &self.0 {
            let _previous = owner.send_replace(None);
        }
    }
}
