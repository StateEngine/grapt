//! Process-owned resource reuse; permission decisions never survive a request.
use std::{
    io,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex, OnceLock, TryLockError,
    },
};

use super::{super::request_budget::RequestBudget, worker::Worker};

struct State {
    worker: Option<Worker>,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static OWNERS: AtomicU32 = AtomicU32::new(0);

fn access<T>(
    operation: impl FnOnce(&mut State, &mut RequestBudget) -> io::Result<T>,
) -> io::Result<T> {
    let state = STATE.get_or_init(|| Mutex::new(State { worker: None }));
    let mut budget = RequestBudget::new();
    let mut state = budget.lock(state).map_err(|error| match error {
        TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::TimedOut,
            "profile protection admission timed out",
        ),
        TryLockError::Poisoned(poisoned) => {
            // A failed owner must not leave a usable helper behind.
            poisoned.into_inner().worker.take();
            io::Error::other("profile protection helper owner failed")
        }
    })?;
    let result = operation(&mut state, &mut budget);
    // The last scope can end while this operation owns the helper. Its atomic
    // release prevents a waiter from stranding ownership on admission timeout.
    if OWNERS.load(Ordering::Acquire) == 0 {
        state.worker.take();
    }
    result
}

pub(in crate::profile_security) fn begin_batch() -> io::Result<()> {
    OWNERS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |owners| {
            owners.checked_add(1).filter(|next| *next <= 1024)
        })
        .map_err(|_| io::Error::other("profile protection owner count exceeds its bound"))?;
    Ok(())
}

pub(in crate::profile_security) fn end_batch() -> io::Result<()> {
    let previous = OWNERS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |owners| {
            owners.checked_sub(1)
        })
        .map_err(|_| io::Error::other("profile protection scope is absent"))?;
    if previous == 1 {
        access(|_, _| Ok(()))?;
    }
    Ok(())
}

pub(super) fn run(operation: &str, bytes: &[u8]) -> io::Result<Vec<u8>> {
    access(|state, budget| {
        budget.check()?;
        if OWNERS.load(Ordering::Acquire) == 0 {
            state.worker.take();
            return Worker::spawn()?.run(operation, bytes, budget);
        }
        if state.worker.as_ref().is_some_and(Worker::exhausted) {
            state.worker.take();
        }
        if state.worker.is_none() {
            state.worker = Some(Worker::spawn()?);
        }
        let result = state
            .worker
            .as_mut()
            .ok_or_else(|| io::Error::other("profile protection helper is absent"))?
            .run(operation, bytes, budget);
        if result.is_err() {
            state.worker.take();
        }
        result
    })
}
