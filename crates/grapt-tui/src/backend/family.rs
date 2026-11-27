//! Last-request context and durable completion for the bounded family presentation.
use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    pub used: Option<u64>,
    pub capacity: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct Preview {
    pub session: SessionWireId,
    pub rows: Vec<Slice>,
}

struct Entry {
    aggregate: Option<u64>,
    updated: Option<u64>,
    activity: Option<grapt_client::client::SessionActivity>,
    outcome: Option<grapt_client::Outcome>,
    context: Option<u64>,
    error: Option<String>,
}

#[derive(Default)]
pub(super) struct Family {
    entries: BTreeMap<SessionWireId, Entry>,
    previews: Vec<Preview>,
}

impl Family {
    pub fn take_previews(&mut self) -> Vec<Preview> {
        std::mem::take(&mut self.previews)
    }

    /// One read per frame; catalog or live activity changes invalidate the cached report.
    pub fn refresh<C: grapt_client::Connection>(&mut self, client: &mut Client<C>) -> bool {
        let Some(selected) = client.selected().cloned() else {
            self.entries.clear();
            return false;
        };
        let root = client
            .sessions()
            .iter()
            .find(|row| row.id == selected)
            .and_then(|row| row.parent.clone())
            .unwrap_or_else(|| selected.clone());
        let mut members: Vec<_> = client
            .sessions()
            .iter()
            .filter(|row| row.id == root || row.parent.as_ref() == Some(&root))
            .cloned()
            .collect();
        members.sort_by_key(|row| {
            (
                row.id != selected && row.id != root,
                std::cmp::Reverse(row.detail.created_ms),
            )
        });
        members.truncate(client.bounds().max_retained_sessions);
        self.entries
            .retain(|id, _| members.iter().any(|row| &row.id == id));
        if let Some(view) = client.view() {
            let aggregate = members
                .iter()
                .find(|row| row.id == selected)
                .and_then(|row| row.detail.context_tokens);
            self.entries.insert(
                selected.clone(),
                Entry {
                    aggregate,
                    updated: members
                        .iter()
                        .find(|row| row.id == selected)
                        .and_then(|row| row.detail.updated_ms),
                    activity: client.observed_activity(&selected),
                    outcome: outcome(view),
                    context: view.cache.latest.context,
                    error: None,
                },
            );
        }
        let mut missing = members.iter().filter(|row| {
            row.id != selected
                && self
                    .entries
                    .get(&row.id)
                    .is_none_or(|entry| !entry.current(client, row))
        });
        let next = missing.next().cloned();
        let more = missing.next().is_some();
        if let Some(row) = next {
            // An unavailable report stays unknown; aggregate input is never a fallback.
            let (context, terminal, error) = match client.recent_session_view(&row.id) {
                Ok(view) => {
                    let terminal = outcome(&view);
                    self.previews.push(Preview {
                        session: row.id.clone(),
                        rows: view.slices,
                    });
                    (view.cache.latest.context, terminal, None)
                }
                Err(error) => (None, None, Some(error.to_string())),
            };
            self.entries.insert(
                row.id.clone(),
                Entry {
                    aggregate: row.detail.context_tokens,
                    updated: row.detail.updated_ms,
                    activity: client.observed_activity(&row.id),
                    outcome: terminal,
                    context,
                    error,
                },
            );
        }
        more
    }

    /// Completion is a durable history fact; idle and reconnect silence are not.
    pub fn status<C: grapt_client::Connection>(
        &self,
        client: &Client<C>,
        row: &SessionSummary,
    ) -> String {
        if client
            .permissions()
            .iter()
            .any(|ask| ask.session_id == row.id)
        {
            return "waiting for permission".into();
        }
        if client.selected() == Some(&row.id) && !client.forms().is_empty() {
            return "waiting for question".into();
        }
        let live = super::sessions::status(client, &row.id);
        if !matches!(live.as_str(), "idle" | "unreported" | "succeeded") {
            return live;
        }
        self.entries
            .get(&row.id)
            .filter(|entry| entry.current(client, row))
            .and_then(|entry| entry.outcome.as_ref())
            .map_or(live, |outcome| match outcome {
                grapt_client::Outcome::Succeeded => "completed".into(),
                grapt_client::Outcome::Failed => "failed".into(),
                grapt_client::Outcome::Interrupted => "interrupted".into(),
            })
    }

    pub fn context<C: grapt_client::Connection>(
        &self,
        client: &Client<C>,
        row: &SessionSummary,
    ) -> Context {
        let Some(entry) = self.entries.get(&row.id) else {
            return Context::default();
        };
        let model = row
            .detail
            .model
            .as_deref()
            .map(|model| model.split('#').next().unwrap_or(model));
        let capacity = model.and_then(|model| {
            let (provider, id) = model.split_once('/')?;
            client
                .models()
                .iter()
                .find(|candidate| {
                    candidate.provider_id.spelled() == provider && candidate.id.spelled() == id
                })
                .and_then(|model| model.context_window)
        });
        Context {
            used: entry.context,
            error: entry.error.clone(),
            capacity,
        }
    }
}

impl Entry {
    fn current<C: grapt_client::Connection>(
        &self,
        client: &Client<C>,
        row: &SessionSummary,
    ) -> bool {
        self.aggregate == row.detail.context_tokens
            && self.updated == row.detail.updated_ms
            && self.activity == client.observed_activity(&row.id)
    }
}

fn outcome(view: &grapt_client::SessionView) -> Option<grapt_client::Outcome> {
    match &view.execution {
        grapt_client::ExecutionState::Settled { outcome, .. } => Some(outcome.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
