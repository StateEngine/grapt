use super::*;

/// The key a Session's MCP connections are held under.
///
/// ONE DERIVATION, used by publication, the execution-budget lookup and
/// dispatch. They previously derived it separately and disagreed: publication
/// used the placement directory and dispatch canonicalized it, which on
/// Windows is a verbatim path that never matches. A helper is not decoration
/// here -- it is what makes the three sites incapable of drifting apart again.
///
/// It is NOT a normalisation. The placement directory is the Session's own
/// recorded identity and is carried through unmodified; anything else would
/// change what an approval binds.
pub(super) fn registry_location(placement: &grapt_types::LocationRef) -> String {
    placement.directory().to_owned()
}

impl std::fmt::Debug for LiveLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveLocation")
            .field("service_root", &self.service_root)
            .finish_non_exhaustive()
    }
}

impl LiveLocation {
    pub(super) fn provider_snapshot(&self) -> Result<Arc<LiveProviders>, String> {
        self.providers
            .snapshot()
            .live
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| "connect a provider before starting execution".to_owned())
    }
    /// `session`'s location, resolved once for its directory and reused by
    /// every later turn there (SC-REL-11).
    pub(super) async fn resolved(
        &self,
        session: &SessionId,
    ) -> Result<Arc<super::super::locations::ResolvedLocation>, String> {
        // Dropped before the await: neither crosses threads.
        let directory = {
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            let record = SessionPorts::new(Arc::clone(&self.inbox), &authorities)
                .get(session)
                .map_err(|error| error.to_string())?;
            record.placement.directory().to_owned()
        };
        self.locations.resolve(session, &directory).await
    }

    /// The chain's synchronous refusals, in one place for both callers: a
    /// prompt checks them before it is acknowledged, so they still answer it
    /// as its refusal in their own words, and open_chain builds on what they
    /// return. Only MCP startup and planning come after the acknowledgement.
    pub(super) fn preflight(&self, session: &SessionId) -> Result<Preflight, String> {
        let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
        let record = SessionPorts::new(Arc::clone(&self.inbox), &authorities)
            .get(session)
            .map_err(|error| error.to_string())?;
        self.require_ephemeral_config(session, &record)?;
        super::super::session_config::validate_selected(
            &self.inbox,
            session,
            &self.service_root,
            Path::new(record.placement.directory()),
        )?;
        let compaction = super::super::compaction_config::CompactionSettings::read(
            &self.service_root,
            Path::new(record.placement.directory()),
        )?;
        let agents = super::super::agent_definitions::Catalog::read(
            &self.service_root,
            Path::new(record.placement.directory()),
        )?;
        let child = {
            let inbox = self.inbox.lock().unwrap_or_else(|error| error.into_inner());
            grapt_subagent::SubagentService::child_context_on(inbox.store(), session)
                .map_err(|error| error.to_string())?
                .is_some()
        };
        let agent_model = agents
            .selected_for(record.agent.as_deref().unwrap_or("default"), child)?
            .map(|agent| agent.model())
            .transpose()?
            .flatten();
        let providers = self.provider_snapshot()?;
        let selection = record.model.as_ref().or(agent_model.as_ref()).cloned()
            .unwrap_or_else(|| serde_json::json!({"providerID":providers.configured_provider.as_str(),"id":providers.configured_model.as_str()}));
        let targets = providers.targets.selected(&selection)?;
        let issued = targets.issue()?;
        let tool_root = std::fs::canonicalize(record.placement.directory())
            .map_err(|error| format!("Session tool directory is unreadable: {error}"))?;
        if !tool_root.is_dir() {
            return Err("Session tool directory is not a directory".to_owned());
        }
        let policy =
            super::super::permission_preset::effective(&self.service_root, &record, |id| {
                SessionPorts::new(Arc::clone(&self.inbox), &authorities)
                    .get(id)
                    .map_err(|error| error.to_string())
            })?;
        Ok(Preflight {
            providers,
            record,
            compaction,
            targets,
            issued,
            tool_root,
            policy,
        })
    }

    /// The target a Session with this selection runs its Steps on.
    pub(super) fn resolve_target(
        &self,
        selected: Option<&serde_json::Value>,
    ) -> Result<ResolvedModelTarget, String> {
        let providers = self.provider_snapshot()?;
        providers
            .targets
            .selected(&providers.selection_or_configured(selected))?
            .issue()
    }
}

impl LiveProviders {
    pub(super) fn selection_or_configured(
        &self,
        selected: Option<&serde_json::Value>,
    ) -> serde_json::Value {
        selected.cloned().unwrap_or_else(|| {
            serde_json::json!({
                "providerID":self.configured_provider.as_str(), "id":self.configured_model.as_str(),
            })
        })
    }
}
