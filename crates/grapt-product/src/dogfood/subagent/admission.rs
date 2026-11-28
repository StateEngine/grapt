//! Child Session creation and mode selection through the existing domain service.

use std::sync::Arc;

use grapt_runtime::SessionPorts;
use grapt_session::SessionSeed;
use grapt_subagent::{SubagentAdmission, SubagentMode, SubagentService};
use grapt_types::{
    AgentId, ClockAuthority, DeterministicAuthorities, EffectiveGraphId, IdAuthority, OperationId,
    SessionId, SubagentOperationId, ToolCallId,
};

use super::{child_routing, AdmittedChild, ServerApplication};

/// Options for the shared admission seam. Placement defaults to the parent.
#[derive(Debug)]
pub(in crate::dogfood) struct ChildOptions {
    mode: SubagentMode,
    history_before_call: Option<ToolCallId>,
    worktree: Option<super::placement::PreparedWorktree>,
}

impl ChildOptions {
    pub(in crate::dogfood) const fn new(mode: SubagentMode) -> Self {
        Self {
            mode,
            worktree: None,
            history_before_call: None,
        }
    }

    pub(in crate::dogfood) fn with_history_before(mut self, call: ToolCallId) -> Self {
        self.history_before_call = Some(call);
        self
    }

    pub(in crate::dogfood) async fn with_worktree(
        mut self,
        application: &ServerApplication,
        parent: &SessionId,
    ) -> Result<Self, String> {
        if self.worktree.is_some() {
            return Err("child options already have a prepared worktree".into());
        }
        self.worktree =
            Some(super::placement::PreparedWorktree::prepare(application, parent).await?);
        Ok(self)
    }

    fn directory<'a>(&'a self, parent: &SessionId, source: &'a str) -> Result<&'a str, String> {
        match &self.worktree {
            Some(worktree) => worktree.directory(parent, source),
            None => Ok(source),
        }
    }
}

/// Create the child before recording an operation that refers to it.
/// A later admission failure leaves an inert, visible Session for inspection.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(in crate::dogfood) fn admit_with_options(
    application: &ServerApplication,
    parent: &SessionId,
    agent: &AgentId,
    description: &str,
    tool_call: Option<ToolCallId>,
    options: &ChildOptions,
) -> Result<AdmittedChild, String> {
    if options
        .history_before_call
        .as_ref()
        .is_some_and(|source| tool_call.as_ref() != Some(source))
    {
        return Err("the history source must be this child admission's tool call".into());
    }
    let record = application.get_record(parent)?;
    // Placement was prepared before admission. Refuse a mismatched parent or
    // source before any child Session or operation can be written.
    let directory = options.directory(parent, record.placement.directory())?;
    let provider_catalog = application.catalog_snapshot();
    let service_root = std::path::Path::new(&provider_catalog.directory);
    let parent_policy = super::super::permission_preset::effective(service_root, &record, |id| {
        application.get_record(id)
    })?;
    let agents = super::super::agent_definitions::Catalog::read(
        service_root,
        std::path::Path::new(directory),
    )?;
    let definition_model = agents
        .selected_for(agent.as_str(), true)?
        .map(|definition| definition.model())
        .transpose()?
        .flatten();
    let parent_agents = super::super::agent_definitions::Catalog::read(
        service_root,
        std::path::Path::new(record.placement.directory()),
    )?;
    let parent_definition_model = parent_agents
        .selected(record.agent.as_deref().unwrap_or("default"))?
        .map(|definition| definition.model())
        .transpose()?
        .flatten();
    let routing = child_routing(
        application,
        parent,
        record.model.as_ref().or(parent_definition_model.as_ref()),
        definition_model.as_ref(),
    )?;
    let seed = SessionSeed::new(record.project.clone(), directory.to_owned(), description)
        .map_err(|error| error.to_string())?
        .with_parent(parent.clone())
        .with_agent(agent.as_str())
        .map_err(|error| error.to_string())?;

    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let session = if let Some(call) = &options.history_before_call {
        let placement = if options.worktree.is_some() {
            seed.placement().map_err(|error| error.to_string())?
        } else {
            record.placement.clone()
        };
        let mut held = application.lock_inbox();
        let forked = grapt_session::fork_session_before_call_at_location(
            held.store_mut(),
            &authorities,
            parent,
            call,
            &placement,
            &mut [&mut grapt_inbox::InboxProjector],
        )
        .map_err(|error| format!("the child's history could not be forked: {error}"))?;
        grapt_session::selection::select_agent_committed(
            held.store_mut(),
            &authorities,
            &forked.session,
            agent,
            &mut [&mut grapt_session::SessionListProjector],
        )
        .map_err(|error| format!("the child agent could not be selected: {error}"))?;
        grapt_session::title::rename_session(
            held.store_mut(),
            &authorities,
            &forked.session,
            description,
        )
        .map_err(|error| format!("the child task could not be named: {error}"))?;
        forked.session
    } else {
        SessionPorts::new(Arc::clone(&application.inbox), &authorities)
            .create(&seed)
            .map_err(|error| error.to_string())?
            .session
    };
    {
        let mut held = application.lock_inbox();
        grapt_session::selection::select_preset_committed(
            held.store_mut(),
            &authorities,
            &session,
            Some(parent_policy.preset.name()),
            &mut [&mut grapt_session::SessionListProjector],
        )
        .map_err(|error| format!("the child permission ceiling could not be selected: {error}"))?;
    }
    if let Some(model) = &routing.selection {
        // The child's own Steps resolve their target from this selection, so
        // the routed model is the one that actually runs.
        let mut held = application.lock_inbox();
        let seq = grapt_session::selection::select_model_committed(
            held.store_mut(),
            &authorities,
            &session,
            model,
            &mut [&mut grapt_session::SessionListProjector],
        )
        .map_err(|error| format!("the child model could not be selected: {error}"))?;
        if let Some(seq) = seq {
            super::super::input_events::publish(&held, &application.hub, &session, seq);
        }
    }

    // Three DISTINCT identities, none derived from another. The operation
    // family, the subagent address and the child Session are separate
    // coordinates on purpose: the oracle keyed the operation by child session
    // id, and that impersonation is exactly what these newtypes remove.
    let operation = OperationId::new(format!("opr_{}", application.ids.mint().as_str()))
        .ok_or_else(|| "minted operation id is invalid".to_owned())?;
    let subagent = SubagentOperationId::new(format!("sub_{}", application.ids.mint().as_str()))
        .ok_or_else(|| "minted subagent operation id is invalid".to_owned())?;
    let effective_graph = EffectiveGraphId::new(format!("efg_{}", application.ids.mint().as_str()))
        .ok_or_else(|| "minted effective graph id is invalid".to_owned())?;

    let admission = SubagentAdmission {
        operation,
        subagent: subagent.clone(),
        child: session.clone(),
        parent: parent.clone(),
        agent: agent.clone(),
        // The target snapshotted at admission (amendment §7.5): the explicit
        // Subagent model selection, or the parent's own target.
        model: routing.target,
        description: description.to_owned(),
        mode: options.mode,
        tool_call,
        effective_graph,
    };

    let created_ms = application.clock.now().as_millis();
    let held = application.lock_inbox();
    // The subagent tables live in the SAME database the inbox owns — delivery
    // admits on the parent — so the schema is installed on the borrowed store
    // rather than by opening a second connection. A second connection would put
    // these writes under the store's writer election instead of the inbox's
    // borrow, which is a governance change nothing here needs.
    SubagentService::ensure_schema_on(held.store()).map_err(|error| error.to_string())?;
    SubagentService::open_on(held.store(), &admission, created_ms)
        .map_err(|error| error.to_string())?;

    Ok(AdmittedChild {
        session,
        operation: subagent,
    })
}
