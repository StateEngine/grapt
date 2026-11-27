//! Authenticated prompt custody: the principal, agent and start
//! acknowledgement of each Session's live prompt, held until its run ends.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use grapt_api::SealedPrincipal;
use grapt_types::{AgentId, ExecutionId, MessageId, SessionId};

use super::ExecutionRefusal;

const MAX_LIVE_PROMPT_PRINCIPALS: usize = 64;

#[derive(Debug, Clone)]
pub(super) struct PromptPrincipal {
    pub(super) started: super::super::prompt_driver::Started,
    pub(super) execution: ExecutionId,
    pub(super) input: MessageId,
    pub(super) external_notice: Option<super::super::host_control::ExternalNoticeFence>,
    pub(super) observed: Arc<std::sync::atomic::AtomicBool>,
    pub(super) principal: SealedPrincipal,
    pub(super) agent: AgentId,
    pub(super) workspace: std::path::PathBuf,
    pub(super) instruction_roots: super::super::instructions::InstructionRoots,
    /// The MCP sources this turn's refresh found unusable, held until the
    /// chain opens after the promotion, so its notice follows the user's
    /// message in history.
    pub(super) mcp_notice: Option<Vec<grapt_session::notice::Unavailable>>,
}

#[derive(Debug, Default)]
pub(super) struct PromptPrincipals {
    entries: Mutex<HashMap<SessionId, PromptPrincipal>>,
}

impl PromptPrincipals {
    pub(super) fn register(
        self: &Arc<Self>,
        session: SessionId,
        principal: SealedPrincipal,
        agent: AgentId,
        instruction_roots: super::super::instructions::InstructionRoots,
        startup: (
            super::super::prompt_driver::Started,
            ExecutionId,
            Arc<std::sync::atomic::AtomicBool>,
            std::path::PathBuf,
            MessageId,
            Option<super::super::host_control::ExternalNoticeFence>,
        ),
    ) -> Result<PromptPrincipalCustody, ExecutionRefusal> {
        let mut entries = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if entries.contains_key(&session) {
            return Err(ExecutionRefusal::Run(format!(
                "session {} already has authenticated prompt custody",
                session.as_str()
            )));
        }
        if entries.len() >= MAX_LIVE_PROMPT_PRINCIPALS {
            return Err(ExecutionRefusal::Run(
                "authenticated prompt custody bound is exhausted".to_owned(),
            ));
        }
        let (started, execution, observed, workspace, input, external_notice) = startup;
        entries.insert(
            session.clone(),
            PromptPrincipal {
                started,
                execution,
                input,
                external_notice,
                observed,
                workspace,
                principal,
                agent,
                instruction_roots,
                mcp_notice: None,
            },
        );
        Ok(PromptPrincipalCustody {
            principals: Arc::clone(self),
            session,
        })
    }

    pub(super) fn get(&self, session: &SessionId) -> Option<PromptPrincipal> {
        match self.entries.lock() {
            Ok(entries) => entries.get(session).cloned(),
            Err(poisoned) => poisoned.into_inner().get(session).cloned(),
        }
    }

    /// Hold `unavailable` for the chain this Session's prompt opens.
    pub(super) fn hold_mcp_notice(
        &self,
        session: &SessionId,
        unavailable: Vec<grapt_session::notice::Unavailable>,
    ) {
        let mut entries = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(entry) = entries.get_mut(session) {
            entry.mcp_notice = Some(unavailable);
        }
    }

    /// The held MCP notice, once.
    pub(super) fn take_mcp_notice(
        &self,
        session: &SessionId,
    ) -> Option<Vec<grapt_session::notice::Unavailable>> {
        let mut entries = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        entries.get_mut(session)?.mcp_notice.take()
    }

    pub(super) fn remove(&self, session: &SessionId) {
        match self.entries.lock() {
            Ok(mut entries) => {
                entries.remove(session);
            }
            Err(poisoned) => {
                poisoned.into_inner().remove(session);
            }
        }
    }
}

pub(super) struct PromptPrincipalCustody {
    principals: Arc<PromptPrincipals>,
    session: SessionId,
}

impl Drop for PromptPrincipalCustody {
    fn drop(&mut self) {
        self.principals.remove(&self.session);
    }
}
