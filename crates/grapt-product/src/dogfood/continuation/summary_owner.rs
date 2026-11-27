use super::*;
use grapt_event::compaction_call::{self, CallCoordinates, CallEnd};
use grapt_provider::{AttemptFragment, TokenUsage};
use grapt_runtime::{AttemptOwner, DurableFact, Observation, OwnerError};
use grapt_types::{
    ClockAuthority, CompactionId, EventId, EventVersion, IdAuthority, StepNumber,
    UsageObservationPhase,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rejection {
    ToolEffect,
    TooManyItems,
    TextBound,
}

pub(super) struct SummaryOwner {
    pub inbox: Arc<Mutex<Inbox>>,
    pub artifacts: Arc<FilesystemArtifactStore>,
    pub session: SessionId,
    pub ids: Arc<dyn IdAuthority + Send + Sync>,
    pub clock: Arc<dyn ClockAuthority + Send + Sync>,
    pub compaction: CompactionId,
    pub step: StepNumber,
    pub text: String,
    pub coordinates: Option<CallCoordinates>,
    /// The provider compaction states a compaction_trigger call returned, as
    /// retained, in order. Never displayed.
    pub compaction_states: Vec<String>,
    /// The call's provider-reported input and output tokens, at its end.
    pub tokens: Option<(Option<u64>, Option<u64>)>,
    /// Output items other than a compaction state (a reasoning item, a
    /// message), counted before normalization drops them, so a response that
    /// must be exactly one item can be refused (SC-CACHE-07).
    pub other_items: usize,
    /// Why this owner refused a fragment, independent of the displayed text.
    pub rejection: Option<Rejection>,
    /// Stop a malformed response that keeps emitting after rejection, while
    /// allowing its already-following usage and terminal fragments to settle.
    pub rejected_fragments: u16,
}

impl SummaryOwner {
    fn reject(&mut self, rejection: Rejection) {
        self.rejection.get_or_insert(rejection);
    }

    pub(super) fn rejection_reason(&self) -> &'static str {
        match self.rejection {
            Some(Rejection::ToolEffect) => "tool call",
            Some(Rejection::TooManyItems) => "oversize",
            Some(Rejection::TextBound) | None => "rejected",
        }
    }

    fn append(&self, kind: &str, data: serde_json::Value) -> Result<(), OwnerError> {
        let id: EventId = format!("evt_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|error| OwnerError {
                reason: format!("summary event identity: {error}"),
            })?;
        let mut inbox = lock(&self.inbox);
        let aggregate = grapt_store::AggregateId::Session(self.session.clone());
        let writer = inbox
            .store()
            .fencing_token(&aggregate)
            .map_err(owner_error)?;
        let (_, notices) = inbox
            .store_mut()
            .append(
                grapt_store::AppendInput {
                    id: &id,
                    aggregate,
                    writer,
                    kind_name: kind,
                    version: EventVersion::new(1).ok_or_else(|| OwnerError {
                        reason: "summary event version invalid".to_owned(),
                    })?,
                    created_ms: self.clock.now().as_millis(),
                    data: &data,
                },
                &mut [],
            )
            .map_err(owner_error)?;
        notices.discard();
        Ok(())
    }

    fn usage(&self, usage: &TokenUsage, phase: UsageObservationPhase) -> Result<(), OwnerError> {
        let coordinates = self.coordinates.as_ref().ok_or_else(|| OwnerError {
            reason: "summary usage precedes its attempt".to_owned(),
        })?;
        let fact = grapt_event::session_message::UsageObservedFact::new(
            coordinates.attempt.clone(),
            grapt_types::AttemptUsage::observed(
                usage.input_total().clone(),
                usage.input_fresh().clone(),
                usage.output().clone(),
                usage.reasoning().clone(),
                usage.cache_read().clone(),
                usage.cache_write().clone(),
                usage.total().clone(),
                phase,
            ),
        );
        // The established normalized usage codec preserves absent versus zero,
        // phases and derivation provenance. This kind is outside assistant history.
        self.append(
            "session.compaction.call.usage",
            grapt_event::session_message::encode_observed(&fact),
        )
    }

    fn end(&self, outcome: CallEnd) -> Result<(), OwnerError> {
        let coordinates = self.coordinates.as_ref().ok_or_else(|| OwnerError {
            reason: "summary terminal precedes its attempt".to_owned(),
        })?;
        self.append(
            "session.compaction.call.ended",
            compaction_call::encode_ended(coordinates, outcome),
        )
    }
}

fn owner_error(error: impl std::fmt::Display) -> OwnerError {
    OwnerError {
        reason: error.to_string(),
    }
}

impl AttemptOwner for SummaryOwner {
    fn capture_hosted_output(
        &mut self,
        text: &str,
    ) -> Result<grapt_types::ToolTerminalArtifact, OwnerError> {
        self.reject(Rejection::ToolEffect);
        let bound = grapt_artifact::CaptureBound::new(retained::MAX_SUMMARY_BYTES, 4096)
            .ok_or_else(|| owner_error("summary hosted output bound invalid"))?;
        let captured = self
            .artifacts
            .capture(
                text.as_bytes(),
                bound,
                grapt_artifact::ArtifactKind::CapturedOutput,
                grapt_artifact::Sensitivity::SecretBearing,
            )
            .map_err(owner_error)?;
        grapt_types::ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
            .ok_or_else(|| owner_error("summary hosted output disposition disagrees"))
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn commit(&mut self, fact: DurableFact) -> Result<(), OwnerError> {
        match fact {
            DurableFact::AttemptStarted { attempt } => {
                if self.coordinates.is_some() {
                    return Err(owner_error("summary attempt opened twice"));
                }
                let coordinates = CallCoordinates {
                    compaction: self.compaction.clone(),
                    attempt,
                    step: self.step,
                };
                self.append(
                    "session.compaction.call.started",
                    compaction_call::encode_started(&coordinates),
                )?;
                self.coordinates = Some(coordinates);
                Ok(())
            }
            DurableFact::Fragment { fragment } => {
                if self.rejection.is_some()
                    && !matches!(
                        &*fragment,
                        AttemptFragment::Usage { .. } | AttemptFragment::ProviderTerminal { .. }
                    )
                {
                    self.rejected_fragments = self.rejected_fragments.saturating_add(1);
                    if self.rejected_fragments > 64 {
                        return Err(owner_error(
                            "rejected summary exceeded its 64-fragment drain bound",
                        ));
                    }
                    return Ok(());
                }
                match *fragment {
                    AttemptFragment::TextDelta { text, .. } => {
                        if self.text.len().saturating_add(text.len())
                            > retained::MAX_SUMMARY_BYTES as usize
                        {
                            self.reject(Rejection::TextBound);
                            return Ok(());
                        }
                        self.text.push_str(&text);
                        Ok(())
                    }
                    AttemptFragment::ReasoningClosed { state, .. } => {
                        if let Some(text) = state
                            .capture_bytes()
                            .and_then(|bytes| std::str::from_utf8(bytes).ok())
                            .filter(|text| text.contains("\"openai-compaction-v1\""))
                        {
                            if self.compaction_states.len()
                                >= super::compaction::MAX_COMPACTION_ITEMS
                            {
                                self.reject(Rejection::TooManyItems);
                                return Ok(());
                            }
                            self.compaction_states.push(text.to_owned());
                        } else {
                            self.other_items = self.other_items.saturating_add(1);
                        }
                        Ok(())
                    }
                    AttemptFragment::TextClosed { .. } => {
                        self.other_items = self.other_items.saturating_add(1);
                        Ok(())
                    }
                    AttemptFragment::Usage { usage } => {
                        self.usage(&usage, UsageObservationPhase::DuringStream)
                    }
                    AttemptFragment::ProviderTerminal {
                        usage: Some(usage), ..
                    } => {
                        self.tokens = Some((usage.input_total().value(), usage.output().value()));
                        self.usage(&usage, UsageObservationPhase::Terminal)
                    }
                    AttemptFragment::ToolInputOpened { .. }
                    | AttemptFragment::ToolInputDelta { .. }
                    | AttemptFragment::ToolInputClosed { .. }
                    | AttemptFragment::ToolInputInvalid { .. }
                    | AttemptFragment::ToolCalled { .. }
                    | AttemptFragment::HostedToolResult { .. }
                    | AttemptFragment::HostedToolFailed { .. } => {
                        self.reject(Rejection::ToolEffect);
                        Ok(())
                    }
                    _ => Ok(()),
                }
            }
            DurableFact::AttemptEnded { .. } => self.end(if self.rejection.is_some() {
                CallEnd::Failed
            } else {
                CallEnd::Completed
            }),
            DurableFact::AttemptFailed { failure } => {
                let outcome = if matches!(
                    failure.reason(),
                    grapt_types::AttemptTerminalReason::Interrupted { .. }
                ) {
                    CallEnd::Cancelled
                } else {
                    CallEnd::Failed
                };
                self.end(outcome)
            }
            // NoSummaryTools exposes no effect executor. Its local refusal can
            // settle while the rejected provider response drains its usage.
            DurableFact::ToolTerminal { .. } if self.rejection.is_some() => Ok(()),
            DurableFact::NestedToolCalled { .. }
            | DurableFact::ToolTerminal { .. }
            | DurableFact::StepEnded { .. }
            | DurableFact::StepFailed { .. } => Err(owner_error(
                "summary operation cannot settle tools or a coding Step",
            )),
        }
    }
    fn notify(&mut self, _: &DurableFact) {}
    fn observe(&mut self, _: Observation) {}
}

#[cfg(test)]
#[path = "summary_owner_tests.rs"]
mod tests;
