//! Bounded capture and durable content publication for the owning Step.
use super::*;

impl<A: ArtifactStore, S: CommittedFactSink> DurableAttemptOwner<'_, A, S> {
    /// A close and its replay state share one synchronous database segment.
    pub(super) fn close_block(&mut self, block: u32) -> Result<(), OwnerError> {
        self.close_block_with_state(block, None)
    }

    fn close_block_with_state(
        &mut self,
        block: u32,
        replay: Option<&[u8]>,
    ) -> Result<(), OwnerError> {
        let position = self.blocks.iter().position(|(held, _)| *held == block);
        let Some(position) = position else {
            return Err(OwnerError {
                reason: format!("block {block} closed without being opened"),
            });
        };
        let (_, open) = self.blocks.remove(position);
        self.append_finished(open, replay)
    }

    fn append_finished(
        &mut self,
        open: OpenBlock,
        replay: Option<&[u8]>,
    ) -> Result<(), OwnerError> {
        let mut inputs = Vec::with_capacity(2);
        // An empty block has no content item or ordinal; replay may still exist.
        if !open.text.is_empty() || open.dropped != 0 {
            let bound = CaptureBound::new(MAX_BLOCK_CAPTURE_BYTES, BLOCK_CAPTURE_BUFFER_BYTES)
                .ok_or_else(|| OwnerError {
                    reason: "invalid block capture bound".into(),
                })?;
            inputs.push(persistence::ContentCapture {
                kind: open.kind,
                bytes: open.text.as_bytes(),
                bound,
                withheld: open.dropped,
            });
        }
        if let Some(bytes) = replay {
            let bound = CaptureBound::new(
                grapt_provider::AdapterState::MAX_BYTES as u64,
                BLOCK_CAPTURE_BUFFER_BYTES,
            )
            .ok_or_else(|| OwnerError {
                reason: "invalid replay capture bound".into(),
            })?;
            inputs.push(persistence::ContentCapture {
                kind: ContentKind::Opaque,
                bytes,
                bound,
                withheld: 0,
            });
        }
        self.append_contents(&inputs, None)
    }

    /// Accumulates a delta, refusing to grow past the named bound.
    ///
    /// Bytes beyond the bound are COUNTED rather than kept, so the capture's
    /// disposition can say truncation happened. Growing the string and letting
    /// the capture truncate later would mean holding the unbounded string the
    /// contract forbids.
    pub(super) fn accumulate(
        &mut self,
        block: u32,
        kind: ContentKind,
        text: &str,
    ) -> Result<(), OwnerError> {
        let entry = self
            .blocks
            .iter_mut()
            .find(|(held, entry)| *held == block && entry.kind == kind)
            .map(|(_, entry)| entry)
            .ok_or_else(|| OwnerError {
                reason: format!("block {block} received bytes before its opening committed"),
            })?;
        let room = MAX_BLOCK_CAPTURE_BYTES.saturating_sub(entry.text.len() as u64);
        if room == 0 {
            entry.dropped = entry.dropped.saturating_add(text.len() as u64);
            return Ok(());
        }
        if (text.len() as u64) <= room {
            entry.text.push_str(text);
            let held = entry.text.len() as u64;
            self.peak_block_bytes = self.peak_block_bytes.max(held);
            return Ok(());
        }
        // Split on a CHARACTER boundary at or below the room remaining: a byte
        // split would produce invalid UTF-8 in the captured artifact.
        let mut cut = room as usize;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        entry.text.push_str(&text[..cut]);
        entry.dropped = entry.dropped.saturating_add((text.len() - cut) as u64);
        let held = entry.text.len() as u64;
        self.peak_block_bytes = self.peak_block_bytes.max(held);
        Ok(())
    }

    pub(super) fn open_block(&mut self, block: u32, kind: ContentKind) -> Result<(), OwnerError> {
        // The normalized stream admits at most one text and one reasoning
        // block at a time. Enforce that bound at the durable owner as well.
        if self.blocks.len() >= 2
            || self
                .blocks
                .iter()
                .any(|(held, open)| *held == block || open.kind == kind)
        {
            return Err(OwnerError {
                reason: format!(
                    "block {block} duplicates an opening or exceeds the open-block bound"
                ),
            });
        }
        let attempt = self.attempt.clone().ok_or_else(|| OwnerError {
            reason: "block arrived before any attempt opened".into(),
        })?;
        let message = self.message.clone().ok_or_else(|| OwnerError {
            reason: "block arrived before its assistant opening committed".into(),
        })?;
        let block_kind = match &kind {
            ContentKind::Text => ContentBlockKind::Text,
            ContentKind::Reasoning => ContentBlockKind::Reasoning,
            _ => {
                return Err(OwnerError {
                    reason: "only text or reasoning has a streaming block".into(),
                })
            }
        };
        let id = self.event_id()?;
        let fact = ContentBlockOpenedFact::new(attempt, message, block, block_kind);
        let now = self.now_ms();
        let seq = {
            let mut guard = self.store.lock();
            let writer = self.writer(&guard)?;
            let tx = guard.stream_transaction().map_err(|source| OwnerError {
                reason: source.to_string(),
            })?;
            let appended = grapt_store::session_message::record_block_opened(
                &tx,
                &self.session,
                &id,
                writer,
                now,
                &fact,
                &mut [],
            )
            .map_err(write)?;
            tx.commit()
                .map_err(|source| OwnerError {
                    reason: source.to_string(),
                })?
                .discard();
            match appended {
                grapt_store::Appended::Committed { seq }
                | grapt_store::Appended::Idempotent { seq } => seq,
            }
        };
        self.blocks.push((
            block,
            OpenBlock {
                kind,
                text: String::new(),
                dropped: 0,
                checkpoint: checkpoint::Checkpointed::opened_at(now),
            },
        ));
        self.sink.content_opened(
            &CommittedPosition {
                event: id,
                seq,
                created_ms: now,
            },
            &fact,
        );
        Ok(())
    }

    /// Hold a finished reasoning block while its attempt has shown nothing
    /// visible; commit it at once otherwise, or when holding it would pass
    /// [`MAX_HELD_REASONING_BYTES`] or [`MAX_HELD_REASONING_BLOCKS`]. A forced
    /// commit makes the attempt visible. A block with no bytes and no replay
    /// would commit nothing, so it is not held at all.
    fn finish_reasoning(&mut self, block: u32, replay: Option<&[u8]>) -> Result<(), OwnerError> {
        if !self.attempt_visible {
            let position = self.blocks.iter().position(|(held, _)| *held == block);
            let Some(position) = position else {
                return Err(OwnerError {
                    reason: format!("block {block} closed without being opened"),
                });
            };
            let (_, open) = self.blocks.remove(position);
            let held = HeldReasoning {
                block: open,
                replay: replay.map(<[u8]>::to_vec),
            };
            if held.block.text.is_empty() && held.block.dropped == 0 && held.replay.is_none() {
                return Ok(());
            }
            self.held_bytes = self.held_bytes.saturating_add(held.bytes());
            self.held.push(held);
            if self.held_bytes <= MAX_HELD_REASONING_BYTES
                && self.held.len() <= MAX_HELD_REASONING_BLOCKS
            {
                return Ok(());
            }
            self.attempt_visible = true;
            return self.commit_held();
        }
        self.close_block_with_state(block, replay)
    }

    /// Commit held reasoning in close order: visible output followed it, or
    /// its Step settles with it.
    pub(super) fn commit_held(&mut self) -> Result<(), OwnerError> {
        self.held_bytes = 0;
        for held in std::mem::take(&mut self.held) {
            self.append_finished(held.block, held.replay.as_deref())?;
        }
        Ok(())
    }

    pub(super) fn apply_fragment(&mut self, fragment: &AttemptFragment) -> Result<(), OwnerError> {
        // Visible output ends the hold: what was held precedes it.
        if fragment.is_visible_output() {
            self.commit_held()?;
        }
        match fragment {
            AttemptFragment::AttemptOpened => Ok(()),
            AttemptFragment::TextOpened { block, .. } => {
                self.open_block(block.value(), ContentKind::Text)
            }
            AttemptFragment::TextDelta { block, text, .. } => {
                self.accumulate(block.value(), ContentKind::Text, text)?;
                self.checkpoint_if_due(block.value())
            }
            AttemptFragment::TextClosed { block, state } => {
                self.close_block_with_state(block.value(), state.capture_bytes())
            }
            AttemptFragment::ReasoningOpened { block, .. } => {
                self.open_block(block.value(), ContentKind::Reasoning)
            }
            AttemptFragment::ReasoningDelta { block, text, .. } => {
                self.accumulate(block.value(), ContentKind::Reasoning, text)
            }
            AttemptFragment::ReasoningClosed { block, state } => {
                self.finish_reasoning(block.value(), state.capture_bytes())
            }
            AttemptFragment::ToolCalled {
                call,
                tool,
                input,
                execution,
                state,
            } => {
                let identity = self.tool_identity(call, tool, *execution)?;
                let bound = match CaptureBound::new(
                    MAX_TOOL_INPUT_CAPTURE_BYTES,
                    BLOCK_CAPTURE_BUFFER_BYTES,
                ) {
                    Some(bound) => bound,
                    None => unreachable!("the named tool-input bound is non-zero"),
                };
                // The whole input reaches the capture, so the capture boundary
                // itself observes any over-bound byte and nothing was withheld
                // upstream of it.
                let mut inputs = vec![persistence::ContentCapture {
                    kind: ContentKind::Tool { call: call.clone() },
                    bytes: input.as_bytes(),
                    bound,
                    withheld: 0,
                }];
                if let Some(bytes) = state.capture_bytes() {
                    let bound = CaptureBound::new(
                        grapt_provider::AdapterState::MAX_BYTES as u64,
                        BLOCK_CAPTURE_BUFFER_BYTES,
                    )
                    .ok_or_else(|| OwnerError {
                        reason: "invalid tool replay capture bound".into(),
                    })?;
                    inputs.push(persistence::ContentCapture {
                        kind: ContentKind::Opaque,
                        bytes,
                        bound,
                        withheld: 0,
                    });
                }
                self.append_contents(&inputs, Some(identity))
            }
            AttemptFragment::HostedToolResult { state, .. }
            | AttemptFragment::HostedToolFailed { state, .. } => {
                if let Some(bytes) = state.capture_bytes() {
                    let bound = CaptureBound::new(
                        grapt_provider::AdapterState::MAX_BYTES as u64,
                        BLOCK_CAPTURE_BUFFER_BYTES,
                    )
                    .ok_or_else(|| OwnerError {
                        reason: "invalid hosted replay capture bound".into(),
                    })?;
                    self.append_content(ContentKind::Opaque, bytes, bound, 0)?;
                }
                Ok(())
            }
            AttemptFragment::Usage { usage } => {
                self.append_usage(usage, UsageObservationPhase::DuringStream)
            }
            AttemptFragment::ProviderTerminal { usage, .. } => match usage {
                Some(usage) => self.append_usage(usage, UsageObservationPhase::Terminal),
                None => Ok(()),
            },
            // Tool input assembly, invalid input and provider failures reach
            // durability through the tool terminal and the Step
            // terminal, both of which arrive as their own `DurableFact`. There
            // is no second route to them here.
            AttemptFragment::ToolInputOpened { .. }
            | AttemptFragment::ToolInputDelta { .. }
            | AttemptFragment::ToolInputClosed { .. }
            | AttemptFragment::ToolInputInvalid { .. }
            | AttemptFragment::ProviderFailure { .. } => Ok(()),
        }
    }
}
