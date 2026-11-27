//! Session management uses the same authenticated ingress as the composer.
use super::*;

// The existing engine SessionListLimits caps one page at 128 rows.
const SEARCH_PAGE_ROWS: usize = 128;

impl<C: Connection> Client<C> {
    pub(super) fn remember_session(&mut self, row: SessionSummary) {
        self.sessions.retain(|old| old.id != row.id);
        if self.bounds.max_retained_sessions > 0 {
            self.sessions
                .truncate(self.bounds.max_retained_sessions.saturating_sub(1));
            self.sessions.insert(0, row);
        }
    }

    /// Search a bounded server page without replacing the live session catalog.
    pub fn search_sessions(&mut self, search: &str) -> Result<Vec<SessionSummary>, ClientError> {
        if search.len() > 256 || search.chars().any(char::is_control) {
            return Err(ClientError::InvalidInput(
                "Session search exceeds its bound",
            ));
        }
        let bytes = self.ingest(
            Operation::SessionList,
            None,
            None,
            None,
            None,
            Some(Page::Sessions {
                search,
                limit: self.bounds.max_retained_sessions.min(SEARCH_PAGE_ROWS),
            }),
        )?;
        Ok(crate::wire::decode_sessions(&bytes)?
            .into_iter()
            .take(self.bounds.max_retained_sessions)
            .map(|row| SessionSummary {
                id: row.id,
                title: row.title,
                parent: row.parent,
                detail: row.detail,
            })
            .collect())
    }

    /// Rename the addressed row without attaching it or rewriting saved choices.
    pub fn rename_session(
        &mut self,
        session: &SessionWireId,
        title: &str,
    ) -> Result<(), ClientError> {
        let title = title.trim();
        let body = crate::wire::encode_rename(title)?;
        self.ingest(
            Operation::SessionRename,
            Some(session),
            None,
            Some(body),
            None,
            None,
        )?;
        Ok(())
    }

    /// Only an acknowledged deletion removes local state. Refusal and uncertain
    /// transport outcomes retain the row and its drafts for explicit recovery.
    pub fn delete_session(&mut self, session: &SessionWireId) -> Result<(), ClientError> {
        self.ingest(
            Operation::SessionDelete,
            Some(session),
            None,
            None,
            None,
            None,
        )?;
        self.forget_session(session);
        Ok(())
    }

    /// A confirmed deletion or an explicit not-found read retires cached state.
    pub(super) fn forget_session(&mut self, session: &SessionWireId) {
        self.sessions.retain(|row| &row.id != session);
        self.views.remove(session);
        self.activity.remove(session);
        self.observations.remove(session);
        self.pending.remove(session);
        self.quiescence_fact_epoch.remove(session);
        self.quiescence_terminal_epoch.remove(session);
        self.permissions.retain(|ask| &ask.session_id != session);
        if self.selected.as_ref() == Some(session) {
            self.cancel_mcp_catalog();
            self.cancel_operator_request();
            self.connection.cancel_read();
            self.reset_history_window();
            self.recovery.history = None;
            self.promoted_inputs.clear();
            self.forms.clear();
            self.subagent_model = None;
            self.tool_refresh = None;
            self.clear_attachment();
        }
    }
}
