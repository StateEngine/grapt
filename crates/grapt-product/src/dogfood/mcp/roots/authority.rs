//! Approval lifetime covers startup, retained scopes, and snapshot publication.
use super::{Config, Declaration, Prepared, State};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Revision(u64);

#[derive(Debug)]
struct Lease {
    location: String,
    workspace: std::path::PathBuf,
    server: String,
    declaration: Declaration,
    state: Weak<State>,
}

#[derive(Debug)]
pub(in crate::dogfood::mcp) struct Authority {
    revision: Option<u64>,
    editing: usize,
    leases: Vec<Lease>,
}

impl Default for Authority {
    fn default() -> Self {
        Self {
            revision: Some(0),
            editing: 0,
            leases: Vec::new(),
        }
    }
}

impl Authority {
    pub(crate) fn revision(&self) -> Result<Revision, String> {
        self.revision
            .filter(|_| self.editing == 0)
            .map(Revision)
            .ok_or_else(|| "MCP configuration is changing; retry roots discovery".into())
    }

    pub(crate) fn validate(&self, revision: Revision) -> Result<(), String> {
        if self.revision()? != revision {
            return Err("MCP configuration changed while reading roots; retry".into());
        }
        Ok(())
    }

    fn advance(&mut self) {
        self.revision = self.revision.and_then(|value| value.checked_add(1));
    }

    pub(crate) fn edit(
        authority: &Arc<Mutex<Self>>,
        workspace: Option<&str>,
        server: &str,
    ) -> Result<EditGuard, String> {
        let mut held = authority
            .lock()
            .map_err(|_| "MCP roots custody unavailable")?;
        held.editing = held
            .editing
            .checked_add(1)
            .ok_or("MCP configuration edit capacity reached")?;
        held.advance();
        held.leases.retain(|lease| {
            let Some(state) = lease.state.upgrade() else {
                return false;
            };
            if lease.server == server
                && workspace.is_none_or(|path| lease.workspace == std::path::Path::new(path))
            {
                state.revoke();
            }
            true
        });
        Ok(EditGuard(Arc::clone(authority)))
    }

    pub(crate) fn configure(
        &mut self,
        location: &str,
        roots: &Config,
        declared: &BTreeMap<String, Declaration>,
        runtime: &BTreeMap<String, Declaration>,
    ) -> Result<(), String> {
        for lease in &self.leases {
            if lease.location == location {
                if let Some(state) = lease.state.upgrade() {
                    let current = runtime
                        .get(&lease.server)
                        .or_else(|| declared.get(&lease.server));
                    state.set((current == Some(&lease.declaration)).then(|| roots.clone()))?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn connect(
        &mut self,
        location: &str,
        server: &str,
        declaration: &Declaration,
        prepared: Option<&Prepared>,
    ) -> Arc<State> {
        self.leases.retain(|lease| lease.state.strong_count() != 0);
        // Bound temporary pre-admission leases as well as held connections.
        // The extra slot supports the duplex adapter, which acquires its
        // transport slot only after startup.
        if self.leases.len() > super::super::MAX_LIVE_CONNECTIONS {
            return State::new(None);
        }
        let config = prepared
            .filter(|prepared| {
                self.validate(prepared.revision).is_ok()
                    && prepared.permitted.get(server) == Some(declaration)
            })
            .map(|prepared| prepared.config.clone());
        let state = State::new(config);
        if let Some(prepared) = prepared {
            self.leases.push(Lease {
                location: location.into(),
                workspace: prepared.config.location.clone(),
                server: server.into(),
                declaration: declaration.clone(),
                state: Arc::downgrade(&state),
            });
        }
        state
    }
}

/// Owned by the actual configuration writer: cancellation of its HTTP waiter
/// cannot publish a new revision before the blocking write has finished.
pub(crate) struct EditGuard(Arc<Mutex<Authority>>);
impl Drop for EditGuard {
    fn drop(&mut self) {
        if let Ok(mut held) = self.0.lock() {
            held.editing = held.editing.saturating_sub(1);
            held.advance();
        }
    }
}
