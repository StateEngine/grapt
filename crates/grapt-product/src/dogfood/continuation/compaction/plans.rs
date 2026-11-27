//! Warm and cold summary request construction.
//! Prefix-preserving retries.
//! Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
//! Provided AS IS; see LICENSE-APACHE for the license and warranty disclaimer.
use super::retained::SummaryPlan;
use super::*;

/// A summary is one bounded text answer, never a reasoning call.
///
/// Every summary request funnels through here: the warm plan extends a coding
/// request, so it inherits that request's thinking mode, and the summary's
/// 8192-token output bound is below what a thinking budget needs. Leaving
/// thinking on made the provider refuse or truncate the call, which the
/// recovery registry then remembered as a permanent failure.
///
/// This is a trade-off, not a cache-preserving change. Anthropic documents the
/// thinking mode as part of the request the message cache is keyed on, so a
/// warm coding request issued with thinking and extended here without it
/// invalidates the cached prefix: the summary pays a cold prefill of a context
/// that is near the ceiling, in latency and input tokens. Dropping thinking is
/// still the right call, because a summary that a thinking budget truncates or
/// the provider refuses never commits, and a cold prefill that succeeds beats a
/// warm request that cannot. The cost is made visible instead of claimed away:
/// the warm plan records [`SummaryPlan::WarmTextOnly`], and the summary call's
/// own terminal usage carries the cache-read count, so a benchmark counts cold
/// summaries and their fresh input directly.
pub(super) fn text_only_summary(request: ProviderRequest) -> ProviderRequest {
    match request {
        ProviderRequest::AnthropicMessages(mut request) => {
            request.thinking = None;
            ProviderRequest::AnthropicMessages(request)
        }
        ProviderRequest::VertexAnthropic(mut request) => {
            request.thinking = None;
            ProviderRequest::VertexAnthropic(request)
        }
        other => other,
    }
}

/// Whether [`text_only_summary`] changes this request, so the plan line can say
/// that the cache is being traded for a summarizable answer.
fn drops_thinking(request: &ProviderRequest) -> bool {
    match request {
        ProviderRequest::AnthropicMessages(request) | ProviderRequest::VertexAnthropic(request) => {
            request.thinking.is_some()
        }
        _ => false,
    }
}

impl ProductCompactor {
    fn with_manual_instructions(&self, text: &str) -> String {
        match &self.manual_instructions {
            Some(instructions) => {
                format!("{text}\n\nInstructions for this compaction:\n{instructions}")
            }
            None => text.to_owned(),
        }
    }

    /// The cache-warm summary request: the coding request that asked to
    /// compact, its instructions, tools and input unchanged, and one last user
    /// message with the summary task. The tools stay offered, byte for byte,
    /// for the cached prefix, and are not callable (`tool_choice: none`), except
    /// on Anthropic, which documents a changed tool_choice as invalidating the
    /// cached message blocks, and Bedrock, which has no native "none": there
    /// a tool call is refused before any effect and the summary counts as
    /// unusable. Hosted web search stays offered and
    /// granted too, so the offered tool set matches.
    /// `Err` names why the summary goes cold instead: no coding request to
    /// extend, a hosted search the attempt could not grant, or a changed
    /// issued model.
    ///
    /// `Ok` carries the plan to record with the descriptor and whether this
    /// warm request drops the thinking mode. That second fact is RUNTIME ONLY:
    /// it is not a persisted label, because the previous build's decoder
    /// refuses any plan name it does not know and retained-context loading
    /// would then fail after a rollback. A reader that needs the fact logs it
    /// where the request is built.
    pub(super) fn warm_plan(
        &self,
    ) -> Result<Result<(PlannedAttempt, SummaryPlan, bool), SummaryPlan>, String> {
        let Some(mut request) = self.admission_diagnosis.coding_request() else {
            return Ok(Err(SummaryPlan::ColdNoCodingRequest));
        };
        // Whether this coding request carries a thinking mode the summary must
        // drop; that decision changes the plan line, because it costs the cache.
        let text_only = drops_thinking(&request);
        let mut task = super::super::summarization::warm_task_item();
        if let CodexInputItem::Message(message) = &mut task {
            message.content = self.with_manual_instructions(&message.content);
        }
        let bound = || {
            grapt_provider::MaxOutputTokens::new(8192).ok_or("summary output token bound invalid")
        };
        match &mut request {
            ProviderRequest::OpenAiResponses(request) => {
                request.input.push(task);
                request.tool_choice = Some(grapt_provider::ToolChoice::None);
                request.max_output_tokens = Some(bound()?);
            }
            ProviderRequest::ChatGptCodex(request) => {
                request.input.push(task);
                request.tool_choice = Some(grapt_provider::ToolChoice::None);
            }
            ProviderRequest::BedrockConverse(request) => {
                request.messages.push(task);
                request.tool_choice = Some(grapt_provider::ToolChoice::None);
                request.max_tokens = bound()?;
            }
            ProviderRequest::GoogleGemini(request) | ProviderRequest::VertexGemini(request) => {
                request.messages.push(task);
                request.tool_choice = Some(grapt_provider::ToolChoice::None);
                request.max_tokens = Some(bound()?);
            }
            ProviderRequest::CohereChat(request) => {
                request.messages.push(task);
                request.tool_choice = Some(grapt_provider::ToolChoice::None);
                request.max_tokens = Some(self.targets.output_bound(8192)?);
            }
            ProviderRequest::OpenAiCompatible(request) | ProviderRequest::WatsonxChat(request) => {
                request.messages.push(task);
                request.tool_choice = Some(grapt_provider::ToolChoice::None);
                request.max_tokens = Some(
                    grapt_provider::request::MaxOutputTokens::new(8192)
                        .ok_or("summary output token bound invalid")?,
                );
            }
            // tool_choice stays the coding request's: Anthropic documents a
            // changed tool_choice as invalidating cached message blocks. A tool
            // call is refused by the summary owner and counts as unusable.
            ProviderRequest::AnthropicMessages(request)
            | ProviderRequest::VertexAnthropic(request) => {
                request.messages.push(task);
                request.max_tokens = grapt_provider::MaxOutputTokens::new(
                    self.targets.anthropic_max_tokens()?.value().min(8192),
                )
                .ok_or("summary output token bound invalid")?;
            }
        }
        self.bind_summary(request)
            .map(|bound| bound.map(|planned| (planned, SummaryPlan::Warm, text_only)))
    }

    fn bind_summary(
        &self,
        request: ProviderRequest,
    ) -> Result<Result<PlannedAttempt, SummaryPlan>, String> {
        let request = text_only_summary(request);
        let target = self.targets.issue()?;
        let mut builder = PhysicalAttemptAuthoritySnapshot::builder();
        builder.model(target).map_err(|error| error.to_string())?;
        for visibility in super::super::super::production::tool_visibilities(request.tools())
            .map_err(|error| format!("{error:?}"))?
        {
            builder
                .tool(visibility)
                .map_err(|error| error.to_string())?;
        }
        // Hosted search stays offered, granted as the coding attempt granted
        // it, and `tool_choice: none` still applies. Only when that grant
        // cannot be made does the summary go cold, counted.
        if let Some(visibility) = super::super::super::production::hosted_visibility(&request) {
            if visibility
                .ok()
                .and_then(|visibility| builder.tool(visibility).ok())
                .is_none()
            {
                return Ok(Err(SummaryPlan::ColdHostedToolOffered));
            }
        }
        let authority = builder.build().map_err(|error| error.to_string())?;
        if authority.model().model_ref() != request.model() {
            return Ok(Err(SummaryPlan::ColdIssuedModelChanged));
        }
        Ok(Ok(PlannedAttempt {
            operation_mail: None,
            mail: None,
            authority,
            request,
            context: None,
        }))
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn plan(&self, text: &str) -> Result<PlannedAttempt, String> {
        let target = self.targets.issue()?;
        let mut builder = PhysicalAttemptAuthoritySnapshot::builder();
        builder.model(target).map_err(|error| error.to_string())?;
        let authority = builder.build().map_err(|error| error.to_string())?;
        let input = vec![
            CodexInputItem::Message(InputMessage {
                role: MessageRole::System,
                content: SUMMARY_INSTRUCTION.to_owned(),
            }),
            CodexInputItem::Message(InputMessage {
                role: MessageRole::User,
                content: self.with_manual_instructions(text),
            }),
        ];
        let request = match self.targets.protocol_family() {
            ProtocolFamily::BedrockConverse => {
                ProviderRequest::BedrockConverse(grapt_provider::BedrockConverseRequest {
                    model: authority.model().model_ref().clone(),
                    messages: input,
                    max_tokens: grapt_provider::MaxOutputTokens::new(8192)
                        .ok_or("summary output token bound invalid")?,
                    tools: Vec::new(),
                    tool_choice: None,
                })
            }
            ProtocolFamily::OpenAiResponses => {
                ProviderRequest::OpenAiResponses(grapt_provider::OpenAiResponsesRequest {
                    model: authority.model().model_ref().clone(),
                    input,
                    prompt_cache_key: None,
                    max_output_tokens: Some(
                        grapt_provider::MaxOutputTokens::new(8192)
                            .ok_or("summary output token bound invalid")?,
                    ),
                    tools: Vec::new(),
                    web_search: None,
                    tool_choice: None,
                })
            }
            family @ (ProtocolFamily::AnthropicMessages | ProtocolFamily::VertexAnthropic) => {
                let request = grapt_provider::AnthropicMessagesRequest {
                    web_search: None,
                    uncached_system_suffix: None,
                    effort: grapt_provider::EffortPolicy::default(),
                    instruction_mode: grapt_provider::AnthropicInstructionMode::CurrentPrefix,
                    model: authority.model().model_ref().clone(),
                    messages: input,
                    max_tokens: grapt_provider::MaxOutputTokens::new(
                        self.targets.anthropic_max_tokens()?.value().min(8192),
                    )
                    .ok_or("summary output token bound invalid")?,
                    thinking: None,
                    prompt_cache: grapt_provider::AnthropicPromptCache::Off,
                    tools: Vec::new(),
                    tool_choice: None,
                };
                if family == ProtocolFamily::VertexAnthropic {
                    ProviderRequest::VertexAnthropic(request)
                } else {
                    ProviderRequest::AnthropicMessages(request)
                }
            }
            ProtocolFamily::ChatGptCodex => {
                ProviderRequest::ChatGptCodex(grapt_provider::ChatGptCodexRequest {
                    model: authority.model().model_ref().clone(),
                    input,
                    prompt_cache_key: None,
                    tools: Vec::new(),
                    web_search: None,
                    tool_choice: None,
                    reasoning: self.targets.codex_reasoning(),
                    turn: grapt_provider::CodexTurn::for_session(self.continuation.session.clone()),
                })
            }
            ProtocolFamily::GoogleGemini => {
                ProviderRequest::GoogleGemini(grapt_provider::GoogleGeminiRequest {
                    model: authority.model().model_ref().clone(),
                    messages: input,
                    max_tokens: grapt_provider::MaxOutputTokens::new(8192),
                    tools: Vec::new(),
                    tool_choice: None,
                })
            }
            ProtocolFamily::CohereChat => {
                ProviderRequest::CohereChat(grapt_provider::CohereChatRequest {
                    model: authority.model().model_ref().clone(),
                    messages: input,
                    max_tokens: Some(self.targets.output_bound(8192)?),
                    tools: Vec::new(),
                    tool_choice: None,
                })
            }
            ProtocolFamily::VertexGemini => {
                ProviderRequest::VertexGemini(grapt_provider::GoogleGeminiRequest {
                    model: authority.model().model_ref().clone(),
                    messages: input,
                    max_tokens: grapt_provider::MaxOutputTokens::new(8192),
                    tools: Vec::new(),
                    tool_choice: None,
                })
            }
            family @ (ProtocolFamily::OpenAiCompatible | ProtocolFamily::WatsonxChat) => {
                let request = grapt_provider::OpenAiCompatibleRequest {
                    reasoning_replay: Default::default(),
                    prompt_cache: grapt_provider::CompatiblePromptCache::Off,
                    model: authority.model().model_ref().clone(),
                    messages: input,
                    max_tokens: Some(
                        grapt_provider::request::MaxOutputTokens::new(8192)
                            .ok_or("summary output token bound invalid")?,
                    ),
                    tools: Vec::new(),
                    tool_choice: None,
                    routing_session: (family == ProtocolFamily::OpenAiCompatible)
                        .then(|| self.routing_session.clone())
                        .flatten(),
                };
                if family == ProtocolFamily::WatsonxChat {
                    ProviderRequest::WatsonxChat(request)
                } else {
                    ProviderRequest::OpenAiCompatible(request)
                }
            }
        };
        Ok(PlannedAttempt {
            operation_mail: None,
            mail: None,
            authority,
            request,
            context: None,
        })
    }
}

/// How many times a summary request that overflows the window is retried
/// with less of the oldest history.
pub(super) const MAX_OVERFLOW_TRIES: u32 = grapt_runtime::MAX_SUMMARY_CALLS;

impl ProductCompactor {
    /// The bytes of history a summary quote may hold: what admission's
    /// calibrated charge admits, less any manual instructions.
    pub(super) fn summary_history(&self, budget: u64) -> u64 {
        self.admission_diagnosis
            .summary_bytes(budget)
            .saturating_sub(
                self.manual_instructions
                    .as_ref()
                    .map_or(0, |text| text.len() as u64 + 128),
            )
    }

    /// The planned summary call: the quote, the warm or cold request that
    /// carries it, and which plan that was. The history is read and prepared
    /// once; a request that overflows the window is judged again with the
    /// same rows quoted less, the oldest exchanges dropped, up to four tries.
    pub(super) fn planned_summary(
        &mut self,
        id: &CompactionId,
        budget: u64,
        history: &mut u64,
        retry: Option<SummaryPreparation>,
    ) -> Result<(SummaryPreparation, PlannedAttempt, retained::SummaryPlan), CompactionFailure>
    {
        let overflow = |planned: &PlannedAttempt, warm: bool| {
            let overflow = if warm {
                self.admission_diagnosis
                    .warm_summary_overflow(&planned.request, budget)
            } else {
                self.admission_diagnosis
                    .summary_overflow(&planned.request, budget)
            };
            overflow.map_err(|failure| failed(format!("summary accounting failed: {failure:?}")))
        };
        // A call the provider refused as an overflow is made again cold with
        // less of the same quote: the warm request holds no quote to shrink.
        let (mut prepared, plan) = match retry {
            Some(mut prepared) => {
                let quoted = prepared
                    .requote(usize::try_from(*history).unwrap_or(usize::MAX))
                    .map_err(failed)?;
                if !quoted {
                    // The provider refused the summary as an overflow and
                    // nothing is left to quote: a context failure, as the
                    // refusal itself was.
                    self.failure_cause = Some(Cause::Context);
                    return Err(failed(
                        "the provider refused the summary as a context overflow and it cannot be made smaller",
                    ));
                }
                (prepared, retained::SummaryPlan::ColdWarmOverBudget)
            }
            None => {
                // The quoted history fills what admission's calibrated charge
                // admits.
                let prepared = self
                    .continuation
                    .prepare_summary_within(&self.prompt, id, *history)
                    .map_err(super::summarization::PreparationError::into_failure)?;
                // The first call is cache-warm where it fits: the coding
                // request plus the summary task, so the provider's cached
                // prefix covers it, judged at its own size in admission's
                // count; when it does not fit, the cold request (the history
                // quoted as data) does. Rejecting a provider response does not
                // invalidate the coding prefix. The fallback still offers the
                // same tools, with effects withheld.
                let (warm, plan, why) = match self.warm_plan().map_err(failed)? {
                    Ok((warm, _plan, text_only)) => match overflow(&warm, true)? {
                        None => {
                            if text_only {
                                // The persisted label stays `warm` so a
                                // previous build can still decode the retained
                                // selection. The cache cost is stated here,
                                // once, where it is incurred.
                                crate::diagnostics::report(format!(
                                    "session {}: compaction {id}: warm summary drops the thinking mode, so the provider's cached prefix is not reused",
                                    self.continuation.session
                                ));
                            }
                            (Some(warm), retained::SummaryPlan::Warm, String::new())
                        }
                        Some(why) => (
                            None,
                            retained::SummaryPlan::ColdWarmOverBudget,
                            format!(" (the warm request's {why})"),
                        ),
                    },
                    Err(plan) => (None, plan, String::new()),
                };
                crate::diagnostics::report(format!(
                    "session {}: compaction {id}: path: se-summary; summary plan: {}{why}",
                    self.continuation.session,
                    plan.name()
                ));
                if let Some(warm) = warm {
                    return Ok((prepared, warm, plan));
                }
                (prepared, plan)
            }
        };
        let mut shrinks = 0;
        loop {
            let cold = self.plan(&prepared.text).map_err(failed)?;
            let Some(why) = overflow(&cold, false)? else {
                return Ok((prepared, cold, plan));
            };
            *history -= *history / 8;
            let quoted = shrinks < MAX_OVERFLOW_TRIES
                && prepared
                    .requote(usize::try_from(*history).unwrap_or(usize::MAX))
                    .map_err(failed)?;
            if !quoted {
                self.failure_cause = Some(Cause::Context);
                return Err(CompactionFailure::NoProgress {
                    reason: format!(
                        "summary request plus output reserve exceeds context budget ({why})"
                    ),
                });
            }
            shrinks += 1;
            crate::diagnostics::report(format!(
                "session {}: compaction {id}: the summary request overflowed ({why}); dropping the oldest history, try {shrinks} of {MAX_OVERFLOW_TRIES}",
                self.continuation.session
            ));
        }
    }
}
