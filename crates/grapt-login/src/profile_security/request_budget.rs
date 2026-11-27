//! One finite polling budget shared by helper admission and pipe completion.
use std::{
    io,
    sync::{Mutex, MutexGuard, TryLockError},
    time::Duration,
};

pub(super) struct RequestBudget {
    remaining: u16,
}

impl RequestBudget {
    pub(super) fn new() -> Self {
        Self { remaining: 3000 }
    }

    pub(super) fn lock<'a, T>(
        &mut self,
        mutex: &'a Mutex<T>,
    ) -> Result<MutexGuard<'a, T>, TryLockError<MutexGuard<'a, T>>> {
        loop {
            match mutex.try_lock() {
                Err(TryLockError::WouldBlock) if self.pause().is_ok() => {}
                result => return result,
            }
        }
    }

    pub(super) fn check(&self) -> io::Result<()> {
        if self.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "profile protection timed out",
            ));
        }
        Ok(())
    }

    pub(super) fn pause(&mut self) -> io::Result<()> {
        self.remaining = self.remaining.checked_sub(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "profile protection timed out")
        })?;
        std::thread::sleep(Duration::from_millis(5));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_contending_caller_cannot_restart_the_budget_after_admission() -> io::Result<()> {
        let mutex = Mutex::new(());
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let (release, finish) = std::sync::mpsc::sync_channel(1);
        let mutex = &mutex;
        std::thread::scope(|scope| -> io::Result<()> {
            let holder = scope.spawn(move || -> io::Result<()> {
                let _guard = mutex
                    .try_lock()
                    .map_err(|_| io::Error::other("fixture lock was already held"))?;
                ready.send(()).map_err(io::Error::other)?;
                finish
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(io::Error::other)?;
                Ok(())
            });
            started
                .recv_timeout(Duration::from_secs(5))
                .map_err(io::Error::other)?;
            let mut budget = RequestBudget::new();
            budget.remaining = 1;
            assert!(matches!(budget.lock(mutex), Err(TryLockError::WouldBlock)));
            release.send(()).map_err(io::Error::other)?;
            holder
                .join()
                .map_err(|_| io::Error::other("fixture lock holder failed"))??;
            let _guard = budget
                .lock(mutex)
                .map_err(|_| io::Error::other("fixture lock was not released"))?;
            assert!(budget.check().is_err());
            assert_eq!(
                budget.pause().err().map(|error| error.kind()),
                Some(io::ErrorKind::TimedOut)
            );
            Ok(())
        })
    }
}
