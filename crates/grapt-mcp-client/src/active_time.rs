//! Active transport deadlines that exclude an explicitly owned human wait.
//!
//! Modified by Grapt: one bounded pause owner, shared elapsed time across
//! HTTP headers, body reads and recovery, and checked deadline arithmetic.

use crate::HttpError;
use std::future::Future;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

#[derive(Debug, Clone, Copy)]
struct State {
    paused_at: Option<Instant>,
    elapsed_pause: Duration,
}

/// Shared by serial MCP human waits and the transport/admission deadlines
/// enclosing them. Observers do not acquire the channel or its startup lease.
#[derive(Debug, Clone)]
pub struct HumanWait {
    epoch: Instant,
    state: watch::Sender<State>,
}

impl Default for HumanWait {
    fn default() -> Self {
        let (state, _) = watch::channel(State {
            paused_at: None,
            elapsed_pause: Duration::ZERO,
        });
        Self {
            epoch: Instant::now(),
            state,
        }
    }
}

impl HumanWait {
    /// The channel enters this scope only while handling an admitted form.
    /// Overlapping waits refuse rather than silently unpausing another owner.
    pub fn enter(&self) -> Result<HumanWaitGuard, HttpError> {
        let mut entered = false;
        self.state.send_if_modified(|state| {
            if state.paused_at.is_some() {
                return false;
            }
            state.paused_at = Some(Instant::now());
            entered = true;
            true
        });
        if !entered {
            return Err(HttpError::State);
        }
        Ok(HumanWaitGuard(self.clone()))
    }

    /// Elapsed time since this clock's creation, excluding every human scope.
    pub fn active_elapsed(&self) -> Result<Duration, HttpError> {
        let state = self.state.borrow();
        let now = state.paused_at.unwrap_or_else(Instant::now);
        now.checked_duration_since(self.epoch)
            .and_then(|elapsed| elapsed.checked_sub(state.elapsed_pause))
            .ok_or(HttpError::RuntimeOrBudget)
    }

    /// Includes an in-progress scope so enclosing owners can exclude the same
    /// human time without restarting either their active or human allowance.
    pub fn paused_elapsed(&self) -> Duration {
        let state = self.state.borrow();
        state.elapsed_pause.saturating_add(
            state
                .paused_at
                .map_or(Duration::ZERO, |started| started.elapsed()),
        )
    }

    /// Whether an admitted human scope is currently held by the channel.
    pub fn is_waiting(&self) -> bool {
        self.state.borrow().paused_at.is_some()
    }

    /// Spend one nonrenewable active allowance while observing the same pauses.
    pub fn deadline(&self, budget: Duration) -> Result<Deadline, HttpError> {
        if budget.is_zero() {
            return Err(HttpError::TimedOut);
        }
        Ok(Deadline {
            clock: self.clone(),
            expires: self
                .active_elapsed()?
                .checked_add(budget)
                .ok_or(HttpError::RuntimeOrBudget)?,
        })
    }
}

#[derive(Debug)]
pub struct HumanWaitGuard(HumanWait);

impl Drop for HumanWaitGuard {
    fn drop(&mut self) {
        self.0.state.send_modify(|state| {
            if let Some(start) = state.paused_at.take() {
                // Saturation becomes a checked clock failure, never extra time.
                state.elapsed_pause = state.elapsed_pause.saturating_add(start.elapsed());
            }
        });
    }
}

/// An active deadline; the human-wait owner must separately bound human time.
#[derive(Debug, Clone)]
pub struct Deadline {
    clock: HumanWait,
    expires: Duration,
}

impl Deadline {
    /// Remaining active time, or a refusal after expiry.
    pub fn remaining(&self) -> Result<Duration, HttpError> {
        self.expires
            .checked_sub(self.clock.active_elapsed()?)
            .filter(|remaining| !remaining.is_zero())
            .ok_or(HttpError::TimedOut)
    }

    /// Keep polling I/O and cancellation during a human pause without charging
    /// that pause to this deadline or renewing previously spent active time.
    pub async fn timeout<T>(&self, operation: impl Future<Output = T>) -> Result<T, HttpError> {
        let mut changes = self.clock.state.subscribe();
        tokio::pin!(operation);
        loop {
            let paused = changes.borrow_and_update().paused_at.is_some();
            let remaining = self.remaining()?;
            if paused {
                tokio::select! {
                    result = &mut operation => return Ok(result),
                    changed = changes.changed() => changed.map_err(|_| HttpError::State)?,
                }
            } else {
                tokio::select! {
                    biased;
                    changed = changes.changed() => changed.map_err(|_| HttpError::State)?,
                    result = &mut operation => return Ok(result),
                    _ = tokio::time::sleep(remaining) => {},
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn human_wait_preserves_remaining_active_time_and_drop_unpauses(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let clock = HumanWait::default();
        let deadline = clock.deadline(Duration::from_millis(100))?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        let guard = clock.enter()?;
        assert!(clock.enter().is_err());
        let before = deadline.remaining()?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(deadline.remaining()?, before);
        drop(guard);
        // A delayed runtime may observe two ready timers at once. Keep the
        // operation pending so this checks expiry, not select branch priority.
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            deadline.timeout(std::future::pending::<()>()),
        )
        .await?;
        assert!(matches!(result, Err(HttpError::TimedOut)));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn paused_operation_still_observes_disconnect_and_external_cancellation(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let clock = HumanWait::default();
        let deadline = clock.deadline(Duration::from_millis(10))?;
        let guard = clock.enter()?;
        let result = deadline
            .timeout(async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Err::<(), _>("disconnected")
            })
            .await?;
        assert_eq!(result, Err("disconnected"));
        assert!(tokio::time::timeout(
            Duration::from_millis(30),
            deadline.timeout(std::future::pending::<()>())
        )
        .await
        .is_err());
        drop(guard);
        assert!(deadline
            .timeout(std::future::pending::<()>())
            .await
            .is_err());
        Ok(())
    }
}
