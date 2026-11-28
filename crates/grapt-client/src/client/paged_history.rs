//! One bounded page request,
//! cursor ownership in the protocol client, and a reading view separate from live state.
use super::history_move::HistoryMove;
use super::prepared::{Accepted, Browse, Done, Route, Step};
use super::*;
use crate::wire::MessagePage;

const PAGE_ROWS: &str = "200";

#[cfg(test)]
mod tests;

#[derive(Debug, Default)]
pub(super) struct Window {
    older: Option<String>,
    reading: Option<Reading>,
    revision: u64,
    // None: no movement to undo; Some(None): restore the live tail.
    // At most one prior bounded page is retained, moved rather than cloned.
    previous: Option<Option<Reading>>,
}

#[derive(Debug)]
struct Reading {
    view: SessionView,
    older: Option<String>,
    newer: Option<String>,
    descending: bool,
}

impl<C: Connection> Client<C> {
    /// Read a small display-only tail without changing selection, cursors or controls.
    pub fn recent_session_view(
        &mut self,
        session: &SessionWireId,
    ) -> Result<SessionView, ClientError> {
        let page = self.read_window_page(session, None, true)?;
        let bounds = Bounds {
            max_retained_slices: self.bounds.max_retained_slices.min(32),
            ..self.bounds
        };
        project(session, &page, true, bounds)
    }

    /// Opt into bounded newest-first history before attaching a session.
    /// The live projection remains the authority for controller operations;
    /// browsing never replaces it or changes the selected session.
    pub fn enable_paged_history(&mut self) {
        self.history_window = Some(Window::default());
    }

    pub fn display_view(&self) -> Option<&SessionView> {
        self.history_window
            .as_ref()
            .and_then(|w| w.reading.as_ref())
            .map(|r| &r.view)
            .or_else(|| self.view())
    }

    pub fn history_revision(&self) -> u64 {
        self.history_window.as_ref().map_or(0, |w| w.revision)
    }

    pub fn browsing_history(&self) -> bool {
        self.history_window
            .as_ref()
            .is_some_and(|w| w.reading.is_some())
    }

    pub fn history_edges(&self) -> (bool, bool) {
        self.history_window
            .as_ref()
            .map_or((false, false), |w| match &w.reading {
                Some(r) => (r.older.is_some(), r.newer.is_some()),
                None => (w.older.is_some(), false),
            })
    }

    /// Load one page only. A repeated/refused cursor leaves the current reading intact.
    pub fn browse_history(&mut self, movement: HistoryMove) -> Result<bool, ClientError> {
        let browse = match self.plan_browse(movement)? {
            Ok(browse) => browse,
            Err(changed) => return Ok(changed),
        };
        let page =
            self.read_window_page(&browse.session, browse.cursor.as_deref(), browse.descending)?;
        self.install_page(browse, page)
    }

    /// Build a page move off the loop: settled at once when it needs no read.
    pub fn prepare_browse(&mut self, movement: HistoryMove) -> Result<Accepted, ClientError> {
        let browse = match self.plan_browse(movement)? {
            Ok(browse) => browse,
            Err(changed) => return Ok(Accepted::Done(Done::Browsed(changed))),
        };
        let (session, cursor) = (browse.session.clone(), browse.cursor.clone());
        let route = Route {
            page: Some(Page::Window {
                cursor: cursor.as_deref(),
                descending: browse.descending,
            }),
            ..Route::new(Operation::MessageList, Some(&session))
        };
        self.prepare_call(Step::Browse(browse), route)
            .map(Accepted::Next)
    }

    /// A page read for a Session no longer selected moves nothing.
    pub(super) fn accept_browse(
        &mut self,
        browse: Browse,
        answer: Result<Vec<u8>, ClientError>,
    ) -> Result<Accepted, ClientError> {
        let bytes = answer?;
        if self.selected.as_ref() != Some(&browse.session) {
            return Ok(Accepted::Done(Done::Browsed(false)));
        }
        let page = crate::wire::decode_messages(&bytes)?;
        self.install_page(browse, page)
            .map(|changed| Accepted::Done(Done::Browsed(changed)))
    }

    /// The read a move needs, or whether it changed the page without one.
    fn plan_browse(&mut self, movement: HistoryMove) -> Result<Result<Browse, bool>, ClientError> {
        if movement == HistoryMove::Cancel {
            let Some(window) = &mut self.history_window else {
                return Ok(Err(false));
            };
            let Some(previous) = window.previous.take() else {
                return Ok(Err(false));
            };
            window.reading = previous;
            window.revision = window.revision.saturating_add(1);
            return Ok(Err(true));
        }
        if let Some(window) = &mut self.history_window {
            window.previous = None;
        }
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        let Some(window) = &self.history_window else {
            return Ok(Err(false));
        };
        if movement == HistoryMove::Latest {
            let changed = window.reading.is_some();
            self.reset_history_window_reading();
            return Ok(Err(changed));
        }
        let (cursor, descending) = match movement {
            HistoryMove::Beginning => (None, false),
            HistoryMove::Older | HistoryMove::Newer => {
                let cursor = match &window.reading {
                    Some(r) if movement == HistoryMove::Older => r.older.clone(),
                    Some(r) => r.newer.clone(),
                    None if movement == HistoryMove::Older => window.older.clone(),
                    None => None,
                };
                let Some(cursor) = cursor else {
                    return Ok(Err(false));
                };
                (
                    Some(cursor),
                    window.reading.as_ref().is_none_or(|r| r.descending),
                )
            }
            HistoryMove::Latest | HistoryMove::Cancel => return Ok(Err(false)),
        };
        Ok(Ok(Browse {
            session,
            cursor,
            descending,
            movement,
        }))
    }

    fn install_page(&mut self, browse: Browse, page: MessagePage) -> Result<bool, ClientError> {
        let Browse {
            session,
            cursor,
            descending,
            movement,
        } = browse;
        let (older, newer) = edges(&page, descending);
        if cursor.is_some() && (page.next == cursor || page.previous == cursor) {
            return Err(crate::wire::WireError::Invalid {
                what: "history cursor",
                value: "repeated cursor".into(),
            }
            .into());
        }
        if page.rows == 0 {
            if let Some(window) = &mut self.history_window {
                match &mut window.reading {
                    Some(r) if movement == HistoryMove::Older => r.older = None,
                    Some(r) => r.newer = None,
                    None => window.older = None,
                }
            }
            return Ok(false);
        }
        let view = project(&session, &page, descending, self.bounds)?;
        if let Some(window) = &mut self.history_window {
            window.previous = Some(window.reading.take());
            window.reading = Some(Reading {
                view,
                older: if movement == HistoryMove::Beginning {
                    None
                } else {
                    older
                },
                newer,
                descending,
            });
            window.revision = window.revision.saturating_add(1);
        }
        Ok(true)
    }

    pub(super) fn reset_history_window(&mut self) {
        if let Some(window) = &mut self.history_window {
            window.older = None;
            window.previous = None;
            window.reading = None;
            window.revision = window.revision.saturating_add(1);
        }
    }

    fn reset_history_window_reading(&mut self) {
        if let Some(window) = &mut self.history_window {
            window.previous = Some(window.reading.take());
            window.revision = window.revision.saturating_add(1);
        }
    }

    fn read_window_page(
        &mut self,
        session: &SessionWireId,
        cursor: Option<&str>,
        descending: bool,
    ) -> Result<MessagePage, ClientError> {
        let bytes = self.ingest(
            Operation::MessageList,
            Some(session),
            None,
            None,
            None,
            Some(Page::Window { cursor, descending }),
        )?;
        Ok(crate::wire::decode_messages(&bytes)?)
    }

    pub(super) fn install_tail_cursor(&mut self, older: Option<String>, empty: bool) {
        if let Some(window) = &mut self.history_window {
            window.older = if empty { None } else { older };
            if let Some(session) = &self.selected {
                if let Some(view) = self.views.get_mut(session) {
                    view.cache.history_complete(window.older.is_none());
                }
            }
        }
    }
}

pub(super) fn window_request(
    session: &SessionWireId,
    cursor: Option<&str>,
    descending: bool,
) -> Result<WireRequest, TransportError> {
    let request = WireRequest::new(Operation::MessageList, Some(session), None)?
        .with_query("limit", PAGE_ROWS);
    Ok(match cursor {
        Some(cursor) => request.with_query("cursor", cursor),
        None => request.with_query("order", if descending { "desc" } else { "asc" }),
    })
}

fn edges(page: &MessagePage, descending: bool) -> (Option<String>, Option<String>) {
    if descending {
        (page.next.clone(), page.previous.clone())
    } else {
        (page.previous.clone(), page.next.clone())
    }
}

fn project(
    session: &SessionWireId,
    page: &MessagePage,
    descending: bool,
    bounds: Bounds,
) -> Result<SessionView, ClientError> {
    let mut view = SessionView::new(session);
    if descending {
        for message in page.messages.iter().rev() {
            view.apply_durable(message, bounds)?;
        }
    } else {
        for message in &page.messages {
            view.apply_durable(message, bounds)?;
        }
    }
    Ok(view)
}
