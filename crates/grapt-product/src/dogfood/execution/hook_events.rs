//! Child lifecycle callbacks follow the existing owned execution, including failures.
use super::*;

impl LiveExecutor {
    pub(super) fn child_hooks(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
    ) -> Result<Option<Arc<super::super::hooks::HookRuntime>>, String> {
        let child = {
            let inbox = self.inbox.lock().unwrap_or_else(|error| error.into_inner());
            grapt_subagent::SubagentService::child_context_on(inbox.store(), session)
                .map_err(|error| error.to_string())?
                .is_some()
        };
        if !child {
            return Ok(None);
        }
        let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
        let record = SessionPorts::new(Arc::clone(&self.inbox), &authorities)
            .get(session)
            .map_err(|error| error.to_string())?;
        let root = std::fs::canonicalize(record.placement.directory())
            .map_err(|error| format!("Session hook directory is unreadable: {error}"))?;
        Ok(Some(Arc::new(
            self.location
                .load_hooks(&root)?
                .with_notices(super::super::hooks::NoticeContext {
                    inbox: Arc::clone(&self.inbox),
                    ids: Arc::clone(&self.ids),
                    clock: Arc::clone(&self.clock),
                    scope: grapt_session::notice::LifecycleHookScope::Execution(execution.clone()),
                }),
        )))
    }
}
