//! Record process completion before releasing its ephemeral handle.
//! Copyright 2025 OpenAI. Apache-2.0; Modified by Grapt:
//! session-owned durable facts and bounded native outcomes.
use super::*;
use grapt_event::session_terminal::{TerminalFact, TerminalState};
use grapt_types::DeterministicAuthorities;

pub(super) struct History {
    inbox: Arc<Mutex<Inbox>>,
    ids: Arc<OsRandomIds>,
    clock: Arc<SystemClock>,
}
impl History {
    #[cfg(windows)]
    pub fn observed_ms(&self) -> Option<u64> {
        u64::try_from(self.clock.now().as_millis()).ok()
    }

    pub fn new(inbox: Arc<Mutex<Inbox>>, ids: Arc<OsRandomIds>, clock: Arc<SystemClock>) -> Self {
        Self { inbox, ids, clock }
    }
    pub fn record(
        &self,
        session: &SessionId,
        operation: &OperationId,
        state: TerminalState,
        cleanup_error: Option<String>,
    ) -> Result<(), String> {
        let fact = TerminalFact::new(session.clone(), operation.clone(), state, cleanup_error)
            .map_err(|error| error.to_string())?;
        let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
        grapt_session::terminal::record(lock(&self.inbox).store_mut(), &authorities, &fact)
    }
    pub fn missing(
        &self,
        session: &SessionId,
        operation: &OperationId,
    ) -> Result<Option<String>, String> {
        let fact = grapt_session::terminal::latest(lock(&self.inbox).store(), session, operation)?;
        let Some(fact) = fact else {
            return Ok(None);
        };
        let state = if fact.state() == &TerminalState::Starting {
            let state = TerminalState::Lost { reason: "the owning process registry no longer retains this command; no process exit was recorded".into() };
            self.record(session, operation, state.clone(), None)?;
            state
        } else {
            fact.state().clone()
        };
        let reason = match state {
            TerminalState::Starting => return Err("terminal recovery did not settle its lost owner".into()),
            TerminalState::Exited { code } => format!("terminal exited with status {code}"),
            TerminalState::TimedOut => "terminal timed out".into(),
            TerminalState::Cancelled => "terminal was cancelled".into(),
            TerminalState::NotStarted { reason } => format!("terminal command did not start: {reason}"),
            TerminalState::Lost { reason } => format!("terminal handle was lost: {reason}; process status is unknown; do not replay the command"),
        };
        let cleanup = fact
            .cleanup_error()
            .map(|e| format!("; cleanup was unconfirmed: {e}"))
            .unwrap_or_default();
        Ok(Some(format!("{reason}{cleanup}; its live output handle is no longer retained; no input was delivered. Use history({{}}) for previously captured output.")))
    }
}

impl Entry {
    pub(super) fn persist_end(&self) {
        let Some(history) = &self.history else {
            return;
        };
        let _ending = lock(&self.ending);
        let (state, cleanup) = {
            let data = lock(&self.observation);
            if data.end_recorded || !data.supervised {
                return;
            }
            let Some(exit) = &data.exit else {
                return;
            };
            let state = match exit {
                ShellTerminal::Exited(code) => TerminalState::Exited { code: *code },
                ShellTerminal::TimedOut => TerminalState::TimedOut,
                ShellTerminal::Cancelled => TerminalState::Cancelled,
                ShellTerminal::Failed(reason) if data.did_not_start => TerminalState::NotStarted {
                    reason: reason.clone(),
                },
                ShellTerminal::Failed(reason) => TerminalState::Lost {
                    reason: reason.clone(),
                },
            };
            (state, data.cleanup_error.clone())
        };
        let result = history.record(&self.session, &self.operation, state, cleanup);
        let mut data = lock(&self.observation);
        match result {
            Ok(()) => {
                data.end_recorded = true;
                data.persistence_error = None;
            }
            Err(error) => {
                data.persistence_error = Some(format!(
                    "terminal end could not be saved; its handle is retained: {error}"
                ))
            }
        }
    }
}

impl TerminalRegistry {
    pub(super) fn persist_completed(&self) {
        // Snapshot bounded owners before taking the Inbox lock. Failed starts
        // have no returned output handle that could otherwise retry this save.
        let entries: Vec<_> = {
            let entries = lock(&self.entries);
            entries
                .by_id
                .values()
                .chain(entries.quarantine.iter())
                .cloned()
                .collect()
        };
        for entry in entries {
            entry.persist_end();
        }
    }
}
