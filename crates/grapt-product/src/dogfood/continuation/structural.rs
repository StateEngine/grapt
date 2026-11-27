use super::*;
use grapt_store::session_event_queries::boundary::{self, Probe};

impl DurableContinuation {
    /// Locate the first row that cannot belong to one bounded summary window.
    /// Each indexed probe observes the first over-bound row directly; absence
    /// means that family fits, never that a full page was assumed complete.
    pub(super) fn structural_boundary(&self) -> Result<Option<grapt_types::EventSeq>, String> {
        Ok(self.structural_admission()?.map(|admission| admission.seq))
    }

    pub(in crate::dogfood) fn structural_admission(
        &self,
    ) -> Result<Option<StructuralAdmission>, String> {
        self.read_projection(Self::structural_admission_at)
    }

    /// A trimmed retry of history past the read bounds reads from the newest
    /// window those bounds serve, as a summary does. Returns that window's
    /// floor, `None` when the history since the cut fits. Reader contention is
    /// the plan's typed capacity wait, never a refusal.
    pub(in crate::dogfood) fn floor_for_trim(
        &mut self,
    ) -> Result<Option<grapt_types::EventSeq>, grapt_runtime::PlanRefusal> {
        let floor = self.read_for_plan(|reader| {
            reader
                .summary_window_floor_at()
                .map_err(grapt_runtime::PlanRefusal::from)
        })?;
        if floor.is_some() {
            self.set_summary_floor(floor);
        }
        Ok(floor)
    }

    /// The window floor, kept as a non-zero value so the continuation stays
    /// within the plan context's size bound.
    pub(super) fn summary_floor(&self) -> Option<grapt_types::EventSeq> {
        self.summary_floor
            .map(|floor| grapt_types::EventSeq::new(floor.get()))
    }

    pub(super) fn set_summary_floor(&mut self, floor: Option<grapt_types::EventSeq>) {
        self.summary_floor = floor.and_then(|floor| std::num::NonZeroU64::new(floor.value()));
    }

    /// Where reads of the history since the retained cut begin: the cut, or
    /// the summary floor when that lies later.
    pub(super) fn read_from(
        &self,
        retained: Option<&retained::RetainedContext>,
    ) -> Option<grapt_types::EventSeq> {
        let cut = retained.map(|context| context.from);
        match (cut, self.summary_floor()) {
            (Some(cut), Some(floor)) if floor.value() > cut.value() => Some(floor),
            (None, floor) => floor,
            (cut, _) => cut,
        }
    }

    /// The start of the newest window the bounded reads serve, when the
    /// history since the retained cut is past them. A summary quotes that
    /// window; what lies before it was already carried by the previous
    /// summary or is dropped, oldest first, as a prior-art compactor drops
    /// the oldest item of an input that does not fit. The window opens on an
    /// input, so it never begins inside an exchange. `None` when the whole
    /// history since the cut fits.
    pub(in crate::dogfood) fn summary_window_floor(
        &self,
    ) -> Result<Option<grapt_types::EventSeq>, String> {
        self.read_projection(Self::summary_window_floor_at)
    }

    fn summary_window_floor_at(&self) -> Result<Option<grapt_types::EventSeq>, String> {
        let inbox = lock(&self.inbox);
        let retained =
            retained::RetainedContext::load(&inbox, &self.artifacts, &self.session, &self.input)?;
        let from = retained.as_ref().map(|context| context.from);
        if self.structural_admission_from(&inbox, from)?.is_none() {
            return Ok(None);
        }
        let head = grapt_store::session_event_queries::bounds::head(inbox.store(), &self.session)?
            .ok_or("summary window has no history")?
            .value();
        let fits = |start: u64| -> Result<bool, String> {
            self.structural_admission_from(&inbox, Some(grapt_types::EventSeq::new(start)))
                .map(|over| over.is_none())
        };
        // `lo` does not fit, `hi` does: the smallest start that fits.
        let (mut lo, mut hi) = (from.map_or(0, grapt_types::EventSeq::value), head);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if fits(mid)? {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        boundary::exchange_start_at_or_after(
            inbox.store(),
            &self.session,
            grapt_types::EventSeq::new(hi),
        )?
        .map(Some)
        .ok_or_else(|| "no whole exchange fits the bounds a summary can read".to_owned())
    }

    fn structural_admission_at(&self) -> Result<Option<StructuralAdmission>, String> {
        let inbox = lock(&self.inbox);
        let retained =
            retained::RetainedContext::load(&inbox, &self.artifacts, &self.session, &self.input)?;
        let from = self.read_from(retained.as_ref());
        self.structural_admission_from(&inbox, from)
    }

    fn structural_admission_from(
        &self,
        inbox: &Inbox,
        from: Option<grapt_types::EventSeq>,
    ) -> Result<Option<StructuralAdmission>, String> {
        // Each probe counts at least every row its reader reads, so a window
        // past a read's bound compacts before that read would refuse. The
        // probes ignore history visibility, which only ever counts more.
        let probe =
            |family, limit| boundary::first_over(inbox.store(), &self.session, from, family, limit);
        let rows = MAX_CONTINUATION_ROWS;
        let tool = probe(Probe::Tool, grapt_session::MAX_TOOL_CALL_PAGE_ROWS)?;
        let permission = probe(Probe::Permission, rows)?;
        let identity = probe(Probe::ToolIdentity, rows)?;
        let terminal = probe(Probe::ToolTerminal, rows)?;
        let item = probe(Probe::ContextItem, MAX_CONTEXT_ITEMS)?;
        // The promoted page reads human and synthetic inputs alike.
        let promoted = probe(Probe::Promoted, rows)?;
        let agent_mail = probe(
            Probe::AgentMail,
            grapt_inbox::operation_mail::MAX_PRESENTED_ROWS,
        )?;
        let mail = probe(Probe::Mail, rows)?;
        let event = probe(Probe::Event, grapt_session::MAX_HISTORY_ROWS)?;
        let event_bytes = probe(Probe::EventBytes, grapt_session::MAX_HISTORY_DATA_BYTES)?;
        let declared = declared_bytes_over(inbox, &self.session, from, None)?;
        let limits = [
            (tool, rows),
            (permission, rows),
            (identity, rows),
            (terminal, rows),
            (item, MAX_CONTEXT_ITEMS),
            (promoted, rows),
            (agent_mail, grapt_inbox::operation_mail::MAX_PRESENTED_ROWS),
            (mail, rows),
            (event, grapt_session::MAX_HISTORY_ROWS),
            (event_bytes, grapt_session::MAX_HISTORY_DATA_BYTES),
            (declared, MAX_CONTEXT_BYTES),
        ];
        Ok(limits
            .into_iter()
            .filter_map(|(seq, limit)| seq.map(|seq| (seq, limit)))
            .min_by_key(|(seq, _)| seq.value())
            .map(|(seq, limit)| {
                let limit = u64::try_from(limit).unwrap_or(u64::MAX);
                StructuralAdmission {
                    seq,
                    observed: limit.saturating_add(1),
                    limit,
                }
            }))
    }

    /// The request's byte bound, judged on declared lengths before any of the
    /// window's payloads is read.
    pub(super) fn refuse_declared_overflow(
        &self,
        inbox: &Inbox,
        from: Option<grapt_types::EventSeq>,
        until: Option<grapt_types::EventSeq>,
    ) -> Result<(), ContextError> {
        let over = declared_bytes_over(inbox, &self.session, from, until)?;
        match over {
            Some(_) => Err(ContextError::over_bytes(
                Bound::Conversation,
                MAX_CONTEXT_BYTES + 1,
            )),
            None => Ok(()),
        }
    }

    pub(super) fn is_promoted_boundary(
        &self,
        boundary: grapt_types::EventSeq,
    ) -> Result<bool, String> {
        let inbox = lock(&self.inbox);
        boundary::is_promoted(inbox.store(), &self.session, boundary)
    }

    /// Find the newest complete-history boundary before a failed window.
    pub(super) fn preceding_summary_cut(
        &self,
        before: Option<grapt_types::EventSeq>,
    ) -> Result<Option<grapt_types::EventSeq>, String> {
        let inbox = lock(&self.inbox);
        let retained =
            retained::RetainedContext::load(&inbox, &self.artifacts, &self.session, &self.input)?;
        let from = self.read_from(retained.as_ref());
        let assistant =
            boundary::latest_assistant_opening(inbox.store(), &self.session, from, before)?;
        let current = inbox
            .promoted_input(&self.session, &self.input)
            .map_err(|error| error.to_string())?;
        let current = grapt_types::EventSeq::new(current.seq);
        let current = (from.is_none_or(|from| current >= from)
            && current.value() < before.map_or(i64::MAX as u64, |before| before.value()))
        .then_some(current);
        Ok(assistant.into_iter().chain(current).max())
    }

    /// The structural boundary as a summary window may end at it. A bound
    /// inside an assistant exchange, past one of its calls but not past that
    /// call's output, would keep the call and lose the output (SC-REL-08), so
    /// it moves back to the exchange's opening and the whole exchange stays in
    /// the exact retained history. Any other bound is returned unchanged.
    pub(super) fn summary_boundary(&self) -> Result<Option<grapt_types::EventSeq>, String> {
        let Some(bound) = self.structural_boundary()? else {
            return Ok(None);
        };
        let inbox = lock(&self.inbox);
        let retained =
            retained::RetainedContext::load(&inbox, &self.artifacts, &self.session, &self.input)?;
        let from = self.read_from(retained.as_ref());
        let opening = boundary::split_exchange_opening(inbox.store(), &self.session, from, bound)?;
        Ok(Some(opening.unwrap_or(bound)))
    }
}

/// The first row at which the content a request over `[from, until)` reads
/// passes [`MAX_CONTEXT_BYTES`], charging every tool argument, tool output,
/// failure detail and text payload at its declared length up to the
/// per-artifact text bound, and every human or synthetic input at its text.
/// It reads no artifact: a window past it is refused before its payloads are
/// held, where the request's own byte bound applied only after.
fn declared_bytes_over(
    inbox: &Inbox,
    session: &SessionId,
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
) -> Result<Option<grapt_types::EventSeq>, String> {
    boundary::declared_bytes_over(
        inbox.store(),
        session,
        from,
        until,
        MAX_CODEX_TOOL_TEXT_BYTES,
        MAX_CONTEXT_BYTES,
    )
}

/// Every structural bound must mirror the read it guards; the agent-mail
/// read lives in another crate, so its equality is checked here.
const _: () = assert!(grapt_inbox::operation_mail::MAX_PRESENTED_ROWS == MAX_CONTINUATION_ROWS);
/// A compaction reads its whole window through the recent projection, which
/// must hold every row the history fold it summarizes may hold.
const _: () = assert!(grapt_session::MAX_HISTORY_ROWS < recent::MAX_RECENT_WINDOW_ROWS);
