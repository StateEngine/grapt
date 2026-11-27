//! Resuming a positions stream (B3 S3b-3). The client offers its frontier F,
//! the newest frame whose effects and reads it holds, only when it is
//! quiescent at the reconnect: no acquisition in flight on any lane. The
//! service then either certifies that the new stream continues exactly at F
//! or opens a fresh one. Certified, the client ends up as an uninterrupted
//! subscriber would: its live facts were held aside across the cut and come
//! back whole, the frames it already held after F are consumed by id, and no
//! history is read. Every other case abandons the resume through one
//! claim-first transition and takes the fresh path: hydration, a baseline,
//! then the live suffix.
//!
//! Pending state lives only between the cut and the first frame's answer.
//! While it does, the client starts no acquisition; queued work keeps its
//! queued state and starts once, after the answer.
use super::*;
use std::time::Duration;

/// The most frame ids the retained suffix holds: the hub ring's frame bound.
const MAX_SUFFIX: usize = 1024;
/// The longest frame id the retained suffix holds.
const MAX_SUFFIX_ID_BYTES: usize = 128;

/// The id of the newest frame this client holds with every read it started,
/// and the binding of the stream that frame came from.
#[derive(Debug, Clone)]
pub(super) struct Frontier {
    id: String,
    binding: StreamBinding,
}

/// What one absorbed frame did, by the class that decides the frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Live data was refused: the client no longer holds what an
    /// uninterrupted subscriber holds until a baseline is re-read.
    Refused,
    /// Not shown and advancing nothing; a replay ignores it the same way.
    Ignored,
    /// Started a read (a tool hint, forms, skills, MCP, a terminal or an
    /// input); the read's owner holds the frontier until it completes.
    Reads,
    Applied,
}

/// The live facts a cut held aside while a resume is pending, moved rather
/// than cleared: each view's live quiescence, and the client's per-session
/// observation epochs, activity and typed observations.
#[derive(Debug)]
pub(super) struct SuspendedLive {
    views: BTreeMap<SessionWireId, crate::projection::LiveQuiescence>,
    fact_epoch: std::collections::HashMap<SessionWireId, u64>,
    activity: BTreeMap<SessionWireId, activity::Observation>,
    observations: BTreeMap<SessionWireId, observation::Presented>,
}

/// The resume owner. `suffix` is every id absorbed after the committed
/// frontier, in order, and the only list; `seen` counts the entries the
/// current stream has already delivered, so outside a replay it equals the
/// suffix length. `suspended` and `deferred_live` exist only from a cut with
/// a frontier until the first frame's answer.
#[derive(Debug, Default)]
pub(super) struct Resume {
    frontier: Option<Frontier>,
    suffix: Vec<String>,
    seen: usize,
    suspended: Option<SuspendedLive>,
    /// The block that runs on going Live (the preset read and both form
    /// lists) was skipped while the resume was pending.
    deferred_live: bool,
}

impl<C: Connection> Client<C> {
    /// Whether a cut is waiting for the next stream's first frame to answer
    /// a resume: the live facts are held aside, the history read is deferred
    /// and no acquisition starts.
    pub fn resume_pending(&self) -> bool {
        self.resume.suspended.is_some()
    }

    /// The id the next subscribe would offer as `Last-Event-ID`, once a cut
    /// leaves a resume pending.
    pub fn resume_frontier(&self) -> Option<&str> {
        self.resume
            .frontier
            .as_ref()
            .map(|frontier| frontier.id.as_str())
    }

    /// Forget the frontier and the retained suffix.
    fn clear_frontier(&mut self) {
        self.resume.frontier = None;
        self.clear_suffix();
    }

    fn clear_suffix(&mut self) {
        self.resume.suffix.clear();
        self.resume.seen = 0;
    }

    /// Forget the frontier and the suffix. A pending resume is abandoned
    /// first, without a history read: the caller re-reads in its own way.
    pub(super) fn drop_resume(&mut self) {
        self.abandon(false, false);
        self.clear_frontier();
    }

    /// Whether no transport acquisition is in flight on any lane. Queued work
    /// that has not started I/O is not in flight: it survives the cut and
    /// starts once, after the answer.
    fn quiescent(&self) -> bool {
        let history = self
            .recovery
            .history
            .as_ref()
            .is_some_and(|history| history.pending);
        let backfill = self
            .recovery
            .backfill
            .as_ref()
            .is_some_and(|walk| walk.pending);
        !(history
            || backfill
            || self.preset_pending()
            || self.directory_offer
            || self.command_catalog.in_flight()
            || self.catalog_in_flight()
            || self.tool_read_in_flight()
            || self.operator_request_pending()
            || self.worktree_attachment_waiting())
    }

    /// Before each subscribe: offer the frontier only while a resume is
    /// pending and the client is quiescent. Anything else abandons a pending
    /// resume and withdraws any earlier offer.
    pub(super) fn offer_resume(&mut self) {
        let token = self
            .resume
            .frontier
            .as_ref()
            .filter(|_| self.resume_pending() && self.quiescent())
            .map(|frontier| ResumeToken {
                id: frontier.id.clone(),
                binding: frontier.binding.clone(),
            });
        if token.is_some() {
            self.connection.resume_suffix(self.resume.suffix.len());
        } else {
            self.abandon_resume();
        }
        self.connection.resume_from(token);
    }

    /// The one pending-to-fresh transition, with the history read the cut
    /// deferred.
    pub(super) fn abandon_resume(&mut self) {
        self.abandon(true, false);
    }

    /// Claim first, take once: nothing happens unless this call takes the
    /// held facts. Then withdraw the offer, fence an installed stream whose
    /// first frame is unanswered (the claim is the guard: the pending state
    /// is already gone), clear the frontier, release the skipped Live block
    /// and the connection's held refreshes, and read history unless the
    /// caller does. `answered` says the caller is the first frame's answer.
    pub(super) fn abandon(&mut self, history: bool, answered: bool) {
        let Some(skipped) = self
            .resume
            .suspended
            .take()
            .map(|_| std::mem::take(&mut self.resume.deferred_live))
        else {
            return;
        };
        self.connection.resume_from(None);
        if !answered && self.connection.stream_binding().is_some() {
            self.connection.retire_stream();
        }
        self.clear_frontier();
        if skipped {
            self.live_block();
        }
        self.connection.resume_answered(false);
        if history {
            self.request_history();
        }
    }

    /// The connection went Live. A pending resume skips today's block until
    /// its answer: certified, it never runs; abandoned, it runs then.
    pub(super) fn went_live(&mut self) {
        if self.resume_pending() {
            self.resume.deferred_live = true;
        } else {
            self.live_block();
        }
    }

    fn live_block(&mut self) {
        self.refresh_preset_display();
        self.location_forms_dirty = true;
        self.raise_session_forms(true);
    }

    /// The stream closed. The current stream's replay cursor restarts. With a
    /// committed frontier the live facts are held aside, the history read is
    /// deferred, the handshake deadline starts and the pump's tool hints go
    /// to the walk without I/O; a second cut before the answer keeps all of
    /// it. Without a frontier the suffix goes and the caller clears as it
    /// always has (false).
    pub(super) fn stream_cut(
        &mut self,
        now: Duration,
        hints: &mut std::collections::BTreeSet<crate::wire::ToolCallWireId>,
    ) -> bool {
        self.resume.seen = 0;
        if self.resume_pending() {
            return true;
        }
        if self.resume.frontier.is_none() {
            self.clear_suffix();
            return false;
        }
        let views = self
            .views
            .iter_mut()
            .map(|(session, view)| (session.clone(), view.suspend_live_quiescence()))
            .collect();
        self.resume.suspended = Some(SuspendedLive {
            views,
            fact_epoch: std::mem::take(&mut self.quiescence_fact_epoch),
            activity: std::mem::take(&mut self.activity),
            observations: std::mem::take(&mut self.observations),
        });
        self.positions.handshake_since.get_or_insert(now);
        self.defer_tool_hints(std::mem::take(hints));
        true
    }

    /// The first frame of a stream opened while a resume is pending, after
    /// its mode latched; it is consumed either way, never absorbed. Only the
    /// certificate resumes: a positioned `server.connected` whose id is the
    /// offered one, on a stream with the frontier's binding. Anything else
    /// abandons; an answer that is not a `server.connected` is also a gap.
    pub(super) fn answer_resume(&mut self, id: Option<&str>, payload: &str) -> bool {
        let certificate = self.resume.frontier.as_ref().is_some_and(|frontier| {
            self.positions_mode()
                && id == Some(frontier.id.as_str())
                && self.connection.stream_binding().as_ref() == Some(&frontier.binding)
        });
        if certificate && self.cursor.resume_certified() {
            if let Some(held) = self.resume.suspended.take() {
                for (session, facts) in held.views {
                    if let Some(view) = self.views.get_mut(&session) {
                        view.restore_live_quiescence(facts);
                    }
                }
                self.quiescence_fact_epoch = held.fact_epoch;
                self.activity = held.activity;
                self.observations = held.observations;
            }
            self.resume.deferred_live = false;
            self.connection.resume_from(None);
            self.connection.resume_answered(true);
            return true;
        }
        let connected = EventEnvelope::decode(payload)
            .ok()
            .and_then(|envelope| envelope.member)
            == Some(crate::contract::EventMember::ServerConnected);
        self.abandon(connected, true);
        if !connected {
            self.live_data_refused();
        }
        true
    }

    /// A frame of a certified replay: while the stream has not yet delivered
    /// the whole suffix, the next id must be the next one this client holds,
    /// and it is consumed without being absorbed. Any other id is a
    /// divergence, a gap. `None` when the frame is past the suffix.
    pub(super) fn replayed(&mut self, id: Option<&str>) -> Option<bool> {
        let expected = self.resume.suffix.get(self.resume.seen)?;
        if id == Some(expected.as_str()) {
            self.resume.seen = self.resume.seen.saturating_add(1);
            return Some(false);
        }
        Some(self.live_data_refused())
    }

    /// Polling the stream failed. Before the answer that proves nothing, so
    /// a pending resume is abandoned; otherwise the frontier goes, as it does
    /// on any error.
    pub(super) fn poll_failed(&mut self) {
        if self.resume_pending() {
            self.abandon_resume();
        } else {
            self.clear_frontier();
        }
    }

    /// A pending resume is abandoned once the handshake deadline passes: no
    /// later subscribe offers it, a late certificate on an installed stream
    /// can no longer arrive, and history is read the way P1 reads it after an
    /// absent first frame.
    pub(super) fn expire_resume(&mut self, now: Duration) -> bool {
        let expired = self.resume_pending()
            && self.positions.handshake_since.is_some_and(|since| {
                now.saturating_sub(since) >= super::recovery::RECONNECT_RETRY_MAX
            });
        if expired {
            self.abandon_resume();
        }
        expired
    }

    /// A frame was absorbed: a refusal clears the frontier with the suffix;
    /// a new positioned frame joins the suffix once, and a suffix past its
    /// bound drops the resume.
    pub(super) fn frame_absorbed(&mut self, id: Option<String>, outcome: Outcome) {
        if outcome == Outcome::Refused {
            self.clear_frontier();
            return;
        }
        let Some(id) = id else {
            return;
        };
        if self.resume.seen != self.resume.suffix.len() {
            return;
        }
        if self.resume.suffix.len() >= MAX_SUFFIX || id.len() > MAX_SUFFIX_ID_BYTES {
            self.drop_resume();
            return;
        }
        self.resume.suffix.push(id);
        self.resume.seen = self.resume.suffix.len();
    }

    /// Commit the suffix at the end of a pump, only when the client holds
    /// everything an uninterrupted subscriber holds at its last frame: the
    /// stream has delivered the whole suffix, no read a frame started is
    /// outstanding, no refusal awaits a baseline, no history read blocks the
    /// stream, no resume is pending, and the cursor is live. `hints` says
    /// the pump's tool hints were not handed to a walk.
    pub(super) fn commit_frontier(&mut self, hints: bool) {
        if self.resume_pending()
            || self.resume.seen != self.resume.suffix.len()
            || !self.cursor.state().is_live()
            || self.gap_recovery_pending
            || self.history_blocks_stream()
            || self.read_debt(hints)
        {
            return;
        }
        let Some(binding) = self.connection.stream_binding() else {
            return;
        };
        if let Some(id) = self.resume.suffix.pop() {
            self.resume.frontier = Some(Frontier { id, binding });
            self.clear_suffix();
        }
    }

    /// Whether a read some frame started is still outstanding, by its owner.
    fn read_debt(&self, hints: bool) -> bool {
        hints
            || self.tool_refresh.is_some()
            || self.recovery.history.is_some()
            || self.command_catalog.in_flight()
            || self.commands_pending()
            || self.mcp_refresh_needed
            || self.mcp_read.is_some()
            || self.location_forms_dirty
            || self.session_forms_dirty
    }
}
