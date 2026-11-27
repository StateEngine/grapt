//! Publish committed stream content through the bounded live hub.
use super::*;

/// What a refusal for size alone says: the public content, or the frame
/// that would carry it, outgrew its bound. Every frame already queued is
/// still valid, so subscribers drain them before their stream ends.
/// An ended event that would re-carry too much text is sent
/// elided instead (see [`block_events::ended_elided`]).
pub(super) const BOUND_REFUSAL: &str = "public content bound exceeded";

/// Finished reasoning blocks one attempt may hold awaiting capture. The owner
/// bounds their bytes; this bounds their count, refusing past it.
const MAX_PENDING_BLOCKS: usize = 16;

/// Publish `event`, naming a refusal for size alone as [`BOUND_REFUSAL`].
fn bounded<T>(published: Result<T, grapt_api::SseError>) -> Result<(), ExecutionRefusal> {
    published.map(|_| ()).map_err(|error| match error {
        grapt_api::SseError::EventBytes | grapt_api::SseError::FrameBytes => {
            ExecutionRefusal::Run(format!("{BOUND_REFUSAL}: {error}"))
        }
        other => ExecutionRefusal::Run(other.to_string()),
    })
}

impl LiveSink {
    fn publish_tool_progress(&mut self, fact: &DurableFact) -> Result<(), String> {
        use grapt_api::u3a::ToolProgressData;
        let Some(message) = self.message.clone() else {
            return Ok(());
        };
        let data = {
            let inbox = self
                .inbox
                .lock()
                .map_err(|_| "progress inbox lock poisoned")?;
            let parent = match fact {
                DurableFact::NestedToolCalled { parent, .. } => Some(parent.clone()),
                DurableFact::ToolTerminal { call, .. } => {
                    super::super::continuation::progress::parent(
                        inbox.store(),
                        &self.session,
                        call,
                    )?
                }
                _ => None,
            };
            let Some(parent) = parent else {
                return Ok(());
            };
            let Some(metadata) = super::super::continuation::progress::snapshot(
                inbox.store(),
                &self.session,
                &parent,
                true,
            )?
            else {
                return Ok(());
            };
            ToolProgressData::new(
                wire_session(&self.session).map_err(|error| error.to_string())?,
                wire_message(&message).map_err(|error| error.to_string())?,
                WireId::parse(parent.as_str().to_owned()).ok_or("progress parent id invalid")?,
                metadata,
            )
        };
        let event = DogfoodEvent::ToolProgress(LiveEvent::new(
            wire_event(&self.event_id().map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?,
            finite(self.clock.now().as_millis()).map_err(|error| error.to_string())?,
            data,
        ));
        self.hub
            .publish(&event)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    pub(super) fn offer_fragment(
        &mut self,
        fragment: &AttemptFragment,
    ) -> Result<(), ExecutionRefusal> {
        match fragment {
            AttemptFragment::ReasoningDelta { block, text, .. } => {
                self.latched_delta(block.value(), ContentKind::Reasoning, text)
            }
            AttemptFragment::TextDelta { block, text, .. } => {
                self.latched_delta(block.value(), ContentKind::Text, text)
            }
            AttemptFragment::ReasoningClosed { block, .. } => self.hold_reasoning(block.value())?,
            AttemptFragment::TextClosed { block, .. } => {
                if !self.is_refused(block.value()) {
                    self.close_empty_block(block.value())?
                }
            }
            AttemptFragment::ProviderTerminal {
                usage: Some(usage), ..
            } => self.publish_usage(usage.clone())?,
            _ => {}
        }
        Ok(())
    }

    pub(super) fn open_block(
        &mut self,
        position: &grapt_session::CommittedPosition,
        fact: &grapt_event::session_message::ContentBlockOpenedFact,
    ) -> Result<(), ExecutionRefusal> {
        let block = fact.block();
        let kind = match fact.kind() {
            grapt_event::session_message::ContentBlockKind::Text => ContentKind::Text,
            grapt_event::session_message::ContentBlockKind::Reasoning => ContentKind::Reasoning,
        };
        if self.message.as_ref() != Some(fact.message())
            || self.attempt.as_ref() != Some(fact.attempt())
        {
            return Err(ExecutionRefusal::Run(
                "committed block belongs to another message or attempt".into(),
            ));
        }
        if self.blocks.len() >= 2
            || self
                .blocks
                .iter()
                .any(|held| held.block == block || held.kind == kind)
        {
            return Err(ExecutionRefusal::Run(
                "public open-block bound or identity conflict".into(),
            ));
        }
        self.blocks.push(LiveBlock {
            block,
            opening: fact.clone(),
            position: position.clone(),
            kind,
            text: String::new(),
            public: None,
        });
        Ok(())
    }

    pub(super) fn publish_delta(&mut self, block: u32, text: &str) -> Result<(), ExecutionRefusal> {
        if text.is_empty() {
            return Ok(());
        }
        let index = self
            .blocks
            .iter()
            .position(|held| held.block == block)
            .ok_or_else(|| ExecutionRefusal::Run("delta has no opened block".into()))?;
        let message = self.message.clone().ok_or_else(|| {
            ExecutionRefusal::Run("delta has no committed assistant identity".into())
        })?;
        if self.blocks[index].public.is_none() {
            let ordinal = if self.blocks[index].kind == ContentKind::Reasoning {
                self.reasoning_ordinal
            } else {
                self.text_ordinal
            };
            let seq = self.blocks[index].position.seq;
            let event = block_events::started(
                &self.blocks[index].position,
                &self.blocks[index].opening,
                &self.session,
                ordinal,
            )?;
            let identity = provisional_block(&self.blocks[index], ordinal);
            let opened = grapt_api::ProvisionalChange::Open {
                session: &self.session,
                block: &identity,
            };
            bounded(self.hub.publish_provisional(&event, opened))?;
            self.blocks[index].public = Some((seq, ordinal));
            let next = ordinal
                .checked_add(1)
                .ok_or_else(|| ExecutionRefusal::Run("public block ordinals exhausted".into()))?;
            if self.blocks[index].kind == ContentKind::Reasoning {
                self.reasoning_ordinal = next;
            } else {
                self.text_ordinal = next;
            }
        }
        let (_, ordinal) = self.blocks[index]
            .public
            .ok_or_else(|| ExecutionRefusal::Run("block opening was not published".into()))?;
        // Refuse the entire oversize public frame. The committed capture keeps
        // its own truthful disposition; invalidation forces a fresh hydration.
        if text.len()
            > super::super::PUBLIC_CONTENT_BYTES.saturating_sub(self.blocks[index].text.len())
        {
            return Err(ExecutionRefusal::Run(format!(
                "{BOUND_REFUSAL}; hydration required"
            )));
        }
        self.blocks[index].text.push_str(text);
        let delta = LiveEvent::new(
            wire_event(&self.event_id()?)?,
            finite(self.clock.now().as_millis())?,
            SliceDeltaData::new(
                wire_session(&self.session)?,
                wire_message(&message)?,
                ordinal.into(),
                dto_string(text)?,
            ),
        );
        let event = if self.blocks[index].kind == ContentKind::Reasoning {
            DogfoodEvent::ReasoningDelta(delta)
        } else {
            DogfoodEvent::TextDelta(delta)
        };
        // Appended with the offer, so a snapshot holds what subscribers hold.
        let identity = provisional_block(&self.blocks[index], ordinal);
        let appended = grapt_api::ProvisionalChange::Append {
            session: &self.session,
            block: &identity,
            text,
        };
        bounded(self.hub.publish_provisional(&event, appended))
    }

    /// A finished reasoning block whose capture has not committed waits for
    /// it: the owner holds finished reasoning until visible output follows.
    /// A refused block waits as `None`; its latch moves to that place.
    fn hold_reasoning(&mut self, block: u32) -> Result<(), ExecutionRefusal> {
        let held = if self.is_refused(block) {
            self.refused.retain(|(held, _)| *held != block);
            None
        } else {
            let Some(index) = self.blocks.iter().position(|held| held.block == block) else {
                return Ok(());
            };
            if self.blocks[index].text.is_empty() {
                self.blocks.remove(index);
                return Ok(());
            }
            Some(self.blocks.remove(index))
        };
        if self.pending.len() >= MAX_PENDING_BLOCKS {
            return Err(ExecutionRefusal::Run(
                "finished reasoning awaiting capture exceeds its bound".into(),
            ));
        }
        self.pending.push_back(held);
        Ok(())
    }

    pub(super) fn close_empty_block(&mut self, block: u32) -> Result<(), ExecutionRefusal> {
        if let Some(index) = self.blocks.iter().position(|held| held.block == block) {
            let held = self.blocks.remove(index);
            if !held.text.is_empty() {
                return Err(ExecutionRefusal::Run(
                    "block closed without its committed capture notice".into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn publish_capture(
        &mut self,
        position: &grapt_session::CommittedPosition,
        fact: &grapt_event::session_message::ContentAppendedFact,
    ) -> Result<(), ExecutionRefusal> {
        if !matches!(
            fact.item().kind(),
            ContentKind::Text | ContentKind::Reasoning
        ) {
            return Ok(());
        }
        // The owner invokes this synchronously after this exact capture commits.
        // At most one active block per kind exists; the receipt carries the
        // actual canonical ContentOrdinal, independent of provider numbering.
        let index = self
            .blocks
            .iter()
            .position(|held| {
                held.opening.message() == fact.message() && &held.kind == fact.item().kind()
            })
            .ok_or_else(|| {
                ExecutionRefusal::Run("captured content has no active public block".into())
            })?;
        let held = self.blocks.remove(index);
        self.publish_ended(position, fact, held)
    }

    /// End a held reasoning block: the owner commits them in close order.
    fn publish_pending(
        &mut self,
        position: &grapt_session::CommittedPosition,
        fact: &grapt_event::session_message::ContentAppendedFact,
    ) -> Result<(), ExecutionRefusal> {
        match self.pending.pop_front().flatten() {
            Some(held) if held.opening.message() == fact.message() => {
                self.publish_ended(position, fact, held)
            }
            Some(_) => Err(ExecutionRefusal::Run(
                "held reasoning belongs to another message".into(),
            )),
            None => Ok(()),
        }
    }

    fn publish_ended(
        &mut self,
        position: &grapt_session::CommittedPosition,
        fact: &grapt_event::session_message::ContentAppendedFact,
        held: LiveBlock,
    ) -> Result<(), ExecutionRefusal> {
        let Some((_, ordinal)) = held.public else {
            return Ok(());
        };
        let event = block_events::ended(position, fact, &self.session, &held)?;
        let identity = provisional_block(&held, ordinal);
        let closed = grapt_api::ProvisionalChange::Close {
            session: &self.session,
            block: &identity,
        };
        match self.hub.publish_provisional(&event, closed) {
            // The block's whole text, which its deltas already carried,
            // outgrew the event bound; nothing was sent. Send its length and
            // digest instead of ending every stream.
            Err(grapt_api::SseError::EventBytes) => bounded(self.hub.publish_provisional(
                &block_events::ended_elided(position, fact, &self.session, &held)?,
                closed,
            )),
            published => bounded(published),
        }
    }

    /// A refused or discarded block's text is no longer offered live; only
    /// that block leaves the snapshot, never its healthy siblings.
    fn withdraw_provisional(&self, held: &LiveBlock) {
        if let Some((_, ordinal)) = held.public {
            let identity = provisional_block(held, ordinal);
            let withdrawn = grapt_api::ProvisionalChange::Close {
                session: &self.session,
                block: &identity,
            };
            let _withdrawn = self.hub.change_provisional(withdrawn);
        }
    }

    fn is_refused(&self, block: u32) -> bool {
        self.refused.iter().any(|(held, _)| *held == block)
    }

    /// A delta for a refused block is dropped; a delta that fails refuses
    /// its block, which invalidates subscribers exactly once.
    pub(super) fn latched_delta(&mut self, block: u32, kind: ContentKind, text: &str) {
        if self.is_refused(block) {
            return;
        }
        if let Err(error) = self.publish_delta(block, text) {
            self.refuse_block(block, kind, error);
        }
    }

    pub(super) fn refuse_block(&mut self, block: u32, kind: ContentKind, error: ExecutionRefusal) {
        if let Some(index) = self.blocks.iter().position(|held| held.block == block) {
            let refused = self.blocks.remove(index);
            self.withdraw_provisional(&refused);
        }
        // Keyed by kind: at most one block per kind is open, so a later
        // refusal of the kind replaces the earlier entry. An empty block closes
        // with no capture to settle it, so this is what bounds the latch.
        self.refused.retain(|(_, held)| held != &kind);
        self.refused.push((block, kind));
        self.publication_refusal(error);
    }

    pub(super) fn publication_refusal(&self, error: ExecutionRefusal) {
        let reason = error.to_string();
        let why = format!(
            "session {}: live content publication refused: {reason}",
            self.session
        );
        // A refusal for size alone leaves every queued frame valid: drain them,
        // then end. Any other refusal makes the projection itself suspect, so
        // streams close at once.
        if reason.contains(BOUND_REFUSAL) {
            self.hub
                .reconcile_because(grapt_api::ReconcileReason::UnrepresentableFrame, &why);
        } else {
            self.hub.invalidate_session_because(&self.session, &why);
        }
        self.publication.refuse(self.session.clone(), reason);
    }

    pub(super) fn publish_usage(&self, usage: TokenUsage) -> Result<(), ExecutionRefusal> {
        let Ok(Some((cost, usage))) = costed_usage_dto(usage, None) else {
            // The compatibility event couples required tokens with required
            // cost. Production has no cost observation yet, so durable token
            // truth remains queryable while this combined live projection is
            // withheld. Missing cost is never rewritten as known zero.
            return Ok(());
        };
        let event = DogfoodEvent::UsageUpdated(LiveEvent::new(
            wire_event(&self.event_id()?)?,
            finite(self.clock.now().as_millis())?,
            UsageUpdatedData::new(wire_session(&self.session)?, cost, usage),
        ));
        self.hub
            .publish(&event)
            .map(|_| ())
            .map_err(|error| ExecutionRefusal::Run(error.to_string()))
    }

    pub(super) fn event_id(&self) -> Result<EventId, ExecutionRefusal> {
        format!("evt_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|_| ExecutionRefusal::Compose("live event identity was invalid".to_owned()))
    }
}

impl CommittedFactSink for LiveSink {
    fn step_began(&mut self, step: StepNumber) {
        self.step = step;
        self.message = None;
        self.attempt = None;
        self.text_ordinal = 0;
        self.reasoning_ordinal = 0;
        self.attempt_ordinals = (0, 0);
        self.blocks.clear();
        self.pending.clear();
        self.refused.clear();
        self.tools.clear();
    }
    fn step_settled(&mut self, _position: &grapt_session::CommittedPosition, fact: &DurableFact) {
        if let DurableFact::StepFailed { failure } = fact {
            super::failure_notice::record(self, failure);
        }
        // The coupled Step event cannot invent missing billing. The chain's
        // committed execution terminal lets the affected client hydrate final
        // durable state while its live subscription continues.
    }

    fn open_blocks_discarded(&mut self) {
        let published = self
            .blocks
            .iter()
            .chain(self.pending.iter().flatten())
            .any(|held| held.public.is_some());
        for held in self.blocks.iter().chain(self.pending.iter().flatten()) {
            self.withdraw_provisional(held);
        }
        self.blocks.clear();
        self.pending.clear();
        self.refused.clear();
        (self.text_ordinal, self.reasoning_ordinal) = self.attempt_ordinals;
        // Subscribers hold slices the durable history will never contain, so
        // they reload it. A refused block already made them reload.
        if published {
            self.hub.invalidate_session_because(
                &self.session,
                &format!(
                    "session {}: attempt retried; reload durable history",
                    self.session
                ),
            );
        }
    }

    fn content_opened(
        &mut self,
        position: &grapt_session::CommittedPosition,
        fact: &grapt_event::session_message::ContentBlockOpenedFact,
    ) {
        if let Err(error) = self.open_block(position, fact) {
            let kind = match fact.kind() {
                grapt_event::session_message::ContentBlockKind::Text => ContentKind::Text,
                grapt_event::session_message::ContentBlockKind::Reasoning => ContentKind::Reasoning,
            };
            self.refuse_block(fact.block(), kind, error);
        }
    }
    fn content_captured(
        &mut self,
        position: &grapt_session::CommittedPosition,
        fact: &grapt_event::session_message::ContentAppendedFact,
    ) {
        if let ContentKind::Tool { call } = fact.item().kind() {
            if let Err(reason) = self.publish_tool_open(position, fact.message(), call) {
                self.reconcile_tool_publication(reason);
            }
            return;
        }
        if fact.item().kind() == &ContentKind::Reasoning && !self.pending.is_empty() {
            if let Err(error) = self.publish_pending(position, fact) {
                self.publication_refusal(error);
            }
            return;
        }
        // A refused block's capture settles its latch; subscribers already
        // re-hydrate from durable truth.
        if let Some(index) = self
            .refused
            .iter()
            .position(|(_, kind)| kind == fact.item().kind())
        {
            if !self
                .blocks
                .iter()
                .any(|held| &held.kind == fact.item().kind())
            {
                self.refused.remove(index);
                return;
            }
        }
        if let Err(error) = self.publish_capture(position, fact) {
            self.publication_refusal(error);
        }
    }

    fn offer(&mut self, fact: &DurableFact) {
        if let DurableFact::ToolTerminal { call, .. } = fact {
            if let Err(reason) = self.publish_tool_terminal(call) {
                self.reconcile_tool_publication(reason);
            }
        }

        if matches!(
            fact,
            DurableFact::NestedToolCalled { .. } | DurableFact::ToolTerminal { .. }
        ) {
            if let Err(reason) = self.publish_tool_progress(fact) {
                self.reconcile_tool_publication(reason);
            }
        }
        if let DurableFact::AttemptStarted { attempt } = fact {
            self.attempt = Some(attempt.clone());
            self.attempt_ordinals = (self.text_ordinal, self.reasoning_ordinal);
        }
        if matches!(fact, DurableFact::AttemptStarted { .. }) && self.message.is_none() {
            let result = {
                let inbox = match self.inbox.lock() {
                    Ok(inbox) => inbox,
                    Err(poisoned) => poisoned.into_inner(),
                };
                super::super::step_events::opening(
                    &inbox,
                    &self.session,
                    self.step,
                    self.agent.clone(),
                    self.model.clone(),
                )
            };
            match result {
                Ok((message, event)) => {
                    self.message = Some(message);
                    if let Err(error) = self.hub.publish(&event) {
                        self.hub.invalidate_session_because(
                            &self.session,
                            &format!(
                                "session {}: step opening could not be published: {error}",
                                self.session
                            ),
                        );
                    }
                }
                Err(reason) => {
                    self.hub.invalidate_session_because(
                        &self.session,
                        &format!("session {}: step opening refused: {reason}", self.session),
                    );
                    self.publication.refuse(self.session.clone(), reason);
                }
            }
        }
        if let DurableFact::Fragment { fragment } = fact {
            // This fact is already durable. Bounded public notification is an
            // advisory: a DTO-size refusal or disconnected listener may omit
            // a frame, but cannot rewrite provider/runtime terminal truth.
            if let Err(error) = self.offer_fragment(fragment) {
                self.publication_refusal(error);
            }
        }
    }
}

/// The registry identity of a public block: the identity its frames name.
fn provisional_block(held: &LiveBlock, ordinal: u64) -> grapt_api::ProvisionalBlock {
    grapt_api::ProvisionalBlock {
        message: held.opening.message().clone(),
        kind: if held.kind == ContentKind::Reasoning {
            grapt_api::ProvisionalKind::Reasoning
        } else {
            grapt_api::ProvisionalKind::Text
        },
        ordinal,
    }
}
