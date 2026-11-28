//! Install a durable tail using the shared history and terminal identity policy.
use super::*;

impl<C: Connection> Client<C> {
    #[cfg(test)]
    pub(super) fn install_history(
        &mut self,
        session: &SessionWireId,
        view: SessionView,
        tail: Option<(crate::wire::MessageWireId, crate::projection::Outcome)>,
        older: history::OlderHistory,
        unsupported: usize,
    ) -> Result<(), ClientError> {
        self.commit_history(session, view, tail, older, unsupported);
        Ok(())
    }

    /// Install a finished view with the shared terminal and quiescence
    /// policy. Infallible, so it can be the last step of an atomic install.
    pub(super) fn commit_history(
        &mut self,
        session: &SessionWireId,
        mut view: SessionView,
        tail: Option<(crate::wire::MessageWireId, crate::projection::Outcome)>,
        older: history::OlderHistory,
        unsupported: usize,
    ) {
        // A live tail is not lifetime accounting: the weighted history share
        // is reportable only once the walk reached the start (review R2).
        view.cache
            .history_complete(older == history::OlderHistory::Complete);
        // A LIVE-OBSERVED INTERRUPT IS CARRIED AS HISTORY, NOT AS THE OUTCOME.
        //
        // Durable history cannot say a turn was interrupted — the frozen finish
        // vocabulary has no member for it — so the observation is worth
        // keeping. What it must NOT do is become the CURRENT outcome: absence
        // of an execution id cannot establish sameness, so durable truth keeps
        // authority over the current outcome and the observation is presented
        // separately, naming the execution it belongs to. Quiescence and the
        // running execution are not carried at all: an install derives them
        // from the session's typed state.
        // The tail's newest settled lifecycle record replaces the terminal the
        // previous view held; a tail that settled nothing keeps it.
        let held = self
            .views
            .get(session)
            .and_then(|previous| previous.held_terminal.clone());
        view.held_terminal = tail.or(held);
        if let Some(previous) = self.views.get(session) {
            view.queued = previous.queued.clone();
            view.interrupted_observed = previous.interrupted_observed.clone();
            // A durable relist does not contradict this independent live inbox
            // fact; stream disconnect invalidates it before rebuilding.
            view.mail_attention = previous.mail_attention;
            // Stream loss clears the continuity level, so any value still held
            // here came from this connection's hydration or a live frame.
            view.continuity_attention = previous.continuity_attention;
        }
        // A running execution with an unknown estimate counts
        // the live prose admitted from this install on.
        view.arm_live_output();
        // A closed stream can mean a permission was cancelled. Reload its
        // durable list too: the transcript's terminal does not remove a cached
        // ask, and a request that is still pending must remain visible.
        self.views.insert(session.clone(), view);
        // A baseline for another scope, or one read while a resume waits for
        // its first frame, leaves nothing a resume could continue.
        if self.resume_pending() || self.selected.as_ref() != Some(session) {
            self.drop_resume();
        }
        self.diagnostics.reconciliations = self.diagnostics.reconciliations.saturating_add(1);

        // The tail is always a baseline now. What earlier history is missing
        // is `older`, shown beside the stream state and never a gap; rows
        // this client cannot draw are counted, not treated as missing.
        // Every product install is a snapshot at its committed position P;
        // only a test install carries none and baselines at 0.
        let baseline = self.positions.baseline.take().unwrap_or(0);
        self.cursor.adopt_baseline(session, baseline);
        self.cursor.presentation_unsupported(unsupported);
        // The re-read baseline is what a refused live frame was waiting for, so
        // the gap recovery is over here and nowhere else; the overflow counter
        // stays as history and must not keep re-announcing itself.
        self.gap_recovery_pending = false;
        if matches!(older, history::OlderHistory::Failed { .. }) {
            self.diagnostics.degraded_reconciliations =
                self.diagnostics.degraded_reconciliations.saturating_add(1);
        }
        self.recovery.older = older;
    }
}
