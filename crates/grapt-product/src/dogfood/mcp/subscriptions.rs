// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// (on_resource_updated), bb64b894fd67158be0f874872e6d7bfe717e7368.
// Modified by Grapt: bounded Session interests, coalesced dirty state, and
// explicit re-reads through the existing held connection; never model input.
use super::McpRegistry;
use grapt_types::SessionId;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

const MAX_RESOURCES: usize = 32;
const MAX_INTERESTS: usize = 256;

#[derive(Debug, Default)]
struct Resource {
    wire: bool,
    revision: u64,
    owners: BTreeMap<SessionId, Owner>,
}
#[derive(Debug)]
struct Owner {
    token: OwnerToken,
    baseline: u64,
    pending: usize,
    retained: bool,
}
#[derive(Clone, Debug, Default)]
struct OwnerToken(Arc<()>);
impl OwnerToken {
    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
#[derive(Debug, Default)]
struct Book {
    closed: bool,
    resources: BTreeMap<String, Resource>,
    hint: Option<(std::sync::Weak<super::notifications::Emitter>, String)>,
}
#[derive(Debug, Default)]
pub(in crate::dogfood) struct State(Mutex<Book>);
impl State {
    fn lock(&self) -> std::sync::MutexGuard<'_, Book> {
        self.0.lock().unwrap_or_else(|error| error.into_inner())
    }
    pub(super) fn hint(&self, emitter: &Arc<super::notifications::Emitter>, key: &str) {
        self.lock().hint = Some((Arc::downgrade(emitter), key.into()));
    }
    /// A cancelled acquisition rolls back only the interest it introduced.
    pub(super) fn reserve(
        self: &Arc<Self>,
        session: &SessionId,
        uri: &str,
    ) -> Result<Interest, String> {
        let mut book = self.lock();
        if book.closed {
            return Err("MCP connection ended; reconnect and retry".into());
        }
        let existing = book
            .resources
            .get(uri)
            .is_some_and(|row| row.owners.contains_key(session));
        if !existing
            && book
                .resources
                .values()
                .map(|row| row.owners.len())
                .sum::<usize>()
                >= MAX_INTERESTS
        {
            return Err("MCP resource subscription owner limit reached".into());
        }
        if !book.resources.contains_key(uri) && book.resources.len() >= MAX_RESOURCES {
            return Err(
                "MCP resource subscription limit of 32 reached; unpin a resource first".into(),
            );
        }
        let row = book.resources.entry(uri.to_owned()).or_default();
        let owner = row.owners.entry(session.clone()).or_insert(Owner {
            token: OwnerToken::default(),
            baseline: row.revision,
            pending: 0,
            retained: false,
        });
        if owner.pending >= super::MAX_QUEUED_CALLERS {
            return Err("MCP resource acquisition limit reached".into());
        }
        owner.pending += 1;
        Ok(Interest {
            state: self.clone(),
            session: session.clone(),
            uri: uri.into(),
            pending: true,
            token: owner.token.clone(),
        })
    }
    pub(super) fn subscribed(&self, uri: &str) -> bool {
        self.lock().resources.get(uri).is_some_and(|row| row.wire)
    }
    pub(super) fn confirm(&self, uri: &str) {
        if let Some(row) = self.lock().resources.get_mut(uri) {
            row.wire = true;
        }
    }
    pub(super) fn revision(&self, uri: &str) -> u64 {
        self.lock().resources.get(uri).map_or(0, |row| row.revision)
    }
    fn read(&self, session: &SessionId, uri: &str, token: &OwnerToken, revision: u64) {
        if let Some(owner) = self
            .lock()
            .resources
            .get_mut(uri)
            .and_then(|row| row.owners.get_mut(session))
            .filter(|owner| owner.token.matches(token))
        {
            owner.baseline = if revision == u64::MAX { 0 } else { revision };
        }
    }
    fn settle(&self, session: &SessionId, uri: &str, token: &OwnerToken, keep: bool) -> bool {
        let mut book = self.lock();
        let mut present = false;
        if let Some(row) = book.resources.get_mut(uri) {
            if let Some(owner) = row
                .owners
                .get_mut(session)
                .filter(|owner| owner.token.matches(token))
            {
                present = true;
                owner.pending = owner.pending.saturating_sub(1);
                owner.retained |= keep;
                if !owner.retained && owner.pending == 0 {
                    row.owners.remove(session);
                }
            }
        }
        book.resources
            .retain(|_, row| row.wire || !row.owners.is_empty());
        present
    }
    pub(super) fn release(&self, session: &SessionId, uri: &str) {
        let mut book = self.lock();
        if let Some(row) = book.resources.get_mut(uri) {
            row.owners.remove(session);
        }
        book.resources
            .retain(|_, row| row.wire || !row.owners.is_empty());
    }
    pub(super) fn end(&self, session: Option<&SessionId>) {
        let mut book = self.lock();
        if session.is_none() {
            book.closed = true;
        }
        let mut changed = false;
        for row in book.resources.values_mut() {
            if let Some(session) = session {
                changed |= row.owners.remove(session).is_some();
            } else {
                changed |= !row.owners.is_empty();
                row.owners.clear();
            }
        }
        book.resources.retain(|_, row| row.wire);
        let hint = book.hint.clone();
        drop(book);
        if changed {
            emit(hint);
        }
    }
    pub(super) fn pending(&self) -> Option<String> {
        self.lock()
            .resources
            .iter()
            .find(|(_, row)| row.wire && row.owners.is_empty())
            .map(|(uri, _)| uri.clone())
    }
    pub(super) fn removed(&self, uri: &str) {
        self.lock().resources.remove(uri);
    }
    pub(super) fn close(&self) {
        let mut book = self.lock();
        book.closed = true;
        let changed = !book.resources.is_empty();
        book.resources.clear();
        let hint = book.hint.clone();
        drop(book);
        if changed {
            emit(hint);
        }
    }
    pub(super) fn notify(&self, value: &Value) -> bool {
        let Some(uri) = value["params"]["uri"].as_str() else {
            return false;
        };
        let mut book = self.lock();
        let Some(row) = book.resources.get_mut(uri) else {
            return false;
        };
        let was_clean = row
            .owners
            .values()
            .any(|owner| owner.baseline == row.revision);
        row.revision = row.revision.saturating_add(1);
        // Saturation remains dirty, even after a subsequent explicit read.
        if row.revision == u64::MAX {
            for baseline in row.owners.values_mut() {
                baseline.baseline = 0;
            }
        }
        was_clean && !row.owners.is_empty()
    }
    pub(super) fn rows(&self, session: &SessionId, server: &str) -> Vec<Value> {
        self.lock().resources.iter().filter_map(|(uri, row)| {
            row.owners.get(session).filter(|owner| owner.retained).map(|owner| json!({"server":server,"uri":uri,"updated":owner.baseline != row.revision}))
        }).collect()
    }
}

pub(in crate::dogfood) struct Interest {
    state: Arc<State>,
    session: SessionId,
    uri: String,
    pending: bool,
    token: OwnerToken,
}
impl Interest {
    pub(in crate::dogfood) fn read(&self, revision: u64) {
        self.state
            .read(&self.session, &self.uri, &self.token, revision);
    }
    pub(in crate::dogfood) fn keep(mut self) -> Result<(), String> {
        let present = self
            .state
            .settle(&self.session, &self.uri, &self.token, true);
        self.pending = false;
        if present {
            Ok(())
        } else {
            Err("Resource subscription ended before completion; retry".into())
        }
    }
}
impl Drop for Interest {
    fn drop(&mut self) {
        if self.pending {
            self.state
                .settle(&self.session, &self.uri, &self.token, false);
        }
    }
}
fn emit(hint: Option<(std::sync::Weak<super::notifications::Emitter>, String)>) {
    if let Some((weak, key)) = hint {
        if let Some(emitter) = weak.upgrade() {
            emitter.resource_changed(&key);
        }
    }
}
impl McpRegistry {
    pub(super) fn track_resources(&self, state: &Arc<State>) {
        let mut states = self
            .resource_states
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        states.retain(|state| state.strong_count() > 0);
        if self
            .resources_closing
            .load(std::sync::atomic::Ordering::Acquire)
        {
            state.close();
        }
        // Every state belongs to one of the registry's bounded live connections.
        states.push(Arc::downgrade(state));
    }
    pub(in crate::dogfood) fn end_resource_subscriptions(&self, session: Option<&SessionId>) {
        let mut states = self
            .resource_states
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        states.retain(|weak| {
            let Some(state) = weak.upgrade() else {
                return false;
            };
            state.end(session);
            true
        });
    }
    pub(in crate::dogfood) async fn close_resource_subscriptions(&self) -> Result<(), String> {
        self.resources_closing
            .store(true, std::sync::atomic::Ordering::Release);
        self.end_resource_subscriptions(None);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let pending = self
                    .resource_states
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .iter()
                    .filter_map(|state| state.upgrade())
                    .any(|state| !state.lock().resources.is_empty());
                if !pending {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| "MCP resource unsubscribe cleanup exceeded five seconds".into())
    }

    pub(in crate::dogfood) async fn resource_subscriptions(
        &self,
        key: &str,
        session: Option<&SessionId>,
    ) -> Vec<Value> {
        let Some(session) = session else {
            return Vec::new();
        };
        let entries = self.entries.lock().await;
        let Some(location) = entries.get(key) else {
            return Vec::new();
        };
        location
            .entries
            .iter()
            .filter_map(|(name, entry)| match entry {
                super::ServerEntry::Live(live)
                    if !live.io.is_retired() && live.observed_unusable().is_none() =>
                {
                    Some(live.io.resources.rows(session, name))
                }
                _ => None,
            })
            .flatten()
            .collect()
    }
}

/// Called only while owning the held channel's existing I/O mutex.
pub(super) async fn drain_one(
    held: &super::HeldIo<super::LiveIo>,
    io: &mut super::LiveIo,
) -> Result<(), super::ChannelError> {
    let Some(uri) = held.resources.pending() else {
        return Ok(());
    };
    let request = io
        .channel
        .unsubscribe_request(&uri)
        .map_err(super::ChannelError::Protocol)?;
    tokio::select! {
        _ = held.retired() => return Ok(()),
        result = io.channel.exchange(request, std::time::Duration::from_secs(2)) => {
            if !result?.is_object() { return Err(super::ChannelError::Protocol(grapt_mcp::McpProtocolError::Malformed("resource unsubscribe acknowledgement".into()))); }
        }
    }
    held.resources.removed(&uri);
    if let Some(signal) = io.channel.catalog_signal() {
        signal.changed();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_acquisitions_cannot_remove_another_reader_or_revive_ended_interests(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let state = Arc::new(State::default());
        let session: SessionId = "ses_resource_reader".parse()?;
        let cancelled = state.reserve(&session, "memo://guide")?;
        let retained = state.reserve(&session, "memo://guide")?;
        state.confirm("memo://guide");
        drop(cancelled);
        retained.keep()?;
        assert!(state.pending().is_none());
        assert_eq!(state.rows(&session, "book").len(), 1);
        let late = state.reserve(&session, "memo://guide")?;
        state.end(Some(&session));
        assert!(late.keep().is_err());
        assert!(state.rows(&session, "book").is_empty());
        assert_eq!(state.pending().as_deref(), Some("memo://guide"));
        // Stale drop cannot consume a replacement owner's pending acquisition.
        let stale = state.reserve(&session, "memo://guide")?;
        state.release(&session, "memo://guide");
        let fresh = state.reserve(&session, "memo://guide")?;
        drop(stale);
        fresh.keep()?;
        assert_eq!(state.rows(&session, "book").len(), 1);
        // Stale keep and read acknowledgement cannot retain or clear the new owner.
        let stale = state.reserve(&session, "memo://guide")?;
        state.end(Some(&session));
        let fresh = state.reserve(&session, "memo://guide")?;
        fresh.keep()?;
        assert!(state.notify(&json!({"params":{"uri":"memo://guide"}})));
        stale.read(state.revision("memo://guide"));
        assert_eq!(state.rows(&session, "book")[0]["updated"], true);
        assert!(stale.keep().is_err());
        let stale = state.reserve(&session, "memo://guide")?;
        state.release(&session, "memo://guide");
        let fresh = state.reserve(&session, "memo://guide")?;
        assert!(stale.keep().is_err());
        drop(fresh);
        assert!(state.rows(&session, "book").is_empty());
        assert!(state.pending().is_some());
        state.close();
        assert!(state.reserve(&session, "memo://guide").is_err());
        Ok(())
    }
}
