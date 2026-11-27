use super::compaction_recovery::Cause;
use super::summarization::{SummaryPreparation, SUMMARY_INSTRUCTION};
use super::*;
use grapt_artifact::{ArtifactKind, CaptureBound, Sensitivity};
use grapt_provider::{PhysicalAttemptAuthoritySnapshot, ProtocolFamily, ProviderRequest};
use grapt_runtime::{
    CompactionContext, CompactionError, CompactionFailure, PlannedAttempt, ProviderCompactionCall,
    StepOutcome,
};
use grapt_types::{
    ClockAuthority, CompactionDescriptor, CompactionDescriptorParts, CompactionId,
    CompactionSummary, CompactionTrigger, ContextWindowId, EventId, IdAuthority,
    InterruptionSource, NextRequestRef, RetainedHistory, RetentionPolicyVersion,
};

mod fallback;
mod plans;
mod public_event;
mod summary_call;
use public_event::public_ended;
pub(in crate::dogfood) use summary_call::REFUSED_SUMMARY;

/// The most compaction items one provider compaction may commit.
pub(super) const MAX_COMPACTION_ITEMS: usize = 4;
/// The provider-compaction fallback when hosted search was on offer and the
/// response was anything but exactly one compaction item.
pub(super) const NOT_SINGLE_ITEM: &str = "compaction-response-not-single-item";
/// The most a provider compaction's items may serialize to together.
const MAX_COMPACTION_TOTAL_BYTES: usize = 256 * 1024;

/// What a committed descriptor stands on.
enum CommittedSummary<'a> {
    /// SE's own summary text.
    Text(&'a str),
    /// The provider's opaque compaction items.
    Provider(ProviderCompaction),
}

/// A provider compaction a compaction_trigger call returned, validated.
struct ProviderCompaction {
    /// The committed item count.
    items: usize,
    /// The exact artifact the commit captures: the provider's states as
    /// received, bounded HERE so an oversized one selects the fallback.
    envelope: String,
    bytes: usize,
    compatibility: grapt_types::SummaryCompatibility,
    tokens: (Option<u64>, Option<u64>),
    /// Text beside the items, ignored by the commit.
    ignored_text_bytes: usize,
}

/// What a compaction mints, timestamps and announces with.
pub(in crate::dogfood) struct CompactionAuthorities {
    pub(in crate::dogfood) feedback: super::super::session_status::RetryFeedback,
    pub(in crate::dogfood) ids: Arc<dyn IdAuthority + Send + Sync>,
    pub(in crate::dogfood) clock: Arc<dyn ClockAuthority + Send + Sync>,
    pub(in crate::dogfood) hub: Arc<grapt_api::u3a::DogfoodEventHub>,
}

pub(in crate::dogfood) struct ProductCompactor {
    feedback: super::super::session_status::RetryFeedback,
    hooks: Option<Arc<super::super::hooks::HookRuntime>>,
    continuation: DurableContinuation,
    prompt: String,
    call: ProviderCompactionCall,
    targets: Arc<dyn super::super::production::ProductTargetAuthority>,
    ids: Arc<dyn IdAuthority + Send + Sync>,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
    admission_diagnosis: super::admission::DiagnosisSlot,
    hub: Arc<grapt_api::u3a::DogfoodEventHub>,
    /// `compaction.server_side`: an OpenAI Responses route compacts on the
    /// provider first.
    server_side: bool,
    /// The proactive ceiling admission compacts at: `None` when `auto` is
    /// off, else the configured token limit, if any, beside the 90% one.
    ceiling: Option<Option<u64>>,
    failure_cause: Option<Cause>,
    manual_instructions: Option<String>,
    /// The turn's own cache key, so a cold summary routes as the turns it
    /// summarizes do.
    routing_session: Option<grapt_provider::PromptCacheKey>,
}

impl std::fmt::Debug for ProductCompactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProductCompactor")
            .field("session", &self.continuation.session)
            .finish_non_exhaustive()
    }
}

impl ProductCompactor {
    pub(in crate::dogfood) fn new(
        continuation: DurableContinuation,
        prompt: String,
        call: ProviderCompactionCall,
        targets: Arc<dyn super::super::production::ProductTargetAuthority>,
        authorities: CompactionAuthorities,
        admission_diagnosis: super::admission::DiagnosisSlot,
    ) -> Self {
        Self {
            hooks: None,
            continuation,
            prompt,
            call,
            targets,
            feedback: authorities.feedback,
            ids: authorities.ids,
            clock: authorities.clock,
            admission_diagnosis,
            hub: authorities.hub,
            server_side: false,
            ceiling: Some(None),
            failure_cause: None,
            manual_instructions: None,
            routing_session: None,
        }
    }

    pub(in crate::dogfood) fn with_routing_session(
        mut self,
        key: grapt_provider::PromptCacheKey,
    ) -> Self {
        self.routing_session = Some(key);
        self
    }

    pub(in crate::dogfood) fn promote_input(&mut self, input: &MessageId) -> Result<(), String> {
        self.prompt = self.continuation.promote_input(input)?;
        super::super::input_events::publish_promoted(
            &lock(&self.continuation.inbox),
            &self.hub,
            &self.continuation.session,
            input,
        );
        Ok(())
    }

    pub(in crate::dogfood) fn with_server_side(mut self, server_side: bool) -> Self {
        self.server_side = server_side;
        self
    }

    /// Judge a committed summary against the ceiling admission uses.
    pub(in crate::dogfood) fn with_ceiling(mut self, auto: bool, limit: Option<u64>) -> Self {
        self.ceiling = auto.then_some(limit);
        self
    }

    pub(in crate::dogfood) fn with_hooks(
        mut self,
        hooks: Arc<super::super::hooks::HookRuntime>,
    ) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// PreCompact, then the compaction itself.
    async fn run_with_pre_hook(
        &mut self,
        context: CompactionContext,
    ) -> Result<(), CompactionFailure> {
        use super::super::hooks::Event;
        let _compacting = self.feedback.compacting();
        if let Some(hooks) = &self.hooks {
            hooks
                .fire(Event::PreCompact, &self.continuation.session, None)
                .await
                .map_err(failed)?;
        }
        self.run(context).await
    }

    async fn post_compact(&self) {
        use super::super::hooks::Event;
        if let Some(hooks) = &self.hooks {
            if let Err(reason) = hooks
                .fire(Event::PostCompact, &self.continuation.session, None)
                .await
            {
                // The descriptor already committed. Its hook failure is durable;
                // returning a compaction failure would invite a second compaction.
                crate::diagnostics::report(format!(
                    "PostCompact hook failed after commit: {reason}"
                ));
            }
        }
    }

    pub(in crate::dogfood) async fn compact(
        &mut self,
        context: CompactionContext,
    ) -> Result<Result<(), CompactionFailure>, InterruptionSource> {
        let signal = context.cancellation.signal();
        let trigger = context.trigger;
        let interrupted = *signal.borrow();
        let settled = match interrupted {
            Some(by) => Err(by),
            None => {
                self.failure_cause = None;
                let result = self.run_with_pre_hook(context).await;
                // Sampled and recorded before PostCompact is awaited: an
                // interrupt during that hook cannot unrecord a committed
                // outcome or turn it into an interruption.
                let interrupted = *signal.borrow();
                if let Some(recovery) = &self.continuation.recovery {
                    record_outcome(recovery, &result, interrupted, trigger, self.failure_cause);
                }
                if committed(&result) {
                    self.post_compact().await;
                }
                settle(result, interrupted)
            }
        };
        if trigger == CompactionTrigger::UserRequested {
            return self.settle_requested(settled);
        }
        settled
    }

    /// A `/compact` that ends without a descriptor settles as the oracle's
    /// manual failure, so it never runs again. A failure is not the user's
    /// message failing: once settled, the Step continues on the uncompacted
    /// context (the failure is durable, which advances the generation the
    /// Step reloads). An interrupt still ends the Step.
    fn settle_requested(
        &self,
        settled: Result<Result<(), CompactionFailure>, InterruptionSource>,
    ) -> Result<Result<(), CompactionFailure>, InterruptionSource> {
        let reason = match &settled {
            Ok(Ok(())) => return settled,
            Ok(Err(CompactionFailure::Failed(error))) => error.reason.clone(),
            Ok(Err(
                CompactionFailure::NoProgress { reason }
                | CompactionFailure::Repeated { reason }
                | CompactionFailure::DidNotFit { reason },
            )) => reason.clone(),
            Err(_) => String::new(),
        };
        let failure = match settled {
            Err(_) => grapt_inbox::CompactionFailure::Aborted,
            Ok(_) => grapt_inbox::CompactionFailure::Failed(&reason),
        };
        let event: Result<EventId, _> = format!("evt_{}", self.ids.mint().as_str()).parse();
        let written = event.map_err(|error| format!("{error}")).and_then(|event| {
            lock(&self.continuation.inbox)
                .fail_compaction(grapt_inbox::CompactionFailRequest {
                    event_id: &event,
                    session: &self.continuation.session,
                    failure,
                    created_ms: self.clock.now().as_millis(),
                })
                .map_err(|error| error.to_string())
        });
        match (settled, written) {
            (Err(by), _) => Err(by),
            (Ok(_), Ok(Some(_))) => Ok(Ok(())),
            (Ok(failure), Ok(None)) => Ok(failure),
            (Ok(_), Err(error)) => Ok(Err(failed(format!(
                "requested compaction failed and could not settle: {error}"
            )))),
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn run(&mut self, context: CompactionContext) -> Result<(), CompactionFailure> {
        self.manual_instructions = if context.trigger == CompactionTrigger::UserRequested {
            lock(&self.continuation.inbox)
                .pending_compaction(&self.continuation.session)
                .map_err(|error| failed(error.to_string()))?
                .and_then(|pending| pending.instructions)
        } else {
            None
        };
        let id = CompactionId::new(self.ids.mint().as_str())
            .ok_or_else(|| failed("compaction identity is invalid"))?;
        let budget = self
            .targets
            .context_tokens()
            .ok_or_else(|| failed("selected model has no known context budget"))?;
        // Admission sends a repeating request uncompacted while it fits, so
        // reaching here means it no longer does: summarizing again would only
        // loop (CONTEXT-QUALITY-01 J04).
        if context.trigger != CompactionTrigger::UserRequested {
            if let Some(reason) = self
                .continuation
                .repeated_since_compaction()
                .map_err(failed)?
            {
                return Err(CompactionFailure::Repeated { reason });
            }
        }
        // One trigger policy: SE decided to compact; an OpenAI Responses route
        // may do it on the provider. Any failure there falls back to SE's own
        // summary in this run, with the reason recorded beside it. The
        // provider's item covers the whole request it compacted, so it
        // retains from its own cut, not the one SE's summary could carry.
        let mut server = None;
        if self.server_side
            && matches!(
                self.targets.protocol_family(),
                ProtocolFamily::OpenAiResponses | ProtocolFamily::ChatGptCodex
            )
        {
            // No new history past the prior cut and a refused provider are
            // one fallback, recorded the same way.
            let fallback_request = self.admission_diagnosis.request_snapshot();
            let fallback_coverage = self.admission_diagnosis.covered();
            let provider_cut = self
                .admission_diagnosis
                .retain_fitting_request(budget)
                .map_err(str::to_owned)
                .and_then(|()| {
                    self.continuation
                        .prepare_provider_summary(&self.prompt, &id, budget)
                        .map_err(|reason| reason.to_string())
                })
                .and_then(|prepared| {
                    if self.admission_diagnosis.covered() == fallback_coverage {
                        return Ok(prepared);
                    }
                    // A cached prefix cannot help if the untouched tail alone
                    // exceeds the next coding request's budget. Restore the
                    // larger cut and summarize that bounded history instead.
                    let minimum = prepared.rebuilt_input("");
                    let estimate = self
                        .admission_diagnosis
                        .rebuilt_estimate(&minimum)
                        .map_err(|_| "retained context accounting failed".to_owned())?;
                    if estimate.is_some_and(|(count, reserve)| {
                        !super::admission::fits(count, reserve, budget)
                    }) {
                        return Err("retained tail exceeds provider compaction budget".to_owned());
                    }
                    Ok(prepared)
                });
            let mut retained_prefix = false;
            let attempt = match provider_cut {
                Ok(prepared) => self
                    .provider_compaction(context.clone(), &id, &mut retained_prefix)
                    .await
                    .map(|provider| (prepared, provider))
                    .map_err(str::to_owned),
                Err(reason) => Err(reason.to_string()),
            };
            if context.cancellation.signal().borrow().is_some() {
                return Err(failed("provider compaction cancelled before publication"));
            }
            match attempt {
                Ok((prepared, provider)) => {
                    self.commit(
                        id,
                        context.step,
                        prepared,
                        CommittedSummary::Provider(provider),
                        context.trigger,
                        None,
                        None,
                    )
                    .map_err(failed)?;
                    crate::diagnostics::report(format!(
                        "session {}: compaction: path: openai-server",
                        self.continuation.session
                    ));
                    return Ok(());
                }
                Err(reason) => {
                    // An admitted provider request already selected a fitting
                    // prefix and its exact coverage. Keep it for the text
                    // fallback; newer history stays outside that cut. A failed
                    // preparation or authority binding cannot retain that cut.
                    if !retained_prefix {
                        self.admission_diagnosis.restore_request(fallback_request);
                    }
                    if let Some(cause) = self.failure_cause.filter(|cause| cause.blocks_fallback())
                    {
                        return Err(failed(cause.message()));
                    }
                    crate::diagnostics::report(format!(
                        "session {}: compaction: path: se-summary; provider fallback: {reason}",
                        self.continuation.session
                    ));
                    server = Some(retained::ServerOutcome::Fallback { reason });
                }
            }
        }
        // History since the retained cut that is past the read bounds is
        // summarized from its newest bounded window; the oldest is dropped.
        let floor = self.continuation.summary_window_floor().map_err(failed)?;
        self.continuation.set_summary_floor(floor);
        if let Some(floor) = floor {
            crate::diagnostics::report(format!(
                "session {}: compaction {id}: history is past the read bounds; summarizing the newest window from {}",
                self.continuation.session,
                floor.value()
            ));
        }
        // The quoted history fills what admission's calibrated charge admits.
        let step = context.step;
        let trigger = context.trigger;
        let cancel = context.cancellation.signal();
        // A summary call the provider refuses as a context overflow is made
        // again with less of the oldest history, as the prior-art compactors
        // drop the oldest item and retry; the runtime bounds the calls.
        let mut history = self.summary_history(budget);
        // A provider compaction that was tried first made the first call.
        let mut calls = 1 + u32::from(server.is_some());
        let mut retry = None;
        let (prepared, owner, plan) = loop {
            let (prepared, planned, plan) =
                self.planned_summary(&id, budget, &mut history, retry.take())?;
            match self.run_summary(&context, &id, planned).await {
                Ok(owner) => break (prepared, owner, plan),
                Err(CompactionFailure::Failed(_))
                    if self.failure_cause == Some(Cause::Context)
                        && calls < plans::MAX_OVERFLOW_TRIES =>
                {
                    calls += 1;
                    history -= history / 8;
                    retry = Some(prepared);
                    self.failure_cause = None;
                    crate::diagnostics::report(format!(
                        "session {}: compaction {id}: the provider refused the summary as a context overflow; dropping the oldest history, call {calls} of {}",
                        self.continuation.session,
                        plans::MAX_OVERFLOW_TRIES
                    ));
                }
                Err(error) => return Err(error),
            }
        };
        if owner.text.len() >= prepared.removed_bytes {
            return Err(CompactionFailure::NoProgress {
                reason: "summary does not reduce retained context".to_owned(),
            });
        }
        if cancel.borrow().is_some() {
            return Err(failed("summary cancelled before publication"));
        }
        let rebuilt = if trigger == CompactionTrigger::StructuralBound {
            None
        } else {
            let rebuilt = prepared.rebuilt_input(&owner.text);
            let estimate = self
                .admission_diagnosis
                .rebuilt_estimate(&rebuilt)
                .map_err(|failure| {
                    failed(format!(
                        "rebuilt coding request accounting failed: {failure:?}"
                    ))
                })?;
            match estimate {
                Some(estimate) => Some(estimate),
                // A requested compaction runs before its input's first
                // request was planned, so there is no snapshot to rebuild;
                // the next ordinary admission still judges the fit.
                None if trigger == CompactionTrigger::UserRequested => None,
                None => return Err(failed("rebuilt coding request has no admission snapshot")),
            }
        };
        let mut prepared = prepared;
        prepared.selection.summary_plan = Some(plan);
        self.commit(
            id,
            step,
            prepared,
            CommittedSummary::Text(&owner.text),
            trigger,
            rebuilt.map(|(estimate, _)| estimate),
            server,
        )
        .map_err(failed)?;
        // The one guard: the descriptor is committed. If the context it left
        // is still at or above the proactive ceiling, or does not fit, the Step
        // stops with that error instead of compacting again at the next Step.
        if let Some((estimate, reserve)) = rebuilt {
            if !super::admission::fits(estimate, reserve, budget)
                || self.ceiling.is_some_and(|limit| {
                    super::admission::auto_compact_reached(Some(estimate), budget, limit)
                })
            {
                return Err(CompactionFailure::DidNotFit {
                    reason: format!(
                        "the compacted context is {estimate} tokens, still at or above the ceiling of a {budget}-token window"
                    ),
                });
            }
        }
        Ok(())
    }

    /// Ask the route to compact the request admission last planned, with a
    /// trailing compaction_trigger, and validate what comes back. The `Err`
    /// is the recorded fallback reason.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn provider_compaction(
        &mut self,
        context: CompactionContext,
        id: &CompactionId,
        retained_prefix: &mut bool,
    ) -> Result<ProviderCompaction, &'static str> {
        let mut request = self
            .admission_diagnosis
            .coding_request()
            .ok_or("no coding request")?;
        let task = self.manual_instructions.as_ref().map(|text| {
            CodexInputItem::Message(InputMessage {
                role: MessageRole::User,
                content: format!("Instructions for this compaction:\n{text}"),
            })
        });
        match &mut request {
            ProviderRequest::OpenAiResponses(request) => {
                request.input.extend(task);
                request.input.push(CodexInputItem::CompactionTrigger)
            }
            ProviderRequest::ChatGptCodex(request) => {
                request.input.extend(task);
                request.input.push(CodexInputItem::CompactionTrigger)
            }
            ProviderRequest::AnthropicMessages(_)
            | ProviderRequest::VertexAnthropic(_)
            | ProviderRequest::OpenAiCompatible(_)
            | ProviderRequest::BedrockConverse(_)
            | ProviderRequest::GoogleGemini(_)
            | ProviderRequest::CohereChat(_)
            | ProviderRequest::WatsonxChat(_)
            | ProviderRequest::VertexGemini(_) => return Err("unsupported protocol"),
        }
        if self.manual_instructions.is_some() {
            let budget = self
                .targets
                .context_tokens()
                .ok_or("unknown context budget")?;
            if self
                .admission_diagnosis
                .warm_summary_overflow(&request, budget)
                .map_err(|_| "compaction instructions accounting failed")?
                .is_some()
            {
                return Err("compaction instructions exceed provider context budget");
            }
        }
        let target = self.targets.issue().map_err(|_| "refused")?;
        let mut builder = PhysicalAttemptAuthoritySnapshot::builder();
        builder.model(target).map_err(|_| "refused")?;
        // The request offers the coding request's tools, byte for byte, so the
        // cached prefix holds; it grants them as the coding attempt did, and
        // the summary call's runner fails any call (NoSummaryTools).
        for visibility in
            super::super::production::tool_visibilities(request.tools()).map_err(|_| "refused")?
        {
            builder.tool(visibility).map_err(|_| "refused")?;
        }
        // Hosted web search rides along
        // with the coding request's `tool_choice: auto`.
        // Only when the grant cannot be made does it fall
        // back, named and counted.
        let hosted = super::super::production::hosted_visibility(&request);
        let searching = hosted.is_some();
        if let Some(visibility) = hosted {
            visibility
                .ok()
                .and_then(|visibility| builder.tool(visibility).ok())
                .ok_or("hosted-tool-offered")?;
        }
        // With search on offer the response must be exactly one compaction
        // item; a search call, text or anything extra is discarded.
        let single = |reason: &'static str| {
            if searching {
                NOT_SINGLE_ITEM
            } else {
                reason
            }
        };
        let authority = builder.build().map_err(|_| "refused")?;
        let model = authority.model().model_ref().clone();
        if &model != request.model() {
            return Err("target changed");
        }
        // Only a prefix rebound to the current model and tool authority can
        // survive a failed provider call. Earlier refusals restore the plan.
        *retained_prefix = true;
        let owner = super::summary_owner::SummaryOwner {
            inbox: self.continuation.inbox.clone(),
            artifacts: self.continuation.artifacts.clone(),
            session: self.continuation.session.clone(),
            ids: self.ids.clone(),
            clock: self.clock.clone(),
            compaction: id.clone(),
            step: context.step,
            text: String::new(),
            coordinates: None,
            compaction_states: Vec::new(),
            tokens: None,
            other_items: 0,
            rejection: None,
            rejected_fragments: 0,
        };
        let planned = PlannedAttempt {
            operation_mail: None,
            mail: None,
            authority,
            request,
            context: None,
        };
        let (owner, report) = self
            .call
            .run(context, planned, owner)
            .await
            .map_err(|_| "refused")?;
        match &report.provider_failure {
            // A 400 may reject the provider-compaction request shape; its text
            // fallback can still work. Every other detail names the cause.
            Some(failure)
                if failure.class != grapt_provider::ProviderFailureClass::InvalidRequest
                    || failure.classification.is_some() =>
            {
                self.failure_cause = Some(Cause::of(Some(failure)));
            }
            Some(_) => {}
            // A watchdog stall carries no detail, only its terminal reason.
            None => {
                if let StepOutcome::Failed { failure } = &report.outcome {
                    self.failure_cause = Some(Cause::of_reason(failure.reason()));
                }
            }
        }
        if owner.rejection.is_some() {
            return Err(single(owner.rejection_reason()));
        }
        match report.outcome {
            StepOutcome::Ended { .. } => {}
            StepOutcome::Failed { failure } => return Err(fallback::failed(failure.reason())),
            // The summary owner refuses a tool effect (nothing runs: the call's
            // runner is NoSummaryTools), more items than one compaction may
            // commit, and text past its bound.
            StepOutcome::OwnerRejected { .. } => {
                return Err(match owner.rejection {
                    Some(_) => single(owner.rejection_reason()),
                    None => "rejected",
                })
            }
        }
        if owner.compaction_states.is_empty() {
            return Err(single("no item"));
        }
        if searching
            && (owner.compaction_states.len() != 1
                || owner.other_items != 0
                || !owner.text.is_empty())
        {
            return Err(NOT_SINGLE_ITEM);
        }
        let mut bytes = 0usize;
        for state in &owner.compaction_states {
            let replay = grapt_provider::ProviderReplay::restore(
                model.provider().clone(),
                model.model().clone(),
                model.variant().cloned(),
                state,
            )
            .map_err(|reason| match reason {
                grapt_provider::ReplayError::CompactionOversize => "oversize",
                grapt_provider::ReplayError::Invalid(_) => "malformed",
            })?;
            if !replay.is_responses_compaction() {
                return Err("malformed");
            }
            bytes = bytes.saturating_add(replay.byte_len());
        }
        if bytes > MAX_COMPACTION_TOTAL_BYTES {
            return Err("oversize");
        }
        // The states are persisted as received, fields the replay ignores
        // included, so the persisted envelope is bounded, not just the
        // normalized items: a commit that could not capture it would fail
        // the compaction instead of falling back.
        let envelope = serde_json::json!({"items": owner.compaction_states}).to_string();
        if u64::try_from(envelope.len())
            .map_or(true, |len| len > retained::MAX_PROVIDER_SUMMARY_BYTES)
        {
            return Err("oversize");
        }
        Ok(ProviderCompaction {
            items: owner.compaction_states.len(),
            envelope,
            bytes,
            compatibility: grapt_types::SummaryCompatibility::bind(&model),
            tokens: owner.tokens.unwrap_or((None, None)),
            ignored_text_bytes: owner.text.len(),
        })
    }

    fn capture(&self, text: &str) -> Result<grapt_types::ArtifactRef, String> {
        let bound = CaptureBound::new(retained::MAX_SUMMARY_BYTES, 4096)
            .ok_or("summary capture bound invalid")?;
        let artifact = self
            .continuation
            .artifacts
            .capture(
                std::io::Cursor::new(text.as_bytes()),
                bound,
                ArtifactKind::CapturedOutput,
                Sensitivity::Ordinary,
            )
            .map_err(|error| error.to_string())?;
        if !artifact.disposition().is_complete() {
            return Err("summary capture was truncated".to_owned());
        }
        Ok(artifact.reference().clone())
    }

    /// A provider compaction's retained item states: secret-bearing, never
    /// displayed, and bounded like the items they carry.
    fn capture_provider(&self, text: &str) -> Result<grapt_types::ArtifactRef, String> {
        let bound = CaptureBound::new(retained::MAX_PROVIDER_SUMMARY_BYTES, 4096)
            .ok_or("provider compaction capture bound invalid")?;
        let artifact = self
            .continuation
            .artifacts
            .capture(
                std::io::Cursor::new(text.as_bytes()),
                bound,
                ArtifactKind::CapturedOutput,
                Sensitivity::SecretBearing,
            )
            .map_err(|error| error.to_string())?;
        if !artifact.disposition().is_complete() {
            return Err("provider compaction capture was truncated".to_owned());
        }
        Ok(artifact.reference().clone())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one commit for both summary kinds keeps a single descriptor transaction"
    )]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn commit(
        &self,
        id: CompactionId,
        step: grapt_types::StepNumber,
        mut prepared: SummaryPreparation,
        summary: CommittedSummary<'_>,
        trigger: CompactionTrigger,
        rebuilt_estimate: Option<u64>,
        server: Option<retained::ServerOutcome>,
    ) -> Result<(), String> {
        let (summary, text, summary_bytes) = match summary {
            CommittedSummary::Text(text) => (
                CompactionSummary::ClientReadable {
                    artifact: self.capture(text)?,
                },
                super::se_summary_notice(text),
                super::retained_users::labelled_summary(text, true).len(),
            ),
            CommittedSummary::Provider(provider) => {
                let artifact = self.capture_provider(&provider.envelope)?;
                prepared.selection.server = Some(retained::ServerOutcome::Committed {
                    items: provider.items,
                    bytes: provider.bytes,
                    input_tokens: provider.tokens.0,
                    output_tokens: provider.tokens.1,
                    ignored_text_bytes: provider.ignored_text_bytes,
                });
                (
                    CompactionSummary::ProviderBound {
                        artifact,
                        compatibility: provider.compatibility,
                    },
                    provider_notice(provider.items, provider.tokens),
                    provider.bytes,
                )
            }
        };
        if server.is_some() {
            prepared.selection.server = server;
        }
        let retained_bytes = prepared
            .retained_bytes
            .checked_add(summary_bytes)
            .ok_or("retained byte count overflowed")?;
        prepared.selection.admission =
            self.admission_diagnosis.matching(trigger).map(|diagnosis| {
                retained::AdmissionDiagnosis {
                    trigger: diagnosis.trigger,
                    estimate_basis: diagnosis.estimate_basis.name(),
                    estimate: diagnosis.estimate,
                    rebuilt_estimate,
                    budget: diagnosis.budget,
                    removed_bytes: prepared.removed_bytes,
                    retained_bytes,
                }
            });
        let selection = self.capture(&prepared.selection.encode().to_string())?;
        let window = ContextWindowId::new(self.ids.mint().as_str())
            .ok_or("context window identity invalid")?;
        let target_epoch = prepared
            .source_epoch
            .next()
            .ok_or("context epoch exhausted")?;
        let descriptor = CompactionDescriptor::new(CompactionDescriptorParts {
            id,
            source_session: self.continuation.session.clone(),
            source_epoch: prepared.source_epoch,
            target_window: window,
            target_epoch,
            trigger,
            retention_policy: RetentionPolicyVersion::new(retained::RETENTION_POLICY)
                .ok_or("retention policy invalid")?,
            summary,
            retained: RetainedHistory::DeterministicSelection {
                descriptor: selection.clone(),
            },
            baseline: selection.clone(),
            next_request: NextRequestRef::new(step, selection),
        })
        .map_err(|error| format!("compaction descriptor refused: {error:?}"))?;
        let event: EventId = format!("evt_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|error| format!("descriptor event identity: {error}"))?;
        let mut inbox = lock(&self.continuation.inbox);
        let aggregate = grapt_store::AggregateId::Session(self.continuation.session.clone());
        let held = inbox.store().held_writers();
        let tx = inbox
            .store_mut()
            .governed_transaction()
            .map_err(|error| error.to_string())?;
        let current = grapt_store::compaction::latest_descriptor(&tx, &self.continuation.session)
            .map_err(|error| error.to_string())?;
        if current.map(|(event, _, _)| event) != prepared.previous_generation {
            return Err("compaction source generation changed before commit".to_owned());
        }
        let mut instructions = grapt_instruction::InstructionProjector;
        // A user-requested descriptor settles its pending request in the same
        // transaction; the inbox projector ignores every other trigger.
        let mut inbox_projector = grapt_inbox::InboxProjector;
        let created = self.clock.now().as_millis();
        let outcome = grapt_store::compaction::record(
            &tx,
            &event,
            held.token(&aggregate).map_err(|error| error.to_string())?,
            created,
            grapt_store::compaction::CompactionAttempt::Settled(&descriptor),
            &mut [&mut instructions, &mut inbox_projector],
        )
        .map_err(|error| error.to_string())?;
        let grapt_store::compaction::CompactionOutcome::Settled { seq } = outcome else {
            return Err("a settled compaction descriptor was refused".to_owned());
        };
        tx.commit().map_err(|error| error.to_string())?.discard();
        // Commit before notify, and notify before releasing the store: the
        // live notice follows durable truth, and no later durable event of this
        // Session can be announced ahead of it, so live sequences never move
        // backwards. A subscriber that misses it re-reads history.
        match public_ended(&event, created, seq, &descriptor, &text, &prepared.recent) {
            Ok(notice) => {
                if let Err(error) = self.hub.publish(&notice) {
                    self.hub.invalidate_because(&format!(
                        "compaction notice could not be published: {error}"
                    ));
                }
            }
            Err(error) => self.hub.invalidate_because(&format!(
                "compaction notice could not be projected: {error}"
            )),
        }
        drop(inbox);
        Ok(())
    }
}

/// The public text of a provider compaction: the fact and its counts, never
/// its opaque content.
pub(in crate::dogfood) fn provider_notice(
    items: usize,
    tokens: (Option<u64>, Option<u64>),
) -> String {
    let count = |tokens: Option<u64>| {
        tokens.map_or_else(|| "unreported".to_owned(), |count| count.to_string())
    };
    format!(
        "[Compacted by the provider: {items} opaque item(s) from {} input tokens, {} output tokens. The history it covered is kept and recoverable with the history tool.]",
        count(tokens.0),
        count(tokens.1)
    )
}

/// Whether `result` published a descriptor: a summary that did not fit was
/// still committed.
fn committed(result: &Result<(), CompactionFailure>) -> bool {
    matches!(result, Ok(()) | Err(CompactionFailure::DidNotFit { .. }))
}

/// Record what a compaction left for the Session's next input. A committed
/// outcome is recorded whatever interrupted it; an unpublished one that was
/// interrupted says nothing against the compaction.
fn record_outcome(
    recovery: &super::compaction_recovery::Session,
    result: &Result<(), CompactionFailure>,
    interrupted: Option<InterruptionSource>,
    trigger: CompactionTrigger,
    transport: Option<Cause>,
) {
    if interrupted.is_some() && !committed(result) {
        return;
    }
    if result.is_ok() {
        recovery.succeeded();
    } else {
        recovery.failed(Cause::of_compaction(result, transport));
        if trigger != CompactionTrigger::UserRequested {
            recovery.report_failure();
        }
    }
}

/// A published descriptor is durable history, so an interrupt that arrives
/// during or after its commit cannot report the compaction as interrupted; the
/// next Step boundary observes the interrupt instead. Only an unpublished
/// compaction becomes the interruption.
fn settle(
    result: Result<(), CompactionFailure>,
    interrupted: Option<InterruptionSource>,
) -> Result<Result<(), CompactionFailure>, InterruptionSource> {
    match interrupted {
        Some(by) if !committed(&result) => Err(by),
        _ => Ok(result),
    }
}

fn failed(reason: impl Into<String>) -> CompactionFailure {
    CompactionFailure::Failed(CompactionError {
        reason: reason.into(),
    })
}

#[cfg(test)]
mod settle_tests {
    use super::*;

    /// An interrupt that lands once a summary that did not fit is committed,
    /// as one during PostCompact does, keeps the committed outcome and the
    /// trim it owes the next input.
    #[test]
    fn an_interrupt_after_a_committed_did_not_fit_keeps_its_outcome_and_trim() -> Result<(), String>
    {
        let did_not_fit = || CompactionFailure::DidNotFit {
            reason: "still over the ceiling".to_owned(),
        };
        let mut recovery = super::super::compaction_recovery::Session::new(
            super::super::compaction_recovery::Registry::default(),
            "ses_post_compact_interrupt"
                .parse()
                .map_err(|_| "session")?,
            "msg_post_compact_first".parse().map_err(|_| "input")?,
        );
        record_outcome(
            &recovery,
            &Err(did_not_fit()),
            Some(InterruptionSource::User),
            CompactionTrigger::PolicyThreshold,
            None,
        );
        assert!(matches!(
            settle(Err(did_not_fit()), Some(InterruptionSource::User)),
            Ok(Err(CompactionFailure::DidNotFit { .. }))
        ));
        recovery.promote_input(&"msg_post_compact_next".parse().map_err(|_| "next")?);
        assert!(recovery.trim_requested(), "the next input lost its trim");
        Ok(())
    }

    #[test]
    fn an_interrupt_after_publication_does_not_disown_the_published_descriptor() {
        assert!(
            matches!(settle(Ok(()), Some(InterruptionSource::User)), Ok(Ok(()))),
            "a published compaction was reported as interrupted"
        );
        assert!(matches!(
            settle(
                Err(failed("summary cancelled before publication")),
                Some(InterruptionSource::User)
            ),
            Err(InterruptionSource::User)
        ));
        assert!(matches!(
            settle(
                Err(CompactionFailure::NoProgress {
                    reason: "no progress".to_owned()
                }),
                None
            ),
            Ok(Err(CompactionFailure::NoProgress { .. }))
        ));
    }
}
