//! Content capture and terminal usage writes share the attempt owner's custody.
use super::*;

pub(super) struct ContentCapture<'a> {
    pub(super) kind: ContentKind,
    pub(super) bytes: &'a [u8],
    pub(super) bound: CaptureBound,
    pub(super) withheld: u64,
}

impl<A: ArtifactStore, S: CommittedFactSink> DurableAttemptOwner<'_, A, S> {
    /// Captures bytes and appends one content item at the next ordinal.
    ///
    /// The artifact commits FIRST and the referencing event afterwards, which
    /// is the order [`ArtifactStore::capture`] documents: an interrupted
    /// capture leaves collectable garbage rather than a dangling reference.
    ///
    /// `withheld` counts bytes dropped by a bounded producer before capture.
    /// The artifact boundary cannot observe those bytes; zero means it received
    /// everything. The persisted disposition accounts for both sources of loss.
    pub(super) fn append_content(
        &mut self,
        kind: ContentKind,
        bytes: &[u8],
        bound: CaptureBound,
        withheld: u64,
    ) -> Result<(), OwnerError> {
        self.append_content_with_identity(kind, bytes, bound, withheld, None)
    }

    pub(super) fn append_content_with_identity(
        &mut self,
        kind: ContentKind,
        bytes: &[u8],
        bound: CaptureBound,
        withheld: u64,
        identity: Option<tool_input::Identity>,
    ) -> Result<(), OwnerError> {
        self.append_contents(
            &[ContentCapture {
                kind,
                bytes,
                bound,
                withheld,
            }],
            identity,
        )
    }

    /// A segment holds prose or tool input and at most one opaque replay item. Capture
    /// artifacts before taking the store lock, then commit the segment once.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn append_contents(
        &mut self,
        inputs: &[ContentCapture<'_>],
        identity: Option<tool_input::Identity>,
    ) -> Result<(), OwnerError> {
        if inputs.is_empty() {
            return Ok(());
        }
        if inputs.len() > 2 {
            return Err(OwnerError {
                reason: "content segment exceeds two items".into(),
            });
        }
        let message = self.message.clone().ok_or_else(|| OwnerError {
            reason: "content arrived before any attempt opened".into(),
        })?;
        let mut ordinal = self.next_ordinal;
        let mut prepared = Vec::with_capacity(inputs.len());
        for input in inputs {
            let captured = self
                .artifacts
                .capture(
                    input.bytes,
                    input.bound,
                    ArtifactKind::CapturedOutput,
                    if matches!(input.kind, ContentKind::Opaque) {
                        Sensitivity::SecretBearing
                    } else {
                        Sensitivity::Ordinary
                    },
                )
                .map_err(|source| OwnerError {
                    reason: format!("content capture failed: {source}"),
                })?;
            let item = NormalizedContentItem::new(
                ordinal,
                input.kind.clone(),
                captured.reference().clone(),
            );
            // Account for bytes a bounded producer withheld before capture.
            let disposition = match input.withheld {
                0 => captured.disposition(),
                _ => CaptureDisposition::TruncatedAtBoundary {
                    captured: item.payload().length(),
                },
            };
            let fact =
                ContentAppendedFact::new(message.clone(), item, disposition).map_err(|source| {
                    OwnerError {
                        reason: format!("the content fact refused its own disposition: {source}"),
                    }
                })?;
            prepared.push((self.event_id()?, self.now_ms(), fact));
            ordinal = ordinal.next().ok_or_else(|| OwnerError {
                reason: "content ordinals are exhausted for this message".into(),
            })?;
        }
        let positions = {
            let lock_span = self.timing_context().map(|mut context| {
                context.scope.class = grapt_types::timing::TimingClass::Content;
                context.span(grapt_types::timing::TimingStage::StoreLock)
            });
            let mut guard = self.store.lock();
            drop(lock_span);
            let writer = self.writer(&guard)?;
            let tx = if identity.is_some() {
                guard.governed_transaction()
            } else {
                guard.stream_transaction()
            }
            .map_err(|source| OwnerError {
                reason: source.to_string(),
            })?;
            // Both facts precede local tool I/O and have no asynchronous gap.
            // A refusal of the input must roll its identity back with it.
            if let Some(identity) = &identity {
                identity.record(&tx, &self.session, writer)?;
            }
            let mut positions = Vec::with_capacity(prepared.len());
            for (id, now, fact) in &prepared {
                let appended = grapt_store::session_message::record_appended(
                    &tx,
                    &self.session,
                    id,
                    writer,
                    *now,
                    fact,
                    &mut [],
                )
                .map_err(write)?;
                let seq = match appended {
                    grapt_store::Appended::Committed { seq }
                    | grapt_store::Appended::Idempotent { seq } => seq,
                };
                positions.push(CommittedPosition {
                    event: id.clone(),
                    seq,
                    created_ms: *now,
                });
            }
            tx.commit()
                .map_err(|source| OwnerError {
                    reason: source.to_string(),
                })?
                .discard();
            positions
        };
        self.next_ordinal = ordinal;
        for ((_, _, fact), position) in prepared.iter().zip(&positions) {
            self.sink.content_captured(position, fact);
        }
        Ok(())
    }

    pub(super) fn append_usage(
        &mut self,
        usage: &TokenUsage,
        phase: UsageObservationPhase,
    ) -> Result<(), OwnerError> {
        let attempt = match &self.attempt {
            Some(attempt) => attempt.clone(),
            // Usage with no attempt to attribute it to is not chargeable to
            // anything, and inventing an attribution is worse than dropping the
            // observation loudly.
            None => {
                return Err(OwnerError {
                    reason: "usage arrived before any attempt opened".to_owned(),
                })
            }
        };
        // New durable writes always stamp the route's protocol family. The
        // fallback keeps the observation rather than dropping it, because
        // usage may never fail the turn.
        let observation = match usage.protocol() {
            Some(family) => AttemptUsage::observed_stamped(
                usage.input_total().clone(),
                usage.input_fresh().clone(),
                usage.output().clone(),
                usage.reasoning().clone(),
                usage.cache_read().clone(),
                usage.cache_write().clone(),
                usage.total().clone(),
                phase,
                family,
            ),
            None => AttemptUsage::observed(
                usage.input_total().clone(),
                usage.input_fresh().clone(),
                usage.output().clone(),
                usage.reasoning().clone(),
                usage.cache_read().clone(),
                usage.cache_write().clone(),
                usage.total().clone(),
                phase,
            ),
        };
        let fact = UsageObservedFact::new(attempt, observation);
        let id = self.event_id()?;
        let now = self.now_ms();
        let lock_span = self.timing_context().map(|mut context| {
            context.scope.class = grapt_types::timing::TimingClass::Usage;
            context.span(grapt_types::timing::TimingStage::StoreLock)
        });
        let mut guard = self.store.lock();
        drop(lock_span);
        let writer = self.writer(&guard)?;
        let tx = guard.stream_transaction().map_err(|source| OwnerError {
            reason: source.to_string(),
        })?;
        grapt_store::session_message::record_observed(
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
        Ok(())
    }
}
