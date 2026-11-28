//! Reading and choosing projects, agents and models.
//!
//! Split from `client.rs` by SUBJECT when that file passed this workspace's
//! module ceiling. Every route, bound and refusal is unchanged.

use super::*;
use std::collections::BTreeSet;

#[derive(Debug)]
pub(super) struct McpRead {
    location: crate::wire::FormLocation,
    session: Option<SessionWireId>,
    operation: Operation,
    existing_forms: BTreeSet<crate::wire::FormWireId>,
}

#[cfg(test)]
#[path = "catalogs_tests.rs"]
mod tests;

impl<C: Connection> Client<C> {
    pub fn projects(&self) -> &[crate::wire::ProjectRow] {
        &self.projects
    }

    /// The agent roster for the selected project, hidden rows already removed.
    ///
    /// Filtered HERE rather than at the wire edge, so the rows the product
    /// marked hidden are a rendering decision this crate can witness rather
    /// than an absence nobody wrote down.
    pub fn agents(&self) -> &[crate::wire::AgentRow] {
        &self.agents
    }

    pub(super) fn install_agents(&mut self, agents: Vec<crate::wire::AgentRow>) {
        // Retain only bounded identities for explicit saved-session restoration.
        // Hidden rows never consume picker slots or become new draft choices.
        self.catalog_refresh.hidden_agents = agents
            .iter()
            .filter(|agent| {
                agent.hidden || agent.mode == grapt_api::u3a::AgentInfoDtoMode::Subagent
            })
            .take(self.bounds.max_catalog_rows)
            .map(|agent| agent.id.clone())
            .collect();
        let visible = agents
            .into_iter()
            .filter(|agent| {
                !agent.hidden && agent.mode != grapt_api::u3a::AgentInfoDtoMode::Subagent
            })
            .collect();
        self.agents = self.keep_catalog(visible, self.bounds.max_catalog_rows);
    }

    pub(super) fn saved_agent_offered(&self, id: &crate::wire::AgentWireId) -> bool {
        self.agents.iter().any(|row| &row.id == id)
            || self.catalog_refresh.hidden_agents.contains(id)
    }

    pub fn models(&self) -> &[crate::wire::ModelRow] {
        &self.models
    }

    /// The configured MCP servers, once the pane has read them.
    pub fn mcp_servers(&self) -> &[crate::wire::McpServerRow] {
        &self.mcp_servers
    }

    /// The config refusal or inspection read error shown in place of stale rows.
    pub fn mcp_refusal(&self) -> Option<&str> {
        self.mcp_refusal.as_deref()
    }

    /// Begin a bounded MCP read; socket transports finish in `pump`.
    ///
    /// Like every catalog read, scoped by the selected project's worktree.
    /// A refused config is PANE STATE rather than an error: the 400 message
    /// is the content the pane shows in place of the list, and a user whose
    /// config is refused is exactly the user who needs to see which file the
    /// product names.
    pub fn refresh_mcp(&mut self) -> Result<(), ClientError> {
        self.read_mcp(false)
    }

    pub fn refresh_mcp_inspection(&mut self) -> Result<(), ClientError> {
        self.read_mcp(true)
    }

    fn read_mcp(&mut self, inspect: bool) -> Result<(), ClientError> {
        // Host-started I/O never coexists with a pending resume.
        self.abandon_resume();
        if self.operator_request_pending() || self.mcp_catalog_pending() {
            return Err(ClientError::Mcp(
                "An operator request is already in progress; draft kept.".into(),
            ));
        }
        self.mcp_servers.clear();
        self.mcp_refusal = None;
        let result = self.fetch_mcp(inspect);
        if inspect {
            if let Err(error) = &result {
                self.mcp_refusal = Some(error.to_string());
            }
        }
        result
    }

    fn fetch_mcp(&mut self, inspect: bool) -> Result<(), ClientError> {
        let location = self.form_location().ok_or(ClientError::NoProject)?;
        let operation = if inspect {
            Operation::McpInspect
        } else {
            Operation::McpList
        };
        let request = WireRequest::new(operation, self.selected.as_ref(), None)?;
        // A selected Session already names its complete MCP placement. Do not
        // contradict a nested Session cwd with the project's directory.
        let request = if self.selected.is_some() {
            request
        } else {
            request.with_query("location[directory]", &location.directory)
        };
        self.mcp_read = Some(McpRead {
            // Old startup forms may be suspended after process loss. They do
            // not identify a human wait belonging to this new acquisition.
            existing_forms: self
                .location_forms()
                .iter()
                .map(|row| row.id.clone())
                .collect(),
            location,
            session: self.selected.clone(),
            operation,
        });
        let result = self.begin_operator(&request);
        self.finish_mcp_read(result).map(|_| ())
    }

    pub fn mcp_catalog_pending(&self) -> bool {
        self.mcp_read.is_some()
    }

    pub fn cancel_mcp_catalog(&mut self) {
        if self.mcp_read.take().is_some() {
            self.connection.cancel_operator();
            self.mcp_refusal = Some("MCP inspection canceled; draft kept.".into());
        }
    }

    /// A refused service answers no MCP read: a waiting one ends with the
    /// refusal, which the waiting command reports.
    pub(super) fn refuse_mcp_catalog(&mut self, refusal: String) {
        self.mcp_refresh_needed = false;
        if self.mcp_read.take().is_some() {
            self.connection.cancel_operator();
            self.mcp_refusal = Some(refusal);
        }
    }

    pub(super) fn advance_mcp_catalog(&mut self) -> bool {
        if self.mcp_read.is_none()
            && self.mcp_refresh_needed
            && !self.operator_request_pending()
            && !self.catalog_pending()
        {
            self.mcp_refresh_needed = false;
            if let Err(error) = self.fetch_mcp(true) {
                self.mcp_refusal = Some(error.to_string());
            }
            return true;
        }
        let Some(read) = &self.mcp_read else {
            return false;
        };
        if self.form_location().as_ref() != Some(&read.location) || self.selected != read.session {
            self.cancel_mcp_catalog();
            self.mcp_refusal = Some("Session or location changed; MCP result discarded.".into());
            return true;
        }
        self.connection.operator_human_wait(
            self.location_forms
                .startup_wait(&read.location, &read.existing_forms),
        );
        let result = self.poll_operator();
        match self.finish_mcp_read(result) {
            Ok(changed) => changed,
            Err(error) => {
                self.mcp_refusal = Some(error.to_string());
                true
            }
        }
    }

    fn finish_mcp_read(
        &mut self,
        result: Result<Option<WireResponse>, ClientError>,
    ) -> Result<bool, ClientError> {
        if matches!(result, Ok(None)) {
            return Ok(false);
        }
        let read = self
            .mcp_read
            .take()
            .ok_or_else(|| ClientError::Mcp("No MCP read pending.".into()))?;
        if self.form_location().as_ref() != Some(&read.location) || self.selected != read.session {
            return Err(ClientError::Mcp(
                "Session or location changed; MCP result discarded.".into(),
            ));
        }
        match result.and_then(|response| {
            self.accept_response(
                read.operation,
                response.ok_or(TransportError::NotConnected)?,
            )
        }) {
            Ok(bytes) => {
                let rows = if read.operation == Operation::McpInspect {
                    crate::wire::decode_mcp_inspection(&bytes)?
                } else {
                    crate::wire::decode_mcp_servers(&bytes)?
                };
                self.mcp_servers = self.keep_catalog(rows, self.bounds.max_catalog_rows);
                let owner = (read.session, read.location);
                if self.mcp_watch_owner.as_ref() != Some(&owner)
                    || read.operation == Operation::McpInspect
                {
                    self.mcp_watches.clear();
                }
                if read.operation == Operation::McpInspect {
                    let dto: grapt_api::u3a::McpInspectionResponseDto =
                        serde_json::from_slice(&bytes)
                            .map_err(|error| ClientError::Mcp(error.to_string()))?;
                    if let Some(rows) = dto.subscriptions().as_present() {
                        self.mcp_watches = rows.as_slice().to_vec();
                    }
                }
                self.mcp_watch_owner = Some(owner);
                Ok(true)
            }
            Err(ClientError::Refused { message, .. }) => {
                self.mcp_watches.clear();
                self.mcp_refusal = Some(message);
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }

    pub fn project(&self) -> Option<&crate::wire::ProjectRow> {
        self.project.as_ref()
    }

    pub fn agent(&self) -> Option<&crate::wire::AgentWireId> {
        self.agent.as_ref()
    }

    pub fn model(&self) -> Option<&crate::wire::ModelRow> {
        self.model.as_ref()
    }

    /// Whether a prompt may be composed yet.
    ///
    /// The three selections are a PRECONDITION, not a suggestion. A client that
    /// let a prompt through without them would either send it to whatever the
    /// server defaulted to — silently answering with an agent and model the
    /// user never chose — or fail at the server with an error the user has no
    /// way to connect to the screen they were looking at.
    pub fn ready_to_chat(&self) -> bool {
        self.project.is_some() && self.agent.is_some() && self.model.is_some()
    }

    /// Keep at most `bound` rows, COUNTING whatever that refuses.
    ///
    /// One place, because the three catalog reads had the same silent `take`
    /// three times over and a fourth would have been written the same way.
    ///
    /// The bound is passed in rather than read here because the contract has
    /// TWO: models carry their own larger one, and collapsing them into a
    /// single client number is what made this client truncate a conformant
    /// model list.
    pub(super) fn keep_catalog<T>(&mut self, rows: Vec<T>, bound: usize) -> Vec<T> {
        let dropped = rows.len().saturating_sub(bound);
        if dropped > 0 {
            self.diagnostics.catalog_rows_dropped = self
                .diagnostics
                .catalog_rows_dropped
                .saturating_add(dropped);
        }
        rows.into_iter().take(bound).collect()
    }

    /// Offer the requested repository through the existing location DTO; the user
    /// still chooses the project, agent and account/model in the gateway.
    pub fn offer_directory(&mut self, directory: &str) -> Result<(), ClientError> {
        let bytes = self.ingest(
            Operation::ProjectCurrent,
            None,
            None,
            None,
            Some(directory),
            None,
        )?;
        self.accept_directory(&bytes, directory)
    }

    /// Validate an explicit directory against the canonical saved Session.
    pub fn offer_saved_directory(
        &mut self,
        session: &SessionWireId,
        directory: &str,
    ) -> Result<(), ClientError> {
        let bytes = self.ingest(
            Operation::ProjectCurrent,
            Some(session),
            None,
            None,
            Some(directory),
            None,
        )?;
        self.accept_directory(&bytes, directory)
    }

    pub(super) fn accept_directory(
        &mut self,
        bytes: &[u8],
        directory: &str,
    ) -> Result<(), ClientError> {
        let (id, worktree) = crate::wire::decode_project_current(bytes)?;
        if directory.len() > 4096
            || directory.chars().any(char::is_control)
            || !creation::absolute_backend_directory(directory)
        {
            return Err(ClientError::InvalidInput(
                "Launch directory must be an absolute backend path within 4096 bytes.",
            ));
        }
        self.launch_directory = Some(directory.to_owned());
        self.launch_project = Some(crate::wire::ProjectRow {
            id,
            worktree,
            name: None,
        });
        Ok(())
    }

    /// Open the explicitly offered directory as a new draft, without a write.
    /// Session creation retains the same uncertainty guard as cached drafts.
    pub fn begin_directory_draft(&mut self) -> Result<(), ClientError> {
        let project = self.launch_project.clone().ok_or(ClientError::NoProject)?;
        self.select_project(&project)?;
        self.draft_creation = creation::Draft::Ready;
        Ok(())
    }

    /// Read the project gateway.
    pub fn refresh_projects(&mut self) -> Result<(), ClientError> {
        let bytes = self.ingest(Operation::ProjectList, None, None, None, None, None)?;
        self.accept_projects(&bytes)
    }

    pub(super) fn accept_projects(&mut self, bytes: &[u8]) -> Result<(), ClientError> {
        let mut rows = crate::wire::decode_projects(bytes)?;
        if let Some(project) = &self.launch_project {
            rows.retain(|row| row.id != project.id || row.worktree != project.worktree);
            rows.insert(0, project.clone());
        }
        self.projects = self.keep_catalog(rows, self.bounds.max_catalog_rows);
        Ok(())
    }

    /// Choose a project and read the catalog scoped to it.
    ///
    /// Choosing a project is not a cosmetic marker: every catalog route is
    /// scoped by that project's worktree, so the roster and the picker are
    /// re-read here. Keeping the previous project's rows on screen under a new
    /// project's name is the exact defect this ordering exists to prevent.
    ///
    /// Any agent or model already chosen is CLEARED, because neither is
    /// guaranteed to exist at the new location. Carrying them forward would
    /// leave the composer enabled against a selection the new project may not
    /// offer.
    ///
    /// The read MCP list and any config refusal go with the switch the same
    /// way: they were read under the old location's worktree, and neither is
    /// truth under the new one.
    pub fn select_project(&mut self, project: &crate::wire::ProjectRow) -> Result<(), ClientError> {
        if self
            .catalog_refresh
            .binding
            .as_ref()
            .is_some_and(|bound| &bound.project != project)
        {
            self.catalog_refresh.binding = None;
        }
        self.location_forms.clear();
        self.location_forms_dirty = true;
        self.raise_session_forms(true);
        // A location change ends any resume of the stream it scoped.
        self.drop_resume();
        self.project = Some(project.clone());
        self.agent = None;
        self.model = None;
        self.mcp_servers.clear();
        self.cancel_mcp_catalog();
        self.mcp_refusal = None;
        self.mcp_login = None;
        self.cancel_operator_request();
        if self.catalog_refresh.enabled() {
            return self.start_catalog_refresh();
        }
        self.refresh_catalog()
    }

    /// Re-read the agent roster and model catalog for the selected project.
    pub(super) fn refresh_catalog(&mut self) -> Result<(), ClientError> {
        self.catalog_refresh.hidden_agents.clear();
        let Some(directory) = self
            .project
            .as_ref()
            .map(|project| project.worktree.clone())
        else {
            return Err(ClientError::NoProject);
        };

        let bytes = self.ingest(
            Operation::AgentList,
            None,
            None,
            None,
            Some(&directory),
            None,
        )?;
        self.install_agents(crate::wire::decode_agents(&bytes)?);

        let bytes = self.ingest(
            Operation::ModelList,
            None,
            None,
            None,
            Some(&directory),
            None,
        )?;
        let rows = crate::wire::decode_models(&bytes)?;
        self.models = self.keep_catalog(rows, self.bounds.max_model_rows);

        // The default is read but NOT auto-applied. The journey asks the user
        // to select a model before chatting; pre-selecting the default would
        // satisfy the precondition on the user's behalf and hide the screen the
        // acceptance is about. It is offered as a marked row instead.
        let bytes = self.ingest(
            Operation::ModelDefault,
            None,
            None,
            None,
            Some(&directory),
            None,
        )?;
        self.default_model = crate::wire::decode_model_default(&bytes)?;
        Ok(())
    }

    /// The model this location starts on, if it declares one.
    pub fn default_model(&self) -> Option<&crate::wire::ModelRow> {
        self.default_model.as_ref()
    }

    /// Choose the agent for subsequent work.
    ///
    /// Recorded locally when no session is open yet — the choice then travels
    /// on `SessionCreateRequestDto` — and sent through `v2.session.switchAgent`
    /// when one is. Both paths exist because both are real: the roster is
    /// chosen before the first session, and the frozen frontend also lets an
    /// open session switch.
    pub fn select_agent(&mut self, agent: &crate::wire::AgentWireId) -> Result<(), ClientError> {
        if !self.agents.iter().any(|row| &row.id == agent) {
            return Err(ClientError::NotOffered { what: "agent" });
        }
        if let Some(session) = self.selected.clone() {
            let body = crate::wire::encode_agent_select(agent)?;
            self.ingest(
                Operation::SessionAgentSelect,
                Some(&session),
                None,
                Some(body),
                None,
                None,
            )?;
        }
        self.agent = Some(agent.clone());
        self.persist_catalog_choice();
        Ok(())
    }

    /// Choose the model for subsequent work.
    ///
    /// A DISABLED model is refused here rather than sent. The picker renders it
    /// — hiding a model the user is looking for answers "where is it" with an
    /// empty list — but selecting one would produce a server refusal the user
    /// cannot act on, so the refusal is made local and legible instead.
    pub fn select_model(&mut self, model: &crate::wire::ModelRow) -> Result<(), ClientError> {
        let model = self.effort_choice(model)?;
        let offered = self.models.iter().any(|row| {
            row.id == model.id
                && row.provider_id == model.provider_id
                && row.variant == model.variant
        });
        if !offered {
            return Err(ClientError::NotOffered { what: "model" });
        }
        if !model.enabled {
            return Err(ClientError::NotEnabled);
        }
        if let Some(session) = self.selected.clone() {
            let body = crate::wire::encode_model_choice(
                &model.id,
                &model.provider_id,
                model.variant.as_ref(),
            )?;
            self.ingest(
                Operation::SessionModelSelect,
                Some(&session),
                None,
                Some(body),
                None,
                None,
            )?;
        }
        self.model = Some(model.clone());
        self.launch_effort = None;
        self.persist_catalog_choice();
        Ok(())
    }

    /// Make the selection a witness needs before it can reach a session.
    ///
    /// The first project, its first agent and its first ENABLED model — the
    /// choices a user makes by pressing Enter three times. It exists so that
    /// witnesses about reconnection, permissions or the transcript do not each
    /// restate the selection journey; the selection journey itself is
    /// witnessed by name in `app::tests` and `client::tests`.
    #[cfg(test)]
    pub(crate) fn select_first_offer(&mut self) -> Result<(), ClientError> {
        self.refresh_projects()?;
        let Some(project) = self.projects.first().cloned() else {
            return Err(ClientError::NotOffered { what: "project" });
        };
        self.select_project(&project)?;
        let Some(agent) = self.agents.first().map(|row| row.id.clone()) else {
            return Err(ClientError::NotOffered { what: "agent" });
        };
        self.select_agent(&agent)?;
        let Some(model) = self.models.iter().find(|row| row.enabled).cloned() else {
            return Err(ClientError::NotOffered { what: "model" });
        };
        self.select_model(&model)
    }
}
