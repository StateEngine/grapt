//! Install a durable tail using the shared history and terminal identity policy.
use super::*;

/// Whether an install carries the previous view's live facts (a legacy
/// re-read) or publishes a presentation derived from a typed observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Carry {
    Legacy,
    Observed,
}

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
        self.commit_history(session, view, tail, older, unsupported, Carry::Legacy);
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
        carry: Carry,
    ) {
        // An observed install already derived its presentation from the
        // session's typed observation; nothing live is carried over it.
        let legacy = carry == Carry::Legacy;
        // A live tail is not lifetime accounting: the weighted history share
        // is reportable only once the walk reached the start (review R2).
        view.cache
            .history_complete(older == history::OlderHistory::Complete);
        // A LIVE-OBSERVED INTERRUPT IS CARRIED AS HISTORY, NOT AS THE OUTCOME.
        //
        // Durable history cannot say a turn was interrupted — the frozen finish
        // vocabulary has no member for it — so the observation is worth
        // keeping. What it must NOT do is become the CURRENT outcome.
        //
        // An earlier revision copied the interrupted terminal into the rebuilt
        // view whenever that rebuild had no execution id. Every rebuild has no
        // execution id: `apply_durable` sets `execution_id: None` for every
        // assistant finish, including a later successful stop. So the guard
        // matched always, and an interrupt observed on execution A would be
        // resurrected over execution B's genuine success or failure after a
        // disconnect. Absence cannot establish sameness.
        //
        // The protocol offers no way to correlate a live execution with a rebuilt
        // terminal, so this does not pretend to: durable truth keeps authority over the
        // current outcome, and the observation is presented separately, naming the
        // execution it belongs to. QUIESCENCE IS LIVE-ONLY TRUTH: no durable record
        // carries it, so a rebuild that settled nothing has not contradicted the newest
        // quiescence fact this client was told — a re-list cannot carry "busy", and the
        // re-subscription a stream cut opens re-sends the current status, so the carried
        // observation is the newest one this client holds. When the rebuild DOES carry a
        // terminal, that terminal's silence is not a fresh fact: an idle observation
        // survives it only when strictly newer than the terminal the previous view
        // carried, and a busy observation survives it only when made after that terminal
        // entered the installed history — the next preparation's busy outlives the relist
        // of the preceding execution's terminal, and a turn's own busy yields to its
        // terminal. A client that never observed the session has no previous view and
        // stays unreported.
        //
        // WHICH terminal is new is decided by IDENTITY: the tail's newest lifecycle
        // record, when it settled, against the terminal the previous view held. A count
        // of user rows folded cannot decide it once a read covers a window — backfill
        // moves the count without any new turn — and a newest page need hold no user row
        // at all. A record the client already holds is no new word; a tail ending on a
        // user row or an unsettled step is a turn in flight and settles nothing.
        let held = self
            .views
            .get(session)
            .and_then(|previous| previous.held_terminal.clone());
        let in_flight = tail.is_none();
        let new_terminal = tail
            .as_ref()
            .is_some_and(|tail| held.as_ref() != Some(tail));
        view.held_terminal = tail.or(held);
        if new_terminal {
            self.quiescence_terminal_epoch
                .insert(session.clone(), self.quiescence_gen);
        }
        if let Some(previous) = self.views.get(session) {
            view.queued = previous.queued.clone();
            view.interrupted_observed = previous.interrupted_observed.clone();
            // A durable relist does not contradict this independent live inbox
            // fact; stream disconnect invalidates it before rebuilding.
            view.mail_attention = previous.mail_attention;
            // Stream loss clears the continuity level, so any value still held
            // here came from this connection's hydration or a live frame.
            view.continuity_attention = previous.continuity_attention;
            if legacy && previous.idle.is_some() {
                let carry = match &view.execution {
                    // The rebuild settled nothing: nothing durable
                    // contradicts the newest quiescence fact this client
                    // holds, and a re-list cannot carry a quiescence — the
                    // fact stands.
                    crate::projection::ExecutionState::Idle => true,
                    // The tail ends on a turn in flight: an older terminal
                    // further down the page is not a new word either.
                    _ if in_flight => true,
                    // The rebuild carries a terminal.
                    _ => match previous.idle {
                        // An IDLE fact is a stamped fact: an observation
                        // strictly newer than the stamped terminal the
                        // previous view carried outlives a relist, and an
                        // unstamped terminal contradicts nothing.
                        Some(true) => match (previous.idle_created, previous.terminal_created) {
                            (Some(fact), Some(terminal)) => fact > terminal,
                            (_, None) => true,
                            _ => false,
                        },
                        // A BUSY fact yields to a terminal newer than its
                        // own observation: a terminal this install newly
                        // carries is the newest word, and every live fact
                        // this client holds was observed before this read.
                        // Against a terminal the client already holds, only
                        // a live observation made after that terminal
                        // entered the history survives the relist.
                        Some(false) => {
                            if new_terminal {
                                false
                            } else {
                                self.quiescence_fact_epoch
                                    .get(session)
                                    .zip(self.quiescence_terminal_epoch.get(session))
                                    .is_some_and(|(fact, terminal)| fact > terminal)
                            }
                        }
                        None => false,
                    },
                };
                if carry {
                    if view.feedback.is_none()
                        && previous
                            .feedback
                            .as_ref()
                            .is_some_and(crate::projection::feedback::Feedback::transient)
                    {
                        view.feedback = previous.feedback.clone();
                    }
                    view.idle = previous.idle;
                    view.idle_created = previous.idle_created;
                    view.idle_seq = previous.idle_seq;
                    view.terminal_created = previous.terminal_created;
                    view.terminal_seq = previous.terminal_seq;
                } else {
                    // The fact yielded to the terminal: no observation is
                    // owed on its behalf.
                    self.quiescence_fact_epoch.remove(session);
                }
            }
            // A RUNNING EXECUTION IS A LIVE-ONLY FACT, and a rebuild that
            // settled nothing has not contradicted it.
            //
            // This is NOT the resurrection the paragraph above refuses. That
            // one repainted a TERMINAL the durable record could not confirm.
            // This carries forward a still-in-flight execution when durable
            // truth offered no terminal at all — a re-list cannot carry
            // "running", so dropping it reports the turn as idle while the
            // product is still working.
            //
            // The moment the rebuild DOES produce a terminal it wins, because
            // then durable truth has spoken about this turn and the live state
            // is stale.
            if legacy
                && matches!(view.execution, crate::projection::ExecutionState::Idle)
                && matches!(
                    previous.execution,
                    crate::projection::ExecutionState::Running { .. }
                )
            {
                view.execution = previous.execution.clone();
            }
            if legacy
                && matches!(
                    view.execution,
                    crate::projection::ExecutionState::Running { .. }
                )
                && view.execution == previous.execution
            {
                view.streamed_text_bytes = previous.streamed_text_bytes;
            }
        }
        // After any carry: a running execution with an unknown estimate counts
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
        // A snapshot installs at its committed position P; a legacy read at 0.
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
