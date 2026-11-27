//! Newest-first history (SC-HIST-01b): the tail a session goes live on, and
//! the older windows that arrive behind it.
//!
//! Opening a session used to walk every page oldest-first before the first
//! paint, and go live only if the walk finished. A session past the page bound
//! never went live at all, and re-walked every page about once a second. Now a
//! read takes ONE newest page, installs it and goes live; older pages follow in
//! the background on the existing single read lane.
//!
//! What is not yet loaded is not a gap. A gap means the TAIL may be wrong, and
//! the tail is exactly what the newest page established. So earlier history has
//! its own state, [`OlderHistory`], held beside the stream state and never
//! inside it: it is shown, it limits what accounting can claim, and it never
//! decides liveness.

use super::*;

/// How to ask for one page of a message list.
///
/// An ENUM rather than a struct of two optional strings, because the two forms
/// are mutually exclusive on the wire and the type is where that belongs.
///
/// The cursor CARRIES its own order — it decodes to
/// `{"id": …, "order": "asc", "direction": "next"}` — so a continuation that
/// also names an order is a conflicting request and the real product answers
/// it 400. A struct with two `Option`s can express that refused combination;
/// this cannot.
///
/// Measured against the live product rather than inferred: `?order=asc` then
/// `?order=asc&cursor=…` is a 400, and `?cursor=…` alone returns the rest.
#[derive(Debug, Clone, Copy)]
pub(super) enum Page<'a> {
    Sessions {
        search: &'a str,
        limit: usize,
    },
    /// The first page, in the order this client reads.
    ///
    /// The order is REQUESTED, never assumed: this client reads `desc`, newest
    /// first, so the page it goes live on is the newest one, and it folds that
    /// page's rows oldest-first. An earlier client took the wire order and
    /// rendered every reopened transcript backwards, with the reply above the
    /// prompt that caused it.
    First {
        order: &'a str,
    },
    /// One bounded page for the opt-in transcript reading window.
    Window {
        cursor: Option<&'a str>,
        descending: bool,
    },
}

/// How much of the history before the tail this client holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OlderHistory {
    /// The opt-in reader can fetch older pages on demand; no background read is queued.
    Available,
    /// The next older page is being read in the background.
    Loading,
    /// The walk reached the start of the session: everything is folded in, or
    /// evicted by retention and counted.
    #[default]
    Complete,
    /// Retained slices reached `max_retained_slices`, so older history would
    /// only be evicted again.
    Bounded,
    /// The read's page or byte budget ran out before the start was reached.
    /// Not `Complete`, and not `Bounded` either: retention may well have room.
    Exhausted { pages: usize, bytes: usize },
    /// An older page could not be read. The tail and liveness stand; the next
    /// tail read starts the walk again.
    Failed { reason: String },
}

/// The one background walk toward older history, for one tail read.
#[derive(Debug)]
pub(super) struct Backfill {
    session: SessionWireId,
    /// The tail read this walk belongs to. A reply for any other read of the
    /// same session is discarded, not spliced.
    generation: u64,
    cursor: String,
    pages: usize,
    bytes: usize,
    pub(super) pending: bool,
    /// Where the walk folds when the view still shows scrollback a re-read
    /// carried: the fresh window replaces it in one step once the walk stops,
    /// so the screen never empties and refills behind a live tail.
    staged: Option<SessionView>,
}

/// What one page leaves the walk to do.
pub(super) enum Step {
    Done(OlderHistory),
    More(String),
}

/// A tail install's fallible part, done: what `commit_tail` publishes.
pub(super) struct PreparedTail {
    pub(super) view: SessionView,
    step: Step,
    staged: Option<SessionView>,
}

/// Fold a newest-first TAIL page, recording how many slices each row folded
/// to, oldest row first, so a later tail read can find its seam. The map is
/// left empty when the page alone overflowed retention.
pub(super) fn tail_window(
    session: &SessionWireId,
    page: &crate::wire::MessagePage,
    bounds: Bounds,
) -> Result<SessionView, ProjectionError> {
    let mut view = SessionView::new(session);
    let mut rows = Vec::with_capacity(page.messages.len());
    for (message, id) in page.messages.iter().zip(&page.ids).rev() {
        let before = view.slices.len();
        view.apply_durable(message, bounds)?;
        rows.push((id.clone(), view.slices.len().saturating_sub(before)));
    }
    if view.dropped_slices == 0 {
        view.tail_rows = rows;
    }
    Ok(view)
}

/// Fold one newest-first page into a window of its own, oldest row first.
pub(super) fn segment(
    session: &SessionWireId,
    page: &crate::wire::MessagePage,
    bounds: Bounds,
) -> Result<SessionView, ProjectionError> {
    let mut view = SessionView::new(session);
    for message in page.messages.iter().rev() {
        view.apply_durable(message, bounds)?;
    }
    Ok(view)
}

/// The terminal a tail page ENDS on (R1′): its newest record that carries a
/// lifecycle, when that record is an assistant message the existing classifier
/// settles — an error-only record included, a `tool-calls` finish excluded,
/// because that step continues the turn.
///
/// System and unsupported rows carry no lifecycle and are passed over, so a
/// later product notice cannot hide the terminal before it. A user row or an
/// unsettled assistant record means a turn is in flight, and nothing older is
/// consulted: an older terminal there is one already held.
pub(super) fn tail_terminal(
    page: &crate::wire::MessagePage,
) -> Option<(crate::wire::MessageWireId, crate::projection::Outcome)> {
    use crate::wire::DurableMessage;
    for (message, id) in page.messages.iter().zip(&page.ids) {
        match message {
            DurableMessage::User { .. }
            | DurableMessage::SeatStartup { .. }
            | DurableMessage::Automated { .. } => return None,
            DurableMessage::Assistant { finish, error, .. } => {
                return SessionView::durable_outcome(finish.as_ref(), error.as_ref())
                    .map(|outcome| (id.clone(), outcome));
            }
            // A turn that failed before any assistant message: the service's
            // failure row is that turn's terminal, or it would read as running
            // forever once its live terminal was missed.
            DurableMessage::ExecutionFailure { .. } => {
                return Some((id.clone(), crate::projection::Outcome::Failed));
            }
            DurableMessage::Compaction { .. }
            | DurableMessage::System { .. }
            | DurableMessage::Unsupported { .. } => {}
        }
    }
    None
}

/// Decide what follows a page, once `retained` slices are held.
///
/// Exhaustion is judged on the RAW page: a page of rows this client cannot
/// draw folds to nothing and still continues. A short page — the product cuts
/// pages by encoded bytes — continues too, as long as it carried a row.
pub(super) fn after_page(
    retained: usize,
    page: &crate::wire::MessagePage,
    pages: usize,
    bytes: usize,
    bounds: Bounds,
) -> Step {
    match &page.next {
        Some(next) if page.rows > 0 => {
            if retained >= bounds.max_retained_slices {
                Step::Done(OlderHistory::Bounded)
            } else if pages >= bounds.max_message_pages || bytes >= byte_budget(bounds) {
                Step::Done(OlderHistory::Exhausted { pages, bytes })
            } else {
                Step::More(next.clone())
            }
        }
        _ => Step::Done(OlderHistory::Complete),
    }
}

/// The bytes one history read may accept across all its pages.
fn byte_budget(bounds: Bounds) -> usize {
    bounds
        .max_response_bytes
        .saturating_mul(bounds.max_message_pages)
}

impl<C: Connection> Client<C> {
    /// How much history before the tail the selected session holds.
    pub fn older_history(&self) -> &OlderHistory {
        &self.recovery.older
    }

    /// A new tail read is starting: any walk toward older history belongs to
    /// the read it replaces, so it is cancelled, and its replies are fenced
    /// out by the new generation.
    pub(super) fn begin_tail(&mut self) {
        self.cancel_tool_refresh();
        self.recovery.generation = self.recovery.generation.wrapping_add(1);
        if self
            .recovery
            .backfill
            .take()
            .is_some_and(|walk| walk.pending)
        {
            self.connection.cancel_read();
        }
    }

    /// A foreground read needs the lane. The walk keeps its cursor and asks
    /// for the same page again once the lane is free.
    pub(super) fn yield_backfill(&mut self) {
        if let Some(walk) = self.recovery.backfill.as_mut() {
            if walk.pending {
                walk.pending = false;
                self.connection.cancel_read();
            }
        }
    }

    /// Install a decoded newest page as the tail, and go live on it.
    ///
    /// Both reads use this: opening a session, and the re-read after a
    /// terminal or a reconnect. The tail never waits on older history. When
    /// the view being replaced holds scrollback strictly older than the new
    /// tail, that scrollback stays on screen, and the walk toward older
    /// history replaces it in one step when it stops.
    pub(super) fn adopt_tail(
        &mut self,
        session: &SessionWireId,
        page: &crate::wire::MessagePage,
        bytes: usize,
    ) -> Result<(), ClientError> {
        let prepared = self.prepare_tail(session, page, bytes)?;
        self.commit_tail(
            session,
            page,
            bytes,
            prepared,
            history_install::Carry::Legacy,
        );
        Ok(())
    }

    /// Everything a tail install can fail on, built without touching the
    /// client: the finished view, its paging step and the staged older view.
    pub(super) fn prepare_tail(
        &self,
        session: &SessionWireId,
        page: &crate::wire::MessagePage,
        bytes: usize,
    ) -> Result<PreparedTail, ClientError> {
        let mut view = tail_window(session, page, self.bounds)?;
        let step = if self.history_window.is_some() {
            // The manual reading window shares this tail install and terminal
            // policy, but loads older pages only when navigation requests them.
            Step::Done(if page.next.is_some() && page.rows > 0 {
                OlderHistory::Available
            } else {
                OlderHistory::Complete
            })
        } else {
            after_page(view.slices.len(), page, 1, bytes, self.bounds)
        };
        let carried = match step {
            Step::More(_) => self
                .views
                .get(session)
                .and_then(|previous| previous.older_than(&view.tail_rows)),
            Step::Done(_) => None,
        };
        let staged = carried.is_some().then(|| SessionView::new(session));
        if let Some(carried) = carried {
            view.prepend(carried, self.bounds);
        }
        Ok(PreparedTail { view, step, staged })
    }

    /// Install a prepared tail. Nothing here can fail, so a caller that
    /// prepared every section first publishes all of them or none.
    pub(super) fn commit_tail(
        &mut self,
        session: &SessionWireId,
        page: &crate::wire::MessagePage,
        bytes: usize,
        prepared: PreparedTail,
        carry: history_install::Carry,
    ) {
        let PreparedTail { view, step, staged } = prepared;
        let older = match &step {
            Step::Done(older) => older.clone(),
            Step::More(_) => OlderHistory::Loading,
        };
        self.commit_history(
            session,
            view,
            tail_terminal(page),
            older,
            page.unsupported,
            carry,
        );
        if self.selected.as_ref() == Some(session) {
            for (message, id) in page.messages.iter().zip(&page.ids).rev() {
                if matches!(message, crate::wire::DurableMessage::User { .. })
                    && !self.promoted_inputs.contains(id)
                {
                    self.promoted_inputs.push_back(id.clone());
                    if self.promoted_inputs.len() > self.bounds.max_retained_slices {
                        self.promoted_inputs.pop_front();
                    }
                }
            }
        }
        self.install_tail_cursor(page.next.clone(), page.rows == 0);
        if let Step::More(cursor) = step {
            self.recovery.backfill = Some(Backfill {
                session: session.clone(),
                generation: self.recovery.generation,
                cursor,
                pages: 1,
                bytes,
                pending: false,
                staged,
            });
        }
    }

    /// Read at most one older page, behind every foreground read.
    pub(super) fn advance_backfill(&mut self) -> bool {
        if self.recovery.history.is_some() || self.preset.job.is_some() {
            return false;
        }
        let Some(mut walk) = self.recovery.backfill.take() else {
            return false;
        };
        if self.selected.as_ref() != Some(&walk.session)
            || walk.generation != self.recovery.generation
        {
            if walk.pending {
                self.connection.cancel_read();
            }
            return false;
        }
        let response = if walk.pending {
            self.connection.poll_read()
        } else {
            WireRequest::new(Operation::MessageList, Some(&walk.session), None).and_then(
                |request| {
                    self.connection
                        .begin_read(&request.with_query("cursor", &walk.cursor))
                },
            )
        };
        let response = match response {
            Ok(Some(response)) => {
                walk.pending = false;
                response
            }
            Ok(None) => {
                walk.pending = true;
                self.recovery.backfill = Some(walk);
                return false;
            }
            Err(error) => return self.backfill_failed(error.to_string()),
        };
        let page = match self
            .accept_response(Operation::MessageList, response)
            .and_then(|bytes| {
                walk.bytes = walk.bytes.saturating_add(bytes.len());
                Ok(crate::wire::decode_messages(&bytes)?)
            }) {
            Ok(page) => page,
            Err(error) => return self.backfill_failed(error.to_string()),
        };
        walk.pages = walk.pages.saturating_add(1);
        // A continuation that hands back the cursor it was given made no
        // progress, and its rows are rows already read: discard them.
        if page.next.as_deref() == Some(walk.cursor.as_str()) {
            return self.backfill_failed("the server repeated a history cursor".to_owned());
        }
        let window = match segment(&walk.session, &page, self.bounds) {
            Ok(window) => window,
            Err(error) => return self.backfill_failed(error.to_string()),
        };
        let unsupported = self
            .cursor
            .unsupported_rows()
            .saturating_add(page.unsupported);
        self.cursor.presentation_unsupported(unsupported);
        let Some(view) = self.views.get_mut(&walk.session) else {
            return false;
        };
        let retained = match walk.staged.as_mut() {
            Some(staged) => {
                staged.prepend(window, self.bounds);
                let tail = view.slices.len().saturating_sub(view.older_len);
                staged.slices.len().saturating_add(tail)
            }
            None => {
                view.prepend(window, self.bounds);
                view.slices.len()
            }
        };
        let older = match after_page(retained, &page, walk.pages, walk.bytes, self.bounds) {
            Step::Done(older) => {
                if let Some(staged) = walk.staged.take() {
                    view.replace_older(staged, self.bounds);
                }
                older
            }
            Step::More(cursor) => {
                walk.cursor = cursor;
                self.recovery.backfill = Some(walk);
                OlderHistory::Loading
            }
        };
        view.cache.history_complete(older == OlderHistory::Complete);
        self.recovery.older = older;
        true
    }

    fn backfill_failed(&mut self, reason: String) -> bool {
        self.connection.cancel_read();
        self.recovery.older = OlderHistory::Failed { reason };
        self.diagnostics.degraded_reconciliations =
            self.diagnostics.degraded_reconciliations.saturating_add(1);
        true
    }
}

#[cfg(test)]
mod tests;
