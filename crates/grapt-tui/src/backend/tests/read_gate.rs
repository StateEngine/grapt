//! A bounded, cancellable hold of one periodic or command HTTP answer.
use grapt_client::Operation;
use std::sync::{Condvar, Mutex};
use std::time::Duration;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Default)]
struct State {
    held: Option<Operation>,
    blocked: bool,
    allow_expired: bool,
    calls: [usize; 13],
}
#[derive(Default)]
pub(super) struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}
fn index(operation: Operation) -> Option<usize> {
    match operation {
        Operation::PermissionRequestList => Some(0),
        Operation::SessionList => Some(1),
        Operation::SessionFormList => Some(2),
        Operation::FormRequestList => Some(3),
        Operation::SessionPermissionReply => Some(4),
        Operation::SessionGet => Some(5),
        Operation::SessionInterrupt => Some(6),
        Operation::MessageList => Some(7),
        Operation::SessionAgentSelect => Some(8),
        Operation::SessionModelSelect => Some(9),
        Operation::SessionRename => Some(10),
        Operation::SessionFormState => Some(11),
        Operation::SandboxGrantRevoke => Some(12),
        _ => None,
    }
}
impl Gate {
    pub fn hold(&self, operation: Operation) -> Result {
        let mut state = self.state.lock().map_err(|_| "gate lock")?;
        assert!(state.held.is_none());
        state.held = Some(operation);
        state.blocked = false;
        Ok(())
    }
    pub fn permit_timed_out_answer(&self) -> Result {
        self.state.lock().map_err(|_| "gate lock")?.allow_expired = true;
        Ok(())
    }
    pub fn blocked(&self) -> bool {
        match self.state.lock() {
            Ok(state) => state.blocked,
            Err(error) => panic!("gate lock poisoned: {error}"),
        }
    }
    pub fn release(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(error) => panic!("gate lock poisoned: {error}"),
        };
        state.held = None;
        state.blocked = false;
        self.changed.notify_all();
    }
    pub fn calls(&self, operation: Operation) -> Result<usize> {
        let index = index(operation).ok_or("periodic operation")?;
        Ok(self.state.lock().map_err(|_| "gate lock")?.calls[index])
    }
    pub fn answer(&self, operation: Operation) -> Result<bool> {
        let began = tokio::time::Instant::now();
        let Some(index) = index(operation) else {
            return Ok(false);
        };
        let mut state = self.state.lock().map_err(|_| "gate lock")?;
        state.calls[index] += 1;
        if state.held != Some(operation) {
            return Ok(false);
        }
        state.blocked = true;
        self.changed.notify_all();
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(10), |state| {
                state.held == Some(operation)
            })
            .map_err(|_| "gate wait")?;
        if timeout.timed_out() && state.held == Some(operation) {
            return Err("HTTP answer hold exceeded its bound".into());
        }
        Ok(state.allow_expired && began.elapsed() >= Duration::from_secs(5))
    }
}
