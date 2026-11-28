//! Session management uses the same authenticated ingress as the composer.
use super::prepared::{Accepted, Done, PreparedCall, Route, Step};
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

    /// Search a bounded server page without replacing the live session
    /// catalog. A search the server stopped before it could list anything
    /// says so, rather than passing for one with no matches.
    pub fn search_sessions(&mut self, search: &str) -> Result<SessionSearch, ClientError> {
        let limit = Self::search_limit(search, self.bounds.max_retained_sessions)?;
        let bytes = self.ingest(
            Operation::SessionList,
            None,
            None,
            None,
            None,
            Some(Page::Sessions { search, limit }),
        )?;
        self.found(&bytes)
    }

    /// Search off the loop; see [`Client::search_sessions`].
    pub fn prepare_session_search(&mut self, search: &str) -> Result<PreparedCall, ClientError> {
        let limit = Self::search_limit(search, self.bounds.max_retained_sessions)?;
        let route = Route {
            page: Some(Page::Sessions { search, limit }),
            ..Route::new(Operation::SessionList, None)
        };
        self.prepare_call(Step::Search, route)
    }

    pub(super) fn accept_search(
        &mut self,
        answer: Result<Vec<u8>, ClientError>,
    ) -> Result<Accepted, ClientError> {
        self.found(&answer?)
            .map(|found| Accepted::Done(Done::Found(found)))
    }

    fn search_limit(search: &str, retained: usize) -> Result<usize, ClientError> {
        if search.len() > 256 || search.chars().any(char::is_control) {
            return Err(ClientError::InvalidInput(
                "Session search exceeds its bound",
            ));
        }
        Ok(retained.min(SEARCH_PAGE_ROWS))
    }

    fn found(&self, bytes: &[u8]) -> Result<SessionSearch, ClientError> {
        let (rows, next) = crate::wire::decode_sessions_page(bytes)?;
        let reach = CatalogReach::of(&rows, next.as_deref());
        let rows = rows
            .into_iter()
            .take(self.bounds.max_retained_sessions)
            .map(|row| SessionSummary {
                id: row.id,
                title: row.title,
                parent: row.parent,
                detail: row.detail,
            })
            .collect();
        Ok(SessionSearch { rows, reach })
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

    /// Rename off the loop; see [`Client::rename_session`].
    pub fn prepare_rename(
        &mut self,
        session: &SessionWireId,
        title: &str,
    ) -> Result<PreparedCall, ClientError> {
        let route = Route {
            body: Some(crate::wire::encode_rename(title.trim())?),
            ..Route::new(Operation::SessionRename, Some(session))
        };
        self.prepare_call(Step::Rename, route)
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

    /// Delete off the loop; see [`Client::delete_session`].
    pub fn prepare_delete(&mut self, session: &SessionWireId) -> Result<PreparedCall, ClientError> {
        let route = Route::new(Operation::SessionDelete, Some(session));
        self.prepare_call(Step::Delete(session.clone()), route)
    }

    /// Re-read the session catalog off the loop.
    pub fn prepare_sessions_refresh(&mut self) -> Result<PreparedCall, ClientError> {
        self.prepare_call(Step::Sessions, Route::new(Operation::SessionList, None))
    }

    /// A confirmed deletion or an explicit not-found read retires cached state.
    pub(super) fn forget_session(&mut self, session: &SessionWireId) {
        self.sessions.retain(|row| &row.id != session);
        self.views.remove(session);
        self.activity.remove(session);
        self.observations.remove(session);
        self.pending.remove(session);
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
