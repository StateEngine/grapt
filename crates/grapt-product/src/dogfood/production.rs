//! Shared production-provider composition policy.
//!
//! LIVE-HEADLESS owns endpoint, durable execution, and usage publication. This
//! module owns only the RMH policy that must remain common to that path: explicit
//! provider bounds, the platform target adapter, and exact one-time consumption
//! of the credential-authorized bootstrap target before fresh per-attempt issue.

mod agent_mail;
mod coding_request;
mod context_trim;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use grapt_provider::{
    ChatGptCodexRequest, PhysicalAttemptAuthoritySnapshot, PromptCacheKey, ProtocolFamily,
    ProviderRequest, ToolChoice, ToolName, ToolVisibility,
};
use grapt_runtime::provider_source::{ContextAdmissionRefusal, PlanRefusalCause};
use grapt_runtime::{HistoryGeneration, PlanRefusal, PlannedAttempt};
use grapt_types::{
    CapabilityId, EffectClass, EgressAuthority, FilesystemAuthority, RealizedEnforcement,
    ResolvedCapability, ResolvedModelTarget, StepNumber,
};

use super::continuation::DurableContinuation;

mod targets;
mod thinking;
mod tool_catalog;
pub(crate) use targets::PlatformTargets;

mod context_hook;
mod image_input;
use context_hook::{apply_session_context_hook, ContextHook};

enum PlanContext {
    Durable(DurableContinuation),
    Transient(Vec<grapt_provider::CodexInputItem>),
}

/// Product-owned, explicit resource policy for provider I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductionProviderLimits {
    pub connect: Duration,
    pub response_head: Duration,
    pub active_attempts: NonZeroUsize,
    pub child_attempts: NonZeroUsize,
}

/// The platform-owned issuer is adapted to this consumer port. It remains the
/// sole authority able to mint a fresh linear target for every attempt.
pub trait ProductTargetAuthority: Send + Sync + 'static {
    fn protocol_family(&self) -> ProtocolFamily;
    fn issue(&self) -> Result<ResolvedModelTarget, String>;
    fn context_tokens(&self) -> Option<u64> {
        None
    }
    fn effort_policy(&self) -> grapt_provider::EffortPolicy {
        Default::default()
    }
    /// The selected model's total output ceiling, including thinking tokens.
    fn anthropic_max_tokens(&self) -> Result<grapt_provider::MaxOutputTokens, String> {
        Err("selected model output limit is unavailable".into())
    }
    /// A requested output budget, capped by the selected model's reported ceiling.
    fn output_bound(&self, requested: u32) -> Result<grapt_provider::MaxOutputTokens, String> {
        grapt_provider::MaxOutputTokens::new(requested)
            .ok_or_else(|| "requested output budget must be positive".into())
    }
    fn anthropic_thinking(&self) -> Option<grapt_provider::ThinkingConfig> {
        None
    }
    fn request_encoding(&self) -> grapt_provider::RequestEncoding {
        Default::default()
    }
    fn image_input_supported(&self) -> bool {
        false
    }
    fn original_image_detail_supported(&self) -> bool {
        false
    }
    /// Required reasoning metadata declared by the compatible integration.
    fn compatible_reasoning_replay(&self) -> grapt_provider::CompatibleReasoningReplay {
        Default::default()
    }
    /// Whether the selected model's account catalog marks it Responses Lite.
    /// The reasoning the selected model's account catalog asks for.
    fn codex_reasoning(&self) -> grapt_provider::CodexReasoning {
        grapt_provider::CodexReasoning::default()
    }
}

/// The visibility a coding attempt grants each tool it offers. A provider
/// compaction call offers the same tools, so it grants the same.
pub(super) fn tool_visibilities(
    tools: &[grapt_provider::ToolDefinition],
) -> Result<Vec<ToolVisibility>, String> {
    tools
        .iter()
        .map(|definition| tool_visibility(&definition.name))
        .collect()
}

/// The visibility a coding attempt grants provider-hosted web search, when
/// its request offers it. A summary or provider compaction that extends that
/// request offers the same search, so it grants the same; `None` when the
/// request offers no hosted search.
pub(super) fn hosted_visibility(
    request: &ProviderRequest,
) -> Option<Result<ToolVisibility, String>> {
    let offered = match request {
        ProviderRequest::OpenAiResponses(request) => request.web_search.is_some(),
        ProviderRequest::ChatGptCodex(request) => request.web_search.is_some(),
        _ => false,
    };
    offered.then(|| tool_visibility("web_search"))
}

fn tool_visibility(name: &str) -> Result<ToolVisibility, String> {
    let required = CapabilityId::derive(&ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::default(),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    ));
    Ok(ToolVisibility {
        tool: ToolName::new(name.to_owned())
            .ok_or("product tool definition has an invalid name")?,
        required,
        visible: true,
    })
}

/// Resolve the production endpoint at the composition edge. The loopback seam
/// exists only under its named testkit feature; ordinary builds always return
/// the provider's production endpoint policy.
pub(crate) fn provider_endpoint() -> Result<super::LiveEndpoint, String> {
    #[cfg(feature = "loopback-provider-testkit")]
    if let Some(base) = std::env::var_os("GRAPT_LIVE_HEADLESS_LOOPBACK") {
        let base = base
            .into_string()
            .map_err(|_| "loopback provider endpoint is not UTF-8".to_owned())?;
        let base = grapt_provider::LoopbackBase::parse(&base).map_err(|_| {
            "loopback provider endpoint must have exact bounded IPv4 loopback authority".to_owned()
        })?;
        return Ok(super::LiveEndpoint::Loopback(base));
    }
    Ok(super::LiveEndpoint::Production(
        grapt_provider::EndpointConfig::new(),
    ))
}

pub(super) struct ProductPlan {
    ephemeral: bool,
    retry_feedback: Option<super::session_status::RetryFeedback>,
    family: ProtocolFamily,
    web_search: Option<grapt_provider::WebSearchSpec>,
    prompt: String,
    prompt_cache_key: PromptCacheKey,
    first_target: Option<ResolvedModelTarget>,
    targets: Arc<dyn ProductTargetAuthority>,
    /// What this execution's configured MCP servers publish, snapshotted once
    /// before planning began. Held rather than re-read so every attempt in one
    /// execution offers the model the same catalog.
    published: Arc<super::mcp::PublishedTools>,
    context: PlanContext,
    text_only: bool,
    initial_step: StepNumber,
    instructions: Option<super::instructions::EffectiveInstructions>,
    continuity_resume: Option<super::stateengine_resume::ResumeContext>,
    preparation_progress: tokio::sync::watch::Sender<grapt_runtime::step::ContextPreparationStage>,
    native_mail: Option<super::native_mail::custody::NativeMailPlan>,
    accounting: super::continuation::admission::Accounting,
    admission_diagnosis: super::continuation::admission::DiagnosisSlot,
    trim_feedback: Option<super::execution::context_trim::TrimFeedback>,
    trimmed_request: bool,
    /// How many items this request carries that compaction could remove:
    /// earlier inputs before the current one, and model outputs, replays and
    /// tool results from any turn or step. With none, it has nothing to do.
    compactable_items: usize,
    /// Where this request's history starts when a trimmed retry reads from
    /// the newest window the read bounds serve; what lies before is omitted.
    window_floor: Option<grapt_types::EventSeq>,
    context_hook: Option<ContextHook>,
    lifecycle: Option<super::hooks::plan::LifecyclePlan>,
    /// The Session's `compaction` config: threshold on/off and headroom.
    compaction: super::compaction_config::CompactionSettings,
    /// This execution's Codex sticky-routing token, shared by its requests.
    codex_turn: grapt_provider::CodexTurn,
    /// The Session's permission policy: a tool it withholds is never offered.
    policy: Option<super::permission_preset::ResolvedPolicy>,
    shell_description: Option<String>,
    output_continuation: output_continuation::State,
}

struct RequestBoundary {
    history_start: usize,
    protected_from: usize,
}

impl RequestBoundary {
    fn after_hook(&mut self, old_prefix: usize, new_prefix: usize, structural: bool) {
        self.history_start = if structural {
            new_prefix + self.history_start.saturating_sub(old_prefix)
        } else {
            new_prefix
        };
        self.protected_from = if structural {
            new_prefix + self.protected_from.saturating_sub(old_prefix)
        } else {
            // A hook may replace an unstructured conversation wholesale; its
            // new messages have no durable origin and stay protected together.
            new_prefix
        };
    }

    /// How many items compaction could remove: the unprotected history, and
    /// every model exchange outside it.
    fn compactable(&self, input: &[grapt_provider::CodexInputItem]) -> usize {
        let history = self.history_start..self.protected_from;
        let outside = input
            .iter()
            .enumerate()
            .filter(|(index, item)| !history.contains(index) && exchange(item))
            .count();
        history.len() + outside
    }
}

/// Whether `item` is part of a model exchange: output, replayed reasoning, a
/// tool call or its result. Instructions, user input and effort updates are
/// not; compaction can only remove exchanges.
fn exchange(item: &grapt_provider::CodexInputItem) -> bool {
    use grapt_provider::{CodexInputItem, InputMessage, MessageRole};
    match item {
        CodexInputItem::Message(InputMessage {
            role: MessageRole::System | MessageRole::User,
            ..
        })
        | CodexInputItem::ImageUser { .. }
        | CodexInputItem::EffortUpdate(_) => false,
        CodexInputItem::Message(InputMessage {
            role: MessageRole::Assistant,
            ..
        })
        | CodexInputItem::AssistantResponseStart
        | CodexInputItem::ProviderReplay(_)
        | CodexInputItem::CompatibleReasoning(_)
        | CodexInputItem::ToolCall { .. }
        | CodexInputItem::TextToolCall { .. }
        | CodexInputItem::ToolOutput { .. }
        | CodexInputItem::ImageToolOutput { .. }
        | CodexInputItem::CompactionTrigger => true,
    }
}

impl ProductPlan {
    fn prepare_request_accounting(&mut self) {
        let encoding = self.targets.request_encoding();
        // Ordinary admission retains its conservative byte charge. Encoded
        // sizing belongs to the explicit recovery after unusable compaction.
        let recovery = matches!(&self.context, PlanContext::Durable(continuation) if continuation.trim_requested());
        self.accounting
            .set_recovery_encoding(recovery.then(|| encoding.clone()));
        self.admission_diagnosis.record_encoding(encoding);
    }

    fn prepare_context_hook(
        &self,
        target: &ResolvedModelTarget,
        input: &mut Vec<grapt_provider::CodexInputItem>,
        tools: &mut Vec<grapt_provider::ToolDefinition>,
        boundary: &mut RequestBoundary,
    ) -> Result<(), PlanRefusal> {
        if let Some(hook) = &self.context_hook {
            apply_session_context_hook(hook, target, input, tools, boundary)
                .map_err(context_trim::refusal)?;
        }
        Ok(())
    }

    fn finalize_context(
        &mut self,
        request: &mut ProviderRequest,
        boundary: &mut RequestBoundary,
        context: Option<grapt_types::RequestContextIdentity>,
    ) -> Result<Option<grapt_types::RequestContextIdentity>, PlanRefusal> {
        if self.trim_context(request, boundary)? {
            self.admission_diagnosis.discard_partial_projection();
            return Ok(None);
        }
        self.admission_diagnosis
            .record_request(request, boundary.history_start)
            .map_err(|failure| {
                context_trim::refusal(format!(
                    "coding request admission snapshot failed: {failure:?}"
                ))
            })?;
        Ok(context)
    }

    pub(super) fn with_ephemeral(mut self, ephemeral: bool) -> Self {
        self.ephemeral = ephemeral;
        self
    }

    pub(super) fn with_trim_feedback(
        mut self,
        feedback: super::execution::context_trim::TrimFeedback,
    ) -> Self {
        self.trim_feedback = Some(feedback);
        self
    }

    pub(super) fn with_retry_feedback(
        mut self,
        feedback: super::session_status::RetryFeedback,
    ) -> Self {
        self.retry_feedback = Some(feedback);
        self
    }

    pub(super) fn with_codex_session(mut self, session: grapt_types::SessionId) -> Self {
        self.codex_turn = grapt_provider::CodexTurn::for_session(session);
        self
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "every argument is a distinct plan input with no natural grouping;                   bundling them into a struct would only move the same list"
    )]
    pub(super) fn new(
        family: ProtocolFamily,
        prompt: String,
        prompt_cache_key: PromptCacheKey,
        first_target: Option<ResolvedModelTarget>,
        targets: Arc<dyn ProductTargetAuthority>,
        published: Arc<super::mcp::PublishedTools>,
        continuation: DurableContinuation,
        initial_step: StepNumber,
    ) -> Self {
        Self {
            ephemeral: false,
            retry_feedback: None,
            family,
            web_search: None,
            prompt,
            prompt_cache_key,
            first_target,
            targets,
            published,
            text_only: false,
            context: PlanContext::Durable(continuation),
            initial_step,
            instructions: None,
            continuity_resume: None,
            preparation_progress: tokio::sync::watch::channel(
                grapt_runtime::step::ContextPreparationStage::Context,
            )
            .0,
            native_mail: None,
            accounting: super::continuation::admission::Accounting::default(),
            codex_turn: grapt_provider::CodexTurn::default(),
            admission_diagnosis: super::continuation::admission::DiagnosisSlot::default(),
            trim_feedback: None,
            trimmed_request: false,
            compactable_items: 0,
            window_floor: None,
            context_hook: None,
            lifecycle: None,
            compaction: super::compaction_config::CompactionSettings::default(),
            policy: None,
            shell_description: None,
            output_continuation: output_continuation::State::default(),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the transient constructor mirrors the ordinary plan coordinates"
    )]
    pub(super) fn transient(
        family: ProtocolFamily,
        prompt: String,
        prompt_cache_key: PromptCacheKey,
        first_target: ResolvedModelTarget,
        targets: Arc<dyn ProductTargetAuthority>,
        published: Arc<super::mcp::PublishedTools>,
        context: Vec<grapt_provider::CodexInputItem>,
        initial_step: StepNumber,
    ) -> Self {
        Self {
            ephemeral: false,
            retry_feedback: None,
            family,
            web_search: None,
            prompt,
            prompt_cache_key,
            first_target: Some(first_target),
            targets,
            published,
            text_only: false,
            context: PlanContext::Transient(context),
            initial_step,
            instructions: None,
            continuity_resume: None,
            preparation_progress: tokio::sync::watch::channel(
                grapt_runtime::step::ContextPreparationStage::Context,
            )
            .0,
            native_mail: None,
            accounting: super::continuation::admission::Accounting::default(),
            codex_turn: grapt_provider::CodexTurn::default(),
            admission_diagnosis: super::continuation::admission::DiagnosisSlot::default(),
            trim_feedback: None,
            trimmed_request: false,
            compactable_items: 0,
            window_floor: None,
            context_hook: None,
            lifecycle: None,
            compaction: super::compaction_config::CompactionSettings::default(),
            policy: None,
            shell_description: None,
            output_continuation: output_continuation::State::default(),
        }
    }

    /// An ephemeral side question offers no effects, including built-in tools.
    pub(super) fn text_only(mut self) -> Self {
        self.text_only = true;
        self
    }

    pub(super) fn with_web_search(mut self, search: Option<grapt_provider::WebSearchSpec>) -> Self {
        self.web_search = search;
        self
    }

    pub(super) fn with_mail_owner(
        mut self,
        mail: super::native_mail::custody::NativeMailPlan,
    ) -> Self {
        self.native_mail = Some(mail);
        self
    }

    pub(super) fn with_admission_diagnosis(
        mut self,
        diagnosis: super::continuation::admission::DiagnosisSlot,
    ) -> Self {
        self.admission_diagnosis = diagnosis;
        self
    }

    pub(super) fn with_policy(mut self, policy: super::permission_preset::ResolvedPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    pub(super) fn with_shell_description(mut self, description: String) -> Self {
        self.shell_description = Some(description);
        self
    }

    pub(super) fn with_compaction(
        mut self,
        compaction: super::compaction_config::CompactionSettings,
    ) -> Self {
        self.compaction = compaction;
        self
    }

    /// Carry this Session's confirmed-usage calibration across executions.
    pub(super) fn with_session_calibration(
        mut self,
        sessions: super::continuation::admission::SessionCalibrations,
        session: grapt_types::SessionId,
    ) -> Self {
        self.accounting =
            super::continuation::admission::Accounting::for_session(sessions, session);
        self
    }

    pub(super) fn with_continuity_resume(
        mut self,
        resume: super::stateengine_resume::ResumeContext,
    ) -> Self {
        self.continuity_resume =
            Some(resume.with_preparation_progress(self.preparation_progress.clone()));
        self
    }

    #[cfg(test)]
    pub(super) fn with_native_mail(mut self, mail: super::native_mail::NativeMailContext) -> Self {
        self.native_mail = Some(super::native_mail::custody::NativeMailPlan::private(mail));
        self
    }

    pub(super) fn with_instructions(
        mut self,
        instructions: super::instructions::EffectiveInstructions,
    ) -> Self {
        self.instructions = Some(instructions);
        self
    }

    pub(super) fn with_lifecycle(
        mut self,
        runtime: Arc<super::hooks::HookRuntime>,
        session: grapt_types::SessionId,
    ) -> Self {
        self.lifecycle = Some(super::hooks::plan::LifecyclePlan::new(runtime, session));
        self
    }

    pub(super) fn with_context_hook(
        mut self,
        plugins: Arc<super::plugins::PluginRuntime>,
        generation: Arc<grapt_plugin::PluginGeneration>,
        session: grapt_types::SessionId,
        agent: grapt_types::AgentId,
        principal: grapt_api::SealedPrincipal,
    ) -> Self {
        self.context_hook = Some(ContextHook {
            plugins,
            generation,
            session,
            agent,
            principal,
        });
        self
    }
}

impl ProductPlan {
    fn admit_context(
        &mut self,
        planned: &PlannedAttempt,
    ) -> Result<Option<grapt_types::CompactionTrigger>, ContextAdmissionRefusal> {
        let continuation = match &self.context {
            PlanContext::Durable(continuation) => Some(continuation.share()),
            PlanContext::Transient(_) => None,
        };
        match continuation {
            Some(continuation) => continuation.read_for_context_admission(|reader| {
                self.context_admission_at(planned, Some(reader))
            }),
            None => self
                .context_admission_at(planned, None)
                .map_err(ContextAdmissionRefusal::from),
        }
    }

    fn context_admission_at(
        &mut self,
        planned: &PlannedAttempt,
        continuation: Option<&DurableContinuation>,
    ) -> Result<Option<grapt_types::CompactionTrigger>, grapt_types::ContextFailure> {
        use crate::dogfood::continuation::admission;
        let budget = self
            .targets
            .context_tokens()
            .ok_or(grapt_types::ContextFailure::CompactionFailed)?;
        let confirmed = match continuation {
            Some(continuation) => self
                .accounting
                .previous_attempt()
                .map(|attempt| continuation.confirmed_usage(attempt))
                .transpose()?,
            None => None,
        };
        // A compacted/rebuilt context no longer contains the old request;
        // its pre-compaction usage must not trigger another compaction.
        let context_usage = confirmed
            .filter(|_| !self.trimmed_request)
            .filter(|_| self.accounting.previous_context_is_retained(planned))
            .and_then(|usage| usage.context);
        let mut estimate = self.accounting.estimate(
            &planned.request,
            planned.authority.model(),
            confirmed.and_then(|usage| usage.input),
        )?;
        if self.trimmed_request {
            estimate.tokens = self
                .targets
                .request_encoding()
                .estimate(&planned.request)
                .map_err(|_| grapt_types::ContextFailure::CompactionFailed)?;
            estimate.basis = admission::EstimateBasis::Estimated;
        }
        // Compaction cannot shrink a request with nothing compactable, so refusing it is
        // final. Before any calibration the byte bound only proves it too large
        // when even the densest tokenizer could not fit it; otherwise the
        // provider's own count decides, and its usage calibrates the next one.
        // Both the ceiling and the window judge this one size.
        let judged = if self.compactable_items == 0
            && estimate.basis == admission::EstimateBasis::Estimated
        {
            admission::densest(&planned.request, estimate.tokens)
        } else {
            estimate.tokens
        };
        self.accounting.record_context(planned.context.as_ref());
        self.admission_diagnosis
            .record_calibration(self.accounting.calibration_for(planned.authority.model()));
        self.admission_diagnosis
            .record_tokens(estimate.tokens, confirmed.and_then(|usage| usage.input));
        let trigger = grapt_types::CompactionTrigger::PolicyThreshold;
        self.admission_diagnosis
            .record(admission::AdmissionDiagnosis {
                trigger,
                estimate_basis: estimate.basis,
                // The number the threshold was judged on.
                estimate: judged,
                budget,
            });
        // Universal ceiling: old auto/suppression settings cannot admit more
        // work past it. The compactor reports repetition/recovery refusals.
        //
        // When the previous request's context is retained but its provider
        // reported no usage, judge the ceiling on this request's local estimate:
        // otherwise a provider that omits usage leaves 90% of the window
        // unreachable and the Session runs until the provider overflows.
        if self.compaction.auto
            && admission::auto_compact_reached(
                admission::ceiling_size(context_usage, judged.into()),
                budget,
                self.compaction.model_auto_compact_token_limit,
            )
        {
            return Ok(Some(trigger));
        }
        let reserve = admission::output_reserve(&planned.request);
        // Compacting again with nothing new since the last compaction would
        // only re-summarize its summary, and so would compacting history whose
        // every call repeats what the last compaction summarized (the reread
        // loop of CONTEXT-QUALITY-01 J04). The threshold then only decides
        // when to compact, as the oracle's is: the request goes out while it
        // fits the model's window, and only a request that does not still
        // asks to compact, which then fails as no progress.
        let compactable = match continuation {
            Some(continuation) => {
                continuation
                    .compactable_since_compaction()
                    .map_err(|_| grapt_types::ContextFailure::CompactionFailed)?
                    && continuation
                        .repeated_since_compaction()
                        .map_err(|_| grapt_types::ContextFailure::CompactionFailed)?
                        .is_none()
            }
            None => true,
        };
        if !compactable {
            return Ok((!admission::fits(judged, reserve, budget)).then_some(trigger));
        }
        // `compaction.auto: false` turns off the proactive ceiling above; a
        // request that cannot fit the window still compacts, as the provider's
        // own overflow would make it, and so do a structural bound and a
        // requested compaction.
        //
        // One honest size guards the window: the request at this target's
        // observed ratio, the count the provider charges for the same bytes. A
        // margin here compacted a Session at about half its real usage and
        // refused a summary that fit. An under-count instead reaches the
        // provider's own overflow classification, which the Step recovers with
        // its single bounded rebuild.
        Ok((!admission::fits(judged, reserve, budget)).then_some(trigger))
    }
}

impl grapt_runtime::AttemptPlan for ProductPlan {
    fn retrying(&mut self, notice: Option<grapt_runtime::step::RetryNotice>) {
        if let Some(feedback) = &self.retry_feedback {
            feedback.observe(notice);
        }
    }

    fn promote_input(&mut self, input: &grapt_types::MessageId) -> Result<(), String> {
        if let Some(hooks) = &mut self.lifecycle {
            hooks.promoted();
        }
        if let PlanContext::Durable(continuation) = &mut self.context {
            self.prompt = continuation.promote_input(input)?;
        }
        Ok(())
    }

    fn output_continuation(&mut self, enabled: bool) -> bool {
        let supported = matches!(&self.context, PlanContext::Durable(_));
        self.output_continuation.set(enabled && supported);
        supported
    }

    fn admitted(&mut self, attempt: &grapt_types::PhysicalAttemptId) {
        self.accounting.admitted(attempt);
        self.admission_diagnosis.sent();
    }

    fn context_admission(
        &mut self,
        planned: &PlannedAttempt,
    ) -> Result<Option<grapt_types::CompactionTrigger>, ContextAdmissionRefusal> {
        self.admit_context(planned)
    }

    fn context_preparation_progress(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<grapt_runtime::step::ContextPreparationStage>> {
        Some(self.preparation_progress.subscribe())
    }

    fn refresh_context(
        &mut self,
        step: StepNumber,
        _generation: HistoryGeneration,
    ) -> grapt_runtime::BoxFuture<
        '_,
        Result<(), grapt_runtime::provider_source::ContextPreparationFailure>,
    > {
        Box::pin(async move {
            use grapt_runtime::step::ContextPreparationStage as Stage;
            if let Some(hooks) = &mut self.lifecycle {
                self.preparation_progress.send_replace(Stage::Lifecycle);
                hooks.prepare().await;
            }
            if let Some(resume) = &mut self.continuity_resume {
                self.preparation_progress.send_replace(Stage::Continuity);
                match resume.refresh().await {
                    Ok(()) => {}
                    // An outage never fails the turn; it proceeds and says why.
                    Err(failure) if resume.degrades(failure) => resume.degrade(failure),
                    Err(failure) => {
                        crate::diagnostics::report(format!(
                            "stateengine-continuity stage=resume failure={failure:?}"
                        ));
                        return Err(
                            grapt_runtime::provider_source::ContextPreparationFailure::Continuity(
                                failure,
                            ),
                        );
                    }
                }
            }
            if let Some(mail) = &mut self.native_mail {
                self.preparation_progress.send_replace(Stage::NativeMail);
                let budget = super::stateengine_resume::INBOX_PREPARATION_BUDGET;
                let refreshed = tokio::time::timeout(budget, mail.refresh(step))
                    .await
                    .unwrap_or(Err(
                        super::stateengine_resume::ContinuityUnavailable::TimedOut {
                            seconds: budget.as_secs(),
                        },
                    ));
                match refreshed {
                    Ok(()) => {}
                    Err(failure) if failure.degrades_mail() => mail.degrade(failure),
                    Err(failure) => {
                        crate::diagnostics::report(format!(
                            "stateengine-continuity stage=inbox failure={failure:?}"
                        ));
                        return Err(
                            grapt_runtime::provider_source::ContextPreparationFailure::Continuity(
                                failure,
                            ),
                        );
                    }
                }
            }
            Ok(())
        })
    }

    fn finish_context(
        &mut self,
        cancelled: bool,
    ) -> grapt_runtime::BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            if let Some(mail) = &mut self.native_mail {
                mail.finish(cancelled).await?;
            }
            Ok(())
        })
    }

    fn shutdown(&mut self) -> grapt_runtime::BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let hooks = match &self.lifecycle {
                Some(hooks) => hooks.stop().await,
                None => Ok(()),
            };
            let mail = match &mut self.native_mail {
                Some(mail) => mail.shutdown().await,
                None => Ok(()),
            };
            mail.and(hooks)
        })
    }

    fn plan(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        _generation: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        if let Some(hooks) = &self.lifecycle {
            hooks
                .check()
                .map_err(|detail| PlanRefusal::new(detail, None))?;
        }
        self.preparation_progress
            .send_replace(grapt_runtime::step::ContextPreparationStage::Request);
        self.admission_diagnosis.clear();
        // Read before the history, so the request carries nothing past it.
        if let PlanContext::Durable(continuation) = &self.context {
            let coverage = continuation
                .read_for_plan(|reader| reader.planned_coverage().map_err(PlanRefusal::from))?;
            self.admission_diagnosis.record_coverage(coverage);
        }
        // Manual compaction still precedes the first coding dispatch and
        // wins over structural/threshold refusals. Prepare the ordinary request
        // first so the existing warm summary can extend its cached prefix.
        // A failed preparation leaves that owner its bounded cold fallback.
        let requested = if step == self.initial_step && ordinal == 1 {
            match &self.context {
                PlanContext::Durable(continuation) => continuation.read_for_plan(|reader| {
                    reader.compaction_requested().map_err(|failure| {
                        PlanRefusal::new(
                            format!("requested compaction is unreadable: {failure:?}"),
                            None,
                        )
                    })
                })?,
                PlanContext::Transient(_) => false,
            }
        } else {
            false
        };
        let planned = self.coding_request(step);
        if planned
            .as_ref()
            .is_err_and(|refusal| refusal.cause == PlanRefusalCause::CapacityBusy)
        {
            return planned;
        }
        if requested {
            return Err(PlanRefusal::new(
                "a requested compaction runs before this input".to_owned(),
                Some(grapt_types::CompactionTrigger::UserRequested),
            ));
        }
        planned
    }
}

/// The oracle's rule for which models get explicit cache breakpoints
/// (`ProviderTransform`): an Anthropic provider, or a model whose id names
/// Anthropic or Claude, as OpenRouter's `anthropic/claude-*` ids do.
fn compatible_prompt_cache(model: &grapt_types::ModelRef) -> grapt_provider::CompatiblePromptCache {
    let id = model.model().as_str();
    if model.provider().as_str() == "anthropic" || id.contains("anthropic") || id.contains("claude")
    {
        grapt_provider::CompatiblePromptCache::AnthropicBreakpoints
    } else {
        grapt_provider::CompatiblePromptCache::Off
    }
}

#[cfg(test)]
#[path = "production/tests.rs"]
pub(super) mod tests;

pub(in crate::dogfood) mod output_continuation;
