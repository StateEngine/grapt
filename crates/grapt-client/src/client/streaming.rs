//! Apply live frames and refresh their durable session projection.
use super::resume::Outcome;
use super::*;

impl<C: Connection> Client<C> {
    pub fn service_stop_notice(&self) -> Option<&str> {
        self.service_stop_notice.as_deref()
    }

    /// Retain the loss notice even if durable recovery completes in this pump.
    pub fn stream_gap_notice(&self) -> Option<&'static str> {
        (self.diagnostics.stream_overflows > 0).then_some(
            if self.cursor.state().is_live() && self.connection.subscribed() {
                "Live event gap detected; durable history refreshed. Live-only details may be missing."
            } else {
                "Live event gap detected; reloading durable history. Live-only details may be missing."
            },
        )
    }

    fn stream_overflowed(&mut self) {
        self.cursor.buffer_overflowed();
        self.diagnostics.stream_overflows = self.diagnostics.stream_overflows.saturating_add(1);
        self.live_data_refused();
    }

    /// Record that live data was refused and durable truth must be re-read.
    ///
    /// Every gap path funnels through here so the notice means "recovering from
    /// ANY gap", and so the flag is raised in exactly one place. Raising it per
    /// call site is how three paths came to recover silently while the notice
    /// stayed down.
    pub(super) fn live_data_refused(&mut self) -> bool {
        // Nor can a resume continue from before it. A pending one is
        // abandoned first; the read below is its recovery.
        self.drop_resume();
        self.gap_recovery_pending = true;
        self.diagnostics.gaps_observed = self.diagnostics.gaps_observed.saturating_add(1);
        let changed = self.invalidate_output_progress();
        self.request_history();
        changed
    }

    /// Whether live data was refused and the durable baseline has not been
    /// re-read since.
    ///
    /// The one fact a gap notice may be derived from. `stream_overflows` counts
    /// history and never returns to zero, so deriving the notice from it made
    /// every later reconnect re-announce an overflow that had already healed.
    pub fn gap_recovery_pending(&self) -> bool {
        self.gap_recovery_pending
    }
    fn invalidate_output_progress(&mut self) -> bool {
        let mut changed = false;
        for view in self.views.values_mut() {
            changed |= view.streamed_text_bytes.take().is_some();
        }
        changed
    }

    /// Pump the stream once: read whatever arrived, apply what is applicable,
    /// and reconcile if the stream told us we no longer have everything.
    ///
    /// Returns whether anything changed.
    pub fn pump(&mut self) -> Result<bool, ClientError> {
        self.pump_at(self.recovery.started.elapsed())
    }

    pub fn pump_at(&mut self, now: std::time::Duration) -> Result<bool, ClientError> {
        // A refused service is asked nothing, so no owner below advances.
        self.refused()?;
        let before = self.connection_status();
        // Before any frame: a certificate after the deadline is never read.
        let mut changed = self.expire_resume(now);
        let mut tool_refresh = std::collections::BTreeSet::new();
        let mut terminal_refresh = false;
        let mut applied = 0usize;
        loop {
            // CHECK THE BUDGET BEFORE CONSUMING.
            //
            // The previous revision polled a frame, incremented the counter,
            // and then broke when the counter exceeded the bound — so the
            // frame that tripped the bound had already been removed from the
            // connection and was silently dropped. A continuation test proved
            // that LATER frames still arrived, which is a weaker claim: it
            // cannot protect a final frame or a live-only one, because there
            // is no later frame to observe.
            //
            // Yielding here instead leaves the untaken frame queued, so the
            // bound costs latency and never content.
            //
            // While a resume is pending exactly one frame, its answer, passes
            // the queued-read blockers; answering ends the pending state, so
            // they hold every later frame as on a client never cut.
            let answering = self.resume_pending();
            if (!answering
                && ((self.history_blocks_stream() && !self.waiting_for_mode())
                    || self.promoted_input_pending()))
                || applied >= self.bounds.max_pending_events
            {
                break;
            }
            let frame = match self.connection.poll_frame() {
                Ok(frame) => frame,
                Err(error) => {
                    self.poll_failed();
                    self.invalidate_output_progress();
                    return Err(error.into());
                }
            };
            let id = match &frame {
                FramePoll::Positioned { id, .. } => Some(id.clone()),
                _ => None,
            };
            let positioned = id.is_some();
            match frame {
                FramePoll::Idle => break,
                FramePoll::Closed | FramePoll::Gap => {
                    changed |=
                        self.stream_ended(frame == FramePoll::Gap, before, now, &mut tool_refresh);
                    break;
                }
                FramePoll::Frame(payload) | FramePoll::Positioned { data: payload, .. } => {
                    if !self.latch_mode(&payload, positioned)? {
                        // A frame of the other position class: the stream broke
                        // its contract. Retire it; the reconnect subscribes afresh.
                        self.connection.retire_stream();
                        self.positions.reset();
                        self.stream_overflowed();
                        changed = true;
                        break;
                    }
                    // `FramePoll::Frame` is already ONE complete `data:`
                    // payload by contract — SSE framing is the HTTP
                    // transport's job, and `SseReader` is the tool it uses.
                    // Re-parsing here would make the embedded transport carry
                    // framing it does not have.
                    if payload.len() > self.bounds.max_frame_bytes {
                        // The byte bound, applied before the payload is
                        // decoded or retained. An oversized frame means the
                        // client no longer has everything, which is the same
                        // fact a dropped connection carries.
                        self.stream_overflowed();
                        changed = true;
                        break;
                    }
                    // The answer is consumed, never absorbed: an
                    // uninterrupted subscriber never saw it.
                    if answering {
                        changed |= self.answer_resume(id.as_deref(), &payload);
                        continue;
                    }
                    applied = applied.saturating_add(1);
                    // A certified replay's frames this client already holds.
                    if let Some(refused) = self.replayed(id.as_deref()) {
                        changed |= refused;
                        continue;
                    }
                    changed |=
                        self.absorb_framed(&payload, id, &mut tool_refresh, &mut terminal_refresh)?;
                    // Hydrate before consuming a subsequent execution's live
                    // text, which may not yet exist in durable history.
                    if terminal_refresh {
                        break;
                    }
                }
            }
        }

        if terminal_refresh {
            // A terminal requests the same bounded history owner as recovery.
            // Keep the subscription and pause later live frames until install.
            self.request_history();
        }
        changed |= self.advance_owners(now, before)?;
        let mut hints = !tool_refresh.is_empty();
        if !terminal_refresh && !self.history_blocks_stream() && self.cursor.state().is_live() {
            hints = false;
            match self.refresh_tool_rows(tool_refresh) {
                Ok(updated) => changed |= updated,
                Err(error) if error.is_transient() => {
                    self.drop_resume();
                    self.cancel_tool_refresh();
                    self.invalidate_output_progress();
                    self.cursor.stream_closed();
                    self.request_history();
                    changed = true;
                }
                Err(error) => return Err(error),
            }
        }
        self.commit_frontier(hints);
        changed |= before != self.connection_status();
        Ok(changed)
    }

    /// After the frames: the queued owners advance, recovery runs, and going
    /// Live is noted. While a resume is pending no queued owner starts: each
    /// keeps its queued state and starts once on the pump after the answer.
    fn advance_owners(
        &mut self,
        now: std::time::Duration,
        before: recovery::ConnectionStatus,
    ) -> Result<bool, ClientError> {
        let mut changed = false;
        if !self.resume_pending() {
            changed |= self.advance_preset();
        }
        changed |= self.recover(now)?;
        if !self.resume_pending() {
            // Older history goes last, behind every foreground read, and never
            // holds back a live frame: the frame loop above does not wait on it.
            changed |= self.advance_backfill();
            changed |= self.advance_catalog();
            changed |= self.advance_commands();
            changed |= self.advance_mcp_catalog();
        }
        if before != recovery::ConnectionStatus::Live
            && self.connection_status() == recovery::ConnectionStatus::Live
        {
            self.went_live();
        }
        Ok(changed)
    }

    /// Absorb one frame and move the frontier's suffix by its outcome; an
    /// error leaves no frontier behind.
    fn absorb_framed(
        &mut self,
        payload: &str,
        id: Option<String>,
        tool_refresh: &mut std::collections::BTreeSet<crate::wire::ToolCallWireId>,
        terminal_refresh: &mut bool,
    ) -> Result<bool, ClientError> {
        match self.absorb(payload, tool_refresh, terminal_refresh) {
            Ok((changed, outcome)) => {
                self.frame_absorbed(id, outcome);
                Ok(changed)
            }
            Err(error) => {
                self.drop_resume();
                Err(error)
            }
        }
    }

    /// The stream ended, cut (`Closed`) or refused (`Gap`). True when that
    /// changed what is shown.
    fn stream_ended(
        &mut self,
        gap: bool,
        before: recovery::ConnectionStatus,
        now: std::time::Duration,
        hints: &mut std::collections::BTreeSet<crate::wire::ToolCallWireId>,
    ) -> bool {
        // The next stream proves its own mode; the service is re-probed.
        self.positions.reset();
        let resuming = if gap {
            self.stream_overflowed();
            false
        } else {
            self.cursor.stream_closed();
            self.stream_cut(now, hints)
        };
        // A stream cut invalidates the live-only quiescence facts it carried:
        // the same client must not report an idle it observed on a connection
        // that is now dead — a restarted server that sends no fresh status
        // leaves the footer at the honest unreported. Durable transcript and
        // terminals stand; the re-subscription's hydration re-establishes
        // truth. A pending resume holds them aside instead, and defers the
        // history read until its first frame answers.
        if !resuming {
            for view in self.views.values_mut() {
                view.invalidate_live_quiescence();
            }
            self.activity.clear();
            self.observations.clear();
            // A newly lost subscription needs fresh history even if
            // its previous terminal hydrated within the cooldown.
            if before == recovery::ConnectionStatus::Live && !gap {
                self.request_history();
            }
        }
        self.diagnostics.stream_closes = self.diagnostics.stream_closes.saturating_add(1);
        gap
    }

    /// Decode one frame and route it through the cursor into the projection.
    /// Returns whether anything changed, and the frame's [`Outcome`]: every
    /// return below names its class.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn absorb(
        &mut self,
        payload: &str,
        tool_refresh: &mut std::collections::BTreeSet<crate::wire::ToolCallWireId>,
        terminal_refresh: &mut bool,
    ) -> Result<(bool, Outcome), ClientError> {
        let Ok(envelope) = EventEnvelope::decode(payload) else {
            // A positions stream's session-less resync (it carries no event
            // id): the stream is about to end and the session must be re-read.
            if super::snapshot::is_stream_resync(payload) {
                return Ok((self.live_data_refused(), Outcome::Refused));
            }
            // Global forms have a Form aggregate and sessionID="global", so
            // they must never enter the Session cursor or its watermarks.
            if self.form_location().is_some_and(|location| {
                crate::wire::global_form_changed(payload, &location.directory)
            }) {
                self.location_forms_dirty = true;
                return Ok((true, Outcome::Reads));
            }
            // A frame the frozen envelope refuses. The client does not render
            // it and does not let it advance anything.
            return Ok((self.invalidate_output_progress(), Outcome::Ignored));
        };
        // Catalog metadata is refreshed by the host, outside the conversation
        // projection and its durable watermarks.
        if envelope.kind_raw == "session.title.changed"
            && envelope.created.is_some()
            && envelope.durable.is_none()
            && envelope.session_id.is_some()
            && envelope
                .data
                .as_object()
                .is_some_and(|object| object.len() == 1)
        {
            return Ok((false, Outcome::Ignored));
        }
        if envelope.kind_raw == "server.connected" {
            self.service_stop_notice = None;
            self.commands_changed();
        }
        if envelope.kind_raw == "skills.changed" {
            let selected = self
                .form_location()
                .is_some_and(|location| crate::wire::skills_changed(payload, &location.directory));
            return Ok((selected && self.commands_changed(), Outcome::Reads));
        }
        if envelope.kind_raw == "server.stopping" {
            if envelope.created.is_some()
                && envelope.durable.is_none()
                && envelope.session_id.is_none()
                && envelope
                    .data
                    .as_object()
                    .is_some_and(|object| object.len() == 1)
            {
                if let Some(
                    reason @ ("serve-window-expired" | "service-stopped" | "terminal-closed"),
                ) = envelope.data["reason"].as_str()
                {
                    self.service_stop_notice = Some(format!(
                        "Service stopped: {reason}; restart the service to continue. Draft kept."
                    ));
                    return Ok((true, Outcome::Applied));
                }
            }
            return Ok((false, Outcome::Ignored));
        }
        // This global advisory never enters Session history or its cursor.
        if matches!(
            envelope.kind_raw.as_str(),
            "mcp.catalog.changed" | "mcp.resource.changed"
        ) {
            let resources = envelope.kind_raw == "mcp.resource.changed";
            let changed = envelope.created.is_some()
                && envelope.durable.is_none()
                && envelope.session_id.is_none()
                && envelope
                    .data
                    .as_object()
                    .is_some_and(|object| object.len() == 1)
                && envelope.data["scope"].as_str().is_some_and(|scope| {
                    scope.len() == 64
                        && scope.bytes().all(|byte| byte.is_ascii_hexdigit())
                        && if resources {
                            self.mcp_resources_changed(scope)
                        } else {
                            self.mcp_catalog_changed(scope)
                        }
                });
            return Ok((changed, Outcome::Reads));
        }
        // Session form facts also decode as unknown conversation members.
        // Route them before the cursor so discovery cannot depend on a decode
        // failure or advance the conversation's durable watermark.
        if matches!(
            envelope.kind_raw.as_str(),
            "form.created" | "form.replied" | "form.cancelled" | "form.suspended"
        ) {
            let changed = self
                .selected
                .as_ref()
                .is_some_and(|session| crate::wire::session_form_changed(payload, session));
            self.raise_session_forms(changed);
            return Ok((changed, Outcome::Reads));
        }
        self.diagnostics.frames_decoded = self.diagnostics.frames_decoded.saturating_add(1);
        // A positions stream judges a typed state against the session's newest
        // presentation fact before any effect.
        match self.admit_presentation(&envelope) {
            Ok(super::observation::Admission::Handled(changed)) => {
                return Ok((changed, Outcome::Applied))
            }
            Ok(super::observation::Admission::Ordinary) => {}
            Err(_) => return Ok((self.live_data_refused(), Outcome::Refused)),
        }
        // A lifecycle frame against the pair, decided once before any effect;
        // while a pair exists, only it decides what is shown.
        let lifecycle = self.lifecycle(&envelope);
        let activity_changed = lifecycle == super::observation::Lifecycle::NotOwned
            && self.observe_activity(&envelope);
        // This cursor's baseline comes from the selected session's history.
        // Other sessions cannot establish a gap in that history.
        if envelope
            .session_id
            .as_ref()
            .is_some_and(|session| self.selected.as_ref() != Some(session))
        {
            return Ok(match lifecycle {
                super::observation::Lifecycle::Newer(seq) => {
                    (self.commit_lifecycle(&envelope, seq), Outcome::Applied)
                }
                super::observation::Lifecycle::Refused => {
                    (self.live_data_refused(), Outcome::Refused)
                }
                super::observation::Lifecycle::Superseded => {
                    self.record_superseded(&envelope);
                    (false, Outcome::Applied)
                }
                super::observation::Lifecycle::NotOwned => (activity_changed, Outcome::Applied),
            });
        }
        if envelope.member == Some(crate::contract::EventMember::ToolProgress) {
            if let Ok(data) =
                serde_json::from_value::<grapt_api::u3a::ToolProgressData>(envelope.data.clone())
            {
                if data
                    .metadata()
                    .calls()
                    .iter()
                    .any(|entry| entry.message().is_some())
                {
                    if let Some(view) = self
                        .views
                        .get_mut(&crate::wire::session_id(data.session().as_str())?)
                    {
                        let call = crate::wire::tool_call_id(data.call().as_str())?;
                        if view
                            .slices
                            .iter()
                            .any(|slice| slice.closed && slice.tool_id.as_ref() == Some(&call))
                        {
                            return Ok((false, Outcome::Ignored));
                        }
                        if view.record_tool_progress(&data, self.bounds) {
                            return Ok((true, Outcome::Applied));
                        }
                    }
                }
            }
        }
        if envelope
            .member
            .is_some_and(crate::contract::EventMember::is_tool_hint)
        {
            if envelope.session_id.as_ref() == self.selected.as_ref() {
                if let Some(call) = envelope
                    .data
                    .get("callID")
                    .and_then(serde_json::Value::as_str)
                {
                    tool_refresh.insert(crate::wire::tool_call_id(call)?);
                }
            }
            return Ok((false, Outcome::Reads));
        }
        match self.cursor.admit(&envelope) {
            Admission::Apply | Admission::Advisory => {
                self.diagnostics.frames_applied = self.diagnostics.frames_applied.saturating_add(1);
                // Refused: the same recovery as the reducer's own refusal.
                // Superseded by a newer presentation fact: admitted and kept
                // as history, never folded into what is shown.
                match lifecycle {
                    super::observation::Lifecycle::Refused => {
                        return Ok((self.live_data_refused(), Outcome::Refused));
                    }
                    super::observation::Lifecycle::Superseded => {
                        self.record_superseded(&envelope);
                        if envelope.member == Some(crate::contract::EventMember::ExecutionStarted) {
                            return Ok((true, Outcome::Applied));
                        }
                        *terminal_refresh = true;
                        return Ok((true, Outcome::Reads));
                    }
                    _ => {}
                }
                let Some(session) = envelope.session_id.clone() else {
                    return Ok((false, Outcome::Applied));
                };
                let bounds = self.bounds;
                let view = self
                    .views
                    .entry(session.clone())
                    .or_insert_with(|| SessionView::new(&session));
                match view.apply(&envelope, bounds) {
                    Ok(_) => {
                        let mut outcome = Outcome::Applied;
                        if matches!(
                            envelope.member,
                            Some(
                                crate::contract::EventMember::ExecutionSucceeded
                                    | crate::contract::EventMember::ExecutionFailed
                                    | crate::contract::EventMember::ExecutionInterrupted
                            )
                        ) {
                            *terminal_refresh = true;
                            outcome = Outcome::Reads;
                        }
                        if envelope.kind_raw == "session.input.promoted" {
                            let input: grapt_api::u3a::InputPromotedData =
                                serde_json::from_value(envelope.data.clone()).map_err(|error| {
                                    crate::wire::WireError::Codec {
                                        reason: error.to_string(),
                                    }
                                })?;
                            self.request_promoted_input(input.input_id().get().clone());
                            outcome = Outcome::Reads;
                        }
                        if envelope.kind_raw == "session.input.admitted" {
                            // Queuing input must not replace an in-flight
                            // assistant's live text with a durable snapshot.
                            self.request_queued_inputs(&session);
                            outcome = Outcome::Reads;
                        }
                        // What is shown comes from the session's typed pair,
                        // never from the frame's fold; a newer lifecycle frame
                        // replaces the pair only now that the reducer accepted it.
                        if let super::observation::Lifecycle::Newer(seq) = lifecycle {
                            self.commit_lifecycle(&envelope, seq);
                        } else {
                            self.present(&session);
                        }
                        Ok((true, outcome))
                    }
                    Err(_) => {
                        // A transcript defect. Reconciling from durable truth
                        // is the honest recovery: the client cannot repair a
                        // malformed stream, but it can go and re-read.
                        self.cursor.buffer_overflowed();
                        Ok((self.live_data_refused(), Outcome::Refused))
                    }
                }
            }
            Admission::Duplicate => {
                self.diagnostics.duplicates_ignored =
                    self.diagnostics.duplicates_ignored.saturating_add(1);
                Ok((false, Outcome::Ignored))
            }
            Admission::Deferred => {
                self.diagnostics.frames_deferred =
                    self.diagnostics.frames_deferred.saturating_add(1);
                Ok((false, Outcome::Ignored))
            }
            Admission::Gap { reason } => {
                let _ = reason;
                Ok((self.live_data_refused(), Outcome::Refused))
            }
        }
    }
}
