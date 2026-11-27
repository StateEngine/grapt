//! Read the last-known catalog first, then refresh without holding input.
//! Refresh fencing (Apache-2.0).
// Copyright 2025 OpenAI. Modified for Grapt's existing DTO controller.

use super::*;
use std::path::{Path, PathBuf};

mod disk;
mod draft;
mod repository;
#[cfg(test)]
mod tests;

#[derive(Debug, Default)]
pub(super) struct State {
    pub(super) binding: Option<Binding>,
    pub(super) hidden_agents: Vec<crate::wire::AgentWireId>,
    repository: Option<repository::Read>,
    repository_note: Option<String>,
    root: Option<PathBuf>,
    validated: bool,
    job: Option<Job>,
    note: Option<String>,
    wanted: Option<(
        Option<crate::wire::AgentWireId>,
        Option<crate::wire::ModelRow>,
    )>,
}

impl State {
    pub(super) fn reading_repository(&self) -> bool {
        self.repository.is_some()
    }

    pub(super) fn enabled(&self) -> bool {
        self.root.is_some()
    }
}

#[derive(Debug)]
pub(super) struct Binding {
    pub(super) session: SessionWireId,
    pub(super) project: crate::wire::ProjectRow,
    pub(super) directory: String,
}

#[derive(Debug)]
struct Job {
    project: crate::wire::ProjectRow,
    phase: usize,
    pending: bool,
    parts: Snapshot,
    session: Option<SessionWireId>,
}

#[derive(Debug, Default)]
struct Snapshot {
    agents: Vec<u8>,
    models: Vec<u8>,
    default: Vec<u8>,
    choice: Option<Choice>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    agent: crate::wire::AgentWireId,
    model: crate::wire::ModelWireId,
    provider: crate::wire::ProviderWireId,
    variant: Option<crate::wire::ModelVariantWireId>,
}

impl Choice {
    fn current<C: Connection>(client: &Client<C>) -> Option<Self> {
        let agent = client.agent.as_ref()?;
        let model = client.model.as_ref()?;
        Some(Self {
            agent: agent.clone(),
            model: model.id.clone(),
            provider: model.provider_id.clone(),
            variant: model.variant.clone(),
        })
    }
}

impl Snapshot {
    fn validate(&self, bound: usize) -> Result<(), String> {
        if [&self.agents, &self.models, &self.default]
            .iter()
            .any(|body| body.len() > bound)
        {
            return Err("catalog cache response exceeds its byte bound".into());
        }
        crate::wire::decode_agents(&self.agents).map_err(|_| "cached agent catalog is invalid")?;
        crate::wire::decode_models(&self.models).map_err(|_| "cached model catalog is invalid")?;
        crate::wire::decode_model_default(&self.default)
            .map_err(|_| "cached default model is invalid")?;
        Ok(())
    }
}

impl<C: Connection> Client<C> {
    /// Revalidate the existing location after a provider connection changes.
    pub fn refresh_connection_catalog(&mut self) -> Result<(), ClientError> {
        let agent = self.agent.clone();
        let model = self.model.clone();
        if self.catalog_refresh.enabled() {
            self.start_catalog_refresh()?;
            self.remember_catalog_selection(agent, model);
            Ok(())
        } else {
            self.refresh_catalog()
        }
    }

    /// The profile owns this expendable cache; it contains catalog DTOs only,
    /// never account credentials or permission decisions.
    pub fn set_catalog_profile(&mut self, root: &Path) {
        self.catalog_refresh.root = Some(root.to_path_buf());
    }

    pub fn catalog_status(&self) -> Option<&str> {
        self.catalog_refresh.note.as_deref()
    }

    /// Live choices have completed validation for the selected project.
    pub fn catalog_validated(&self) -> bool {
        !self.catalog_refresh.enabled() || self.catalog_refresh.validated
    }

    pub fn catalog_pending(&self) -> bool {
        self.catalog_refresh.job.is_some()
    }

    /// Whether a catalog-lane read, the catalog job's or the repository
    /// status's, is in flight.
    pub(super) fn catalog_in_flight(&self) -> bool {
        self.catalog_refresh
            .job
            .as_ref()
            .is_some_and(|job| job.pending)
            || self
                .catalog_refresh
                .repository
                .as_ref()
                .is_some_and(repository::Read::pending)
    }

    pub(crate) fn catalog_in_background(&self) -> bool {
        self.catalog_refresh.enabled()
    }

    pub(super) fn start_catalog_refresh(&mut self) -> Result<(), ClientError> {
        self.connection.cancel_catalog();
        self.catalog_refresh.validated = false;
        self.catalog_refresh.repository = None;
        self.catalog_refresh.repository_note = None;
        self.command_catalog.reset();
        self.catalog_refresh.wanted = None;
        self.catalog_refresh.hidden_agents.clear();
        self.agents.clear();
        self.models.clear();
        self.default_model = None;
        let project = self.project.clone().ok_or(ClientError::NoProject)?;
        let root = self
            .catalog_refresh
            .root
            .as_ref()
            .ok_or(ClientError::NoProject)?;
        match disk::load(root, &project, self.bounds.max_response_bytes) {
            Ok(Some(snapshot)) => {
                self.install_catalog(&snapshot)?;
                self.catalog_refresh.note = Some("Catalog: cached; refreshing…".into());
            }
            Ok(None) => {
                self.catalog_refresh.note = Some("Catalog: loading… Drafts stay editable.".into())
            }
            Err(error) => self.catalog_refresh.note = Some(format!("Catalog: refreshing; {error}")),
        }
        self.catalog_refresh.job = Some(Job {
            session: self
                .catalog_refresh
                .binding
                .as_ref()
                .filter(|bound| bound.project == project)
                .map(|bound| bound.session.clone()),
            project,
            phase: 0,
            pending: false,
            parts: Snapshot::default(),
        });
        Ok(())
    }

    pub(super) fn remember_catalog_selection(
        &mut self,
        agent: Option<crate::wire::AgentWireId>,
        model: Option<crate::wire::ModelRow>,
    ) {
        self.catalog_refresh.wanted = Some((agent, model));
    }

    fn install_catalog(&mut self, snapshot: &Snapshot) -> Result<(), ClientError> {
        let agents = crate::wire::decode_agents(&snapshot.agents)?;
        let models = crate::wire::decode_models(&snapshot.models)?;
        let default = crate::wire::decode_model_default(&snapshot.default)?;
        self.install_catalog_rows(agents, models, default);
        Ok(())
    }

    fn install_catalog_rows(
        &mut self,
        agents: Vec<crate::wire::AgentRow>,
        models: Vec<crate::wire::ModelRow>,
        default: Option<crate::wire::ModelRow>,
    ) {
        self.install_agents(agents);
        self.models = self.keep_catalog(models, self.bounds.max_model_rows);
        self.default_model = default;
    }

    /// Install already decoded target catalogs without a request or cache read.
    /// Old source jobs must not publish over the newly attached checkout.
    pub(super) fn install_attachment_catalog(
        &mut self,
        agents: Vec<crate::wire::AgentRow>,
        models: Vec<crate::wire::ModelRow>,
        default: Option<crate::wire::ModelRow>,
    ) {
        self.connection.cancel_catalog();
        self.catalog_refresh.job = None;
        self.catalog_refresh.repository = None;
        self.catalog_refresh.repository_note = None;
        self.catalog_refresh.wanted = None;
        self.catalog_refresh.note = None;
        self.catalog_refresh.validated = true;
        self.command_catalog.reset();
        self.install_catalog_rows(agents, models, default);
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn advance_catalog(&mut self) -> bool {
        let Some(mut job) = self.catalog_refresh.job.take() else {
            return self.advance_repository();
        };
        if self.project.as_ref() != Some(&job.project) {
            self.connection.cancel_catalog();
            return false;
        }
        let operation = [
            Operation::AgentList,
            Operation::ModelList,
            Operation::ModelDefault,
        ][job.phase];
        let result = (|| -> Result<Option<Vec<u8>>, ClientError> {
            let response = if job.pending {
                self.connection.poll_catalog()?
            } else {
                let mut request = WireRequest::new(operation, None, None)?
                    .with_query("location[directory]", &job.project.worktree);
                if let Some(session) = &job.session {
                    request = request.with_query("session", crate::wire::Spelled::spelled(session));
                }
                self.connection.begin_catalog(&request)?
            };
            response
                .map(|response| self.accept_response(operation, response))
                .transpose()
        })();
        match result {
            Ok(None) => {
                job.pending = true;
                self.catalog_refresh.job = Some(job);
                false
            }
            Ok(Some(bytes)) => {
                match job.phase {
                    0 => job.parts.agents = bytes,
                    1 => job.parts.models = bytes,
                    _ => job.parts.default = bytes,
                }
                job.phase += 1;
                job.pending = false;
                if job.phase < 3 {
                    self.catalog_refresh.job = Some(job);
                    return true;
                }
                if let Err(error) = self.install_catalog(&job.parts) {
                    self.catalog_refresh.note = Some(format!("Catalog refresh failed: {error}"));
                    return true;
                }
                self.catalog_refresh.validated = true;
                let wanted = self.catalog_refresh.wanted.take();
                let agent = self
                    .agent
                    .clone()
                    .or_else(|| wanted.as_ref().and_then(|w| w.0.clone()));
                let model = self.model.clone().or_else(|| wanted.and_then(|w| w.1));
                let bound_session = self.catalog_refresh.binding.as_ref().is_some_and(|bound| {
                    bound.project == job.project
                        && self.selected.as_ref() == Some(&bound.session)
                        && job.session.as_ref() == Some(&bound.session)
                });
                self.agent = agent.filter(|id| {
                    self.agents.iter().any(|row| &row.id == id)
                        || (bound_session && self.saved_agent_offered(id))
                });
                self.model = model.and_then(|selected| {
                    self.models
                        .iter()
                        .find(|row| same_model(row, &selected) && row.enabled)
                        .cloned()
                });
                self.catalog_refresh.note = None;
                if self
                    .launch_effort
                    .as_ref()
                    .is_some_and(|effort| effort.session.is_some())
                {
                    if let Err(error) = self.apply_launch_effort() {
                        self.model = None;
                        self.catalog_refresh.note =
                            Some(format!("Launch --reasoning-effort refused: {error}"));
                        return true;
                    }
                }
                job.parts.choice = Choice::current(self);
                if let Some(root) = &self.catalog_refresh.root {
                    if let Err(error) = disk::save(
                        root,
                        &job.project,
                        &job.parts,
                        self.bounds.max_response_bytes,
                    ) {
                        self.catalog_refresh.note = Some(format!("Catalog refreshed; {error}"));
                    }
                }
                true
            }
            Err(error) => {
                self.connection.cancel_catalog();
                self.catalog_refresh.note = Some(format!("Catalog refresh failed: {error}"));
                true
            }
        }
    }
}

fn same_model(a: &crate::wire::ModelRow, b: &crate::wire::ModelRow) -> bool {
    a.id == b.id && a.provider_id == b.provider_id && a.variant == b.variant
}
