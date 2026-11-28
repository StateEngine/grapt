//! Build an ordinary coding request before choosing whether to send it.
use super::*;

struct PreparedRequest {
    authority: grapt_policy::SnapshotBuilder,
    request: ProviderRequest,
    context: Option<grapt_types::RequestContextIdentity>,
    mail: Option<grapt_runtime::mailbox::remote::NativeMailBatch>,
    operation_mail: Option<grapt_inbox::operation_mail::OperationMailBatch>,
}

impl PreparedRequest {
    fn finish(mut self, target: ResolvedModelTarget) -> Result<PlannedAttempt, PlanRefusal> {
        self.authority
            .model(target)
            .map_err(|error| PlanRefusal::new(error.to_string(), None))?;
        Ok(PlannedAttempt {
            authority: self
                .authority
                .build()
                .map_err(|error| PlanRefusal::new(error.to_string(), None))?,
            request: self.request,
            context: self.context,
            mail: self.mail,
            operation_mail: self.operation_mail,
        })
    }
}

/// Leave out the definitions the provider cannot carry, naming each with its
/// reason. The eligibility owner judges; this names the result for the notice.
fn judge_definitions(
    tools: Vec<grapt_provider::ToolDefinition>,
    excluded: &mut Vec<(String, &'static str)>,
) -> Vec<grapt_provider::ToolDefinition> {
    let (kept, left_out) = grapt_provider::tool_eligibility::admissible(tools);
    excluded.extend(
        left_out
            .into_iter()
            .map(|(name, reason)| (name, reason.as_str())),
    );
    kept
}

impl ProductPlan {
    pub(super) fn coding_request(
        &mut self,
        step: StepNumber,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        let target = match self.first_target.take() {
            Some(target) => target,
            None => self
                .targets
                .issue()
                .map_err(|detail| PlanRefusal::new(detail, None))?,
        };
        match self.prepare_coding_request(step, &target) {
            Ok(prepared) => prepared.finish(target),
            Err(refusal) => {
                if refusal.cause == PlanRefusalCause::CapacityBusy {
                    self.first_target = Some(target);
                }
                Err(refusal)
            }
        }
    }

    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn prepare_coding_request(
        &mut self,
        step: StepNumber,
        target: &ResolvedModelTarget,
    ) -> Result<PreparedRequest, PlanRefusal> {
        self.prepare_request_accounting();
        // A provider compaction is only ever sent to the target that made it.
        if let PlanContext::Durable(continuation) = &self.context {
            continuation.select_target_with_effort(
                target,
                self.family,
                self.targets.effort_policy(),
            );
        }
        // Judge every offered definition BEFORE the instruction epoch freezes
        // it, so an unusable tool never enters durable state and never fails a
        // turn. Each exclusion is reported once per catalog revision.
        // A definition the catalog itself withheld never reaches `offered`, so
        // its reason has to be carried here or the reader is told nothing about
        // a tool that silently vanished.
        let mut excluded: Vec<(String, &'static str)> = Vec::new();
        for withheld in &self.published.shadowed {
            excluded.push((withheld.name.clone(), "a built-in tool answers to its name"));
        }
        for withheld in &self.published.ambiguous {
            excluded.push((
                withheld.name.clone(),
                "more than one server publishes this tool name",
            ));
        }
        for withheld in &self.published.unpublished {
            excluded.push((withheld.name.clone(), "the published catalog had no room"));
        }
        let offered: Vec<_> = tool_catalog::offered(&self.published)
            .map_err(|detail| PlanRefusal::new(detail, None))?
            .into_iter()
            .filter(
                |tool| match grapt_provider::tool_eligibility::eligibility(tool) {
                    grapt_provider::tool_eligibility::Eligibility::Eligible => true,
                    grapt_provider::tool_eligibility::Eligibility::Excluded(reason) => {
                        excluded.push((tool.name.clone(), reason.as_str()));
                        false
                    }
                },
            )
            .collect();
        let mcp_guidance = tool_catalog::guidance(&self.published, self.policy.as_ref())
            .map_err(|detail| PlanRefusal::new(detail, None))?;
        let instructions = self
            .instructions
            .as_ref()
            .map(|instructions| {
                instructions.refresh_with_catalog(
                    Some((&offered, self.published.refresh_failed)),
                    Some(&mcp_guidance),
                )
            })
            .transpose()
            .map_err(|error| {
                let compaction = error.compaction_trigger();
                if compaction.is_some() {
                    self.admission_diagnosis.reuse_previous_request();
                }
                PlanRefusal::new(error.to_string(), compaction)
            })?;
        let mut tools = if self.ephemeral {
            super::super::tools::ephemeral_definitions(self.family)
        } else {
            super::super::tools::offered_definitions(self.family)
        };
        if let (Some(description), Some(tool)) = (
            &self.shell_description,
            tools.iter_mut().find(|tool| tool.name == "exec_command"),
        ) {
            tool.description = Some(description.clone());
        }
        tools.extend(
            instructions
                .as_ref()
                .and_then(|instructions| instructions.frozen_tools.as_deref())
                .unwrap_or(if self.published.refresh_failed {
                    &[]
                } else {
                    &offered
                })
                .iter()
                .cloned(),
        );
        tools.extend(tool_catalog::live(&self.published).cloned());
        // A tool the Session's permissions deny outright is not offered at
        // all, as the oracle withholds it; one denied in part stays offered
        // and is refused when a call names the denied part.
        if let Some(policy) = &self.policy {
            tools.retain(|tool| !policy.withholds(&tool.name));
        }
        let web_search = if matches!(
            self.family,
            ProtocolFamily::OpenAiResponses | ProtocolFamily::ChatGptCodex
        ) && self
            .policy
            .as_ref()
            .is_none_or(|policy| !policy.withholds("websearch"))
        {
            self.web_search.clone()
        } else {
            None
        };
        if self.text_only {
            tools.clear();
        }
        image_input::filter_tools(&mut tools, self.targets.image_input_supported());
        // The hook validates every schema it is shown, so judge before it too.
        tools = judge_definitions(tools, &mut excluded);
        if let PlanContext::Durable(continuation) = &self.context {
            continuation.select_compatible_reasoning(if tools.is_empty() {
                grapt_provider::CompatibleReasoningReplay::None
            } else {
                self.targets.compatible_reasoning_replay()
            });
        }
        let mut epochs = matches!(&self.context, PlanContext::Durable(_))
            && (!self.family.is_anthropic()
                || grapt_provider::supports_anthropic_system_updates(target.model_ref()));
        let mut input = instructions
            .as_ref()
            .map(|instructions| {
                if epochs {
                    instructions.prefix(true, !self.family.is_anthropic())
                } else {
                    let mut current = instructions.prefix(false, !self.family.is_anthropic());
                    current.extend(instructions.notices.clone());
                    current
                }
            })
            .unwrap_or_default();
        let instruction_prefix_len = input.len();
        let native_epoch_candidate = epochs && self.family.is_anthropic();
        // A trimmed retry reads within the bounds rather than compacting
        // again; its notice names where the omitted history ends.
        self.window_floor = match &mut self.context {
            PlanContext::Durable(continuation) if continuation.trim_requested() => {
                continuation.floor_for_trim()?
            }
            _ => None,
        };
        let structural = match &self.context {
            PlanContext::Durable(continuation) => continuation
                .read_for_plan(|reader| reader.structural_admission().map_err(PlanRefusal::from))?,
            PlanContext::Transient(_) => None,
        };
        if let Some(structural) = structural {
            self.admission_diagnosis.record(
                super::super::continuation::admission::AdmissionDiagnosis {
                    trigger: grapt_types::CompactionTrigger::StructuralBound,
                    estimate_basis:
                        super::super::continuation::admission::EstimateBasis::StructuralWindow,
                    estimate: structural.observed,
                    budget: structural.limit,
                },
            );
            self.admission_diagnosis.reuse_previous_request();
            return Err(PlanRefusal::new(
                "durable history reached a structural request bound".to_owned(),
                Some(grapt_types::CompactionTrigger::StructuralBound),
            ));
        }
        let (history, context, protected_from) = match &self.context {
            PlanContext::Durable(continuation) => continuation.read_for_plan(|continuation| {
                let updates = instructions
                    .as_ref()
                    .filter(|_| epochs)
                    .map_or(&[][..], |instructions| instructions.updates.as_slice());
                // Legacy signatures were produced with the current-prefix format.
                // Inspect their adapter provenance before injecting epoch updates.
                let requested = if native_epoch_candidate {
                    continuation
                        .request_with_instructions(
                            &self.prompt,
                            step.value() > self.initial_step.value()
                                && !self.output_continuation.started(),
                            &[],
                        )
                        .and_then(|history| {
                            if history.0.iter().any(|item| {
                                matches!(item,
                                grapt_provider::CodexInputItem::ProviderReplay(replay)
                                    if replay.requires_legacy_anthropic_prefix())
                            }) {
                                epochs = false;
                                Ok(history)
                            } else if updates.is_empty() {
                                Ok(history)
                            } else {
                                continuation.request_with_instructions(
                                    &self.prompt,
                                    step.value() > self.initial_step.value()
                                        && !self.output_continuation.started(),
                                    updates,
                                )
                            }
                        })
                } else {
                    continuation.request_with_instructions(
                        &self.prompt,
                        step.value() > self.initial_step.value()
                            && !self.output_continuation.started(),
                        updates,
                    )
                };
                match requested {
                    Ok(history) => Ok(history),
                    Err(error) => {
                        // SC-REL-07a: compaction answers a capacity refusal,
                        // decided by its type; the diagnosis is what that
                        // producer observed, in its own unit, as the
                        // structural probes record theirs.
                        let compaction = error.compaction_trigger();
                        if let (Some(trigger), Some(capacity)) =
                            (compaction, error.capacity_bound())
                        {
                            self.admission_diagnosis
                                .record(super::super::continuation::admission::AdmissionDiagnosis {
                                trigger,
                                estimate_basis:
                                    super::super::continuation::admission::EstimateBasis::StructuralWindow,
                                estimate: capacity.observed,
                                budget: capacity.limit,
                            });
                        }
                        if compaction == Some(grapt_types::CompactionTrigger::StructuralBound) {
                            self.admission_diagnosis.reuse_previous_request();
                        }
                        Err(PlanRefusal::new(error.to_string(), compaction))
                    }
                }
            })?,
            PlanContext::Transient(history) => {
                let mut history = history.clone();
                let effort = self.targets.effort_policy();
                if effort.updates() {
                    if let Some(value) = effort.resolve(target.model_ref().variant()) {
                        history.push(grapt_provider::CodexInputItem::EffortUpdate(value));
                    }
                }
                history.push(grapt_provider::CodexInputItem::Message(
                    grapt_provider::InputMessage {
                        role: grapt_provider::MessageRole::User,
                        content: self.prompt.clone(),
                    },
                ));
                let protected_from = history.len() - 1;
                (history, None, protected_from)
            }
        };
        if native_epoch_candidate && !epochs {
            // Legacy replay uses the current instruction prefix.
            let mut current = instructions
                .as_ref()
                .map(|value| value.prefix(false, false))
                .unwrap_or_default();
            current.extend(
                instructions
                    .as_ref()
                    .into_iter()
                    .flat_map(|value| value.notices.clone()),
            );
            input.splice(..instruction_prefix_len, current);
        }
        // One-shot continuity input is consumed only after reader admission.
        if let Some(resume) = &mut self.continuity_resume {
            input.extend(
                resume
                    .take_input()
                    .map_err(|error| PlanRefusal::new(error.to_string(), None))?,
            );
        }
        let mut boundary = RequestBoundary {
            history_start: input.len(),
            protected_from: input.len() + protected_from,
        };
        input.extend(history);
        if epochs {
            input.extend(
                instructions
                    .as_ref()
                    .into_iter()
                    .flat_map(|instructions| instructions.notices.clone()),
            );
        }
        if self.output_continuation.cued() {
            output_continuation::append(&mut input);
        }
        self.prepare_context_hook(target, &mut input, &mut tools, &mut boundary)?;
        self.compactable_items = boundary.compactable(&input);
        // Judge the definitions that will actually be sent, again: a callback
        // can replace a description or a schema the request would have kept,
        // and the authority and the notice must describe the same set the
        // request sends.
        let native_reserved = usize::from(web_search.is_some());
        tools = judge_definitions(tools, &mut excluded);
        // Capacity is a selection rule, never a refusal: the request keeps its
        // built-ins and loses the definitions furthest from them. Each loss is
        // named like every other exclusion, so the recorded set is the set that
        // was really left out rather than a count that cannot identify it.
        if tools.len().saturating_add(native_reserved)
            > grapt_provider::tool_eligibility::MAX_REQUEST_TOOLS
        {
            let keep =
                grapt_provider::tool_eligibility::MAX_REQUEST_TOOLS.saturating_sub(native_reserved);
            for tool in tools.drain(keep..) {
                excluded.push((tool.name, "the request reached its tool capacity"));
            }
        }
        let mut builder = PhysicalAttemptAuthoritySnapshot::builder();
        for visibility in
            tool_visibilities(&tools).map_err(|detail| PlanRefusal::new(detail, None))?
        {
            builder
                .tool(visibility)
                .map_err(|error| PlanRefusal::new(error.to_string(), None))?;
        }
        if web_search.is_some() {
            builder
                .tool(
                    tool_visibility("web_search")
                        .map_err(|detail| PlanRefusal::new(detail, None))?,
                )
                .map_err(|error| PlanRefusal::new(error.to_string(), None))?;
        }
        // A durable session notice, not model input. It is recorded even when
        // the set is empty: an emptied set is the repair the reader was promised
        // when the exclusion was first announced.
        if let Some(instructions) = self.instructions.as_ref() {
            let excluded = excluded
                .iter()
                .map(|(name, reason)| grapt_session::notice::ExcludedTool {
                    name: name.clone(),
                    reason: (*reason).to_owned(),
                })
                .collect();
            let authorities = grapt_types::DeterministicAuthorities::new(
                &*instructions.clock,
                &*instructions.ids,
            );
            match instructions.inbox.lock() {
                Ok(mut inbox) => {
                    let _recorded = grapt_session::notice::record_tool_excluded(
                        inbox.store_mut(),
                        &authorities,
                        &instructions.session,
                        excluded,
                    );
                }
                Err(poisoned) => {
                    let mut inbox = poisoned.into_inner();
                    let _recorded = grapt_session::notice::record_tool_excluded(
                        inbox.store_mut(),
                        &authorities,
                        &instructions.session,
                        excluded,
                    );
                }
            }
        }
        if let Some(hooks) = &self.lifecycle {
            hooks
                .apply(&mut input)
                .map_err(|detail| PlanRefusal::new(detail, None))?;
        }
        // Fresh mail joins the immutable conversation tail after tool results
        // and context hooks. Its sealed batch is captured by the existing attempt
        // reservation transaction; a failed plan leaves it pending in the mailbox.
        let prepared_mail = self
            .native_mail
            .as_mut()
            .map(|mail| {
                let warning = mail.decode_warning()?;
                Ok::<_, super::super::stateengine_resume::ContinuityUnavailable>((
                    mail.take_batch()?,
                    warning,
                ))
            })
            .transpose();
        // A mail outage leaves the batch pending and tells the model why; an
        // authority or identity failure still refuses the plan.
        let (mail, warning) = match prepared_mail {
            Ok(prepared) => prepared.unwrap_or((None, None)),
            Err(failure) if failure.degrades_mail() => (
                None,
                Some(format!(
                    "StateEngine mail is unavailable this turn: {failure}"
                )),
            ),
            Err(error) => {
                return Err(PlanRefusal::new(
                    format!("Native inbox preparation refused: {error}"),
                    None,
                ))
            }
        };
        if let Some(warning) = warning {
            input.push(grapt_provider::CodexInputItem::Message(
                grapt_provider::InputMessage {
                    role: grapt_provider::MessageRole::User,
                    content: warning,
                },
            ));
        }
        if let Some(batch) = &mail {
            input.push(grapt_provider::CodexInputItem::Message(
                grapt_provider::InputMessage {
                    role: grapt_provider::MessageRole::User,
                    content: batch.content().to_owned(),
                },
            ));
        }
        let operation_mail = self.operation_mail(step, &mut input)?;
        image_input::normalise(
            &mut input,
            self.targets.image_input_supported(),
            self.targets.original_image_detail_supported(),
        )
        .map_err(|detail| PlanRefusal::new(detail, None))?;
        #[cfg(test)]
        super::super::continuation::tests::response_groups::assert_coding_final_turn(&input);
        let mut request = match self.family {
            ProtocolFamily::BedrockConverse => {
                ProviderRequest::BedrockConverse(grapt_provider::BedrockConverseRequest {
                    model: target.model_ref().clone(),
                    messages: input,
                    max_tokens: grapt_provider::MaxOutputTokens::new(
                        self.targets
                            .anthropic_max_tokens()
                            .map_err(|detail| PlanRefusal::new(detail, None))?
                            .value()
                            .min(super::super::continuation::admission::OUTPUT_RESERVE),
                    )
                    .ok_or_else(|| {
                        PlanRefusal::new("Bedrock output bound is invalid".into(), None)
                    })?,
                    tools,
                    tool_choice: Some(ToolChoice::Auto),
                })
            }
            ProtocolFamily::OpenAiResponses => {
                ProviderRequest::OpenAiResponses(grapt_provider::OpenAiResponsesRequest {
                    model: target.model_ref().clone(),
                    input,
                    prompt_cache_key: Some(self.prompt_cache_key.clone()),
                    max_output_tokens: grapt_provider::MaxOutputTokens::new(
                        super::super::continuation::admission::OUTPUT_RESERVE,
                    ),
                    tools,
                    web_search: web_search.clone(),
                    tool_choice: Some(ToolChoice::Auto),
                })
            }
            family @ (ProtocolFamily::AnthropicMessages | ProtocolFamily::VertexAnthropic) => {
                let refusal = |detail| PlanRefusal::new(detail, None);
                let max_tokens = self.targets.anthropic_max_tokens().map_err(refusal)?;
                let thinking = thinking::bounded(self.targets.anthropic_thinking(), max_tokens)
                    .map_err(refusal)?;
                let request = grapt_provider::AnthropicMessagesRequest {
                    web_search: None,
                    uncached_system_suffix: instructions
                        .as_ref()
                        .and_then(|value| value.session_context())
                        .map(str::to_owned),
                    effort: self.targets.effort_policy(),
                    model: target.model_ref().clone(),
                    messages: input,
                    max_tokens,
                    instruction_mode: if epochs {
                        grapt_provider::AnthropicInstructionMode::Epochs
                    } else {
                        grapt_provider::AnthropicInstructionMode::CurrentPrefix
                    },
                    thinking,
                    prompt_cache: grapt_provider::AnthropicPromptCache::StablePrefix,
                    tools,
                    tool_choice: Some(ToolChoice::Auto),
                };
                if family == ProtocolFamily::VertexAnthropic {
                    ProviderRequest::VertexAnthropic(request)
                } else {
                    ProviderRequest::AnthropicMessages(request)
                }
            }
            ProtocolFamily::ChatGptCodex => ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
                model: target.model_ref().clone(),
                input,
                prompt_cache_key: Some(self.prompt_cache_key.clone()),
                tools,
                web_search: web_search.clone(),
                tool_choice: Some(ToolChoice::Auto),
                reasoning: self.targets.codex_reasoning(),
                turn: self.codex_turn.clone(),
            }),
            ProtocolFamily::GoogleGemini => {
                ProviderRequest::GoogleGemini(grapt_provider::GoogleGeminiRequest {
                    model: target.model_ref().clone(),
                    messages: input,
                    max_tokens: grapt_provider::MaxOutputTokens::new(
                        super::super::continuation::admission::OUTPUT_RESERVE,
                    ),
                    tools,
                    tool_choice: Some(ToolChoice::Auto),
                })
            }
            ProtocolFamily::CohereChat => {
                ProviderRequest::CohereChat(grapt_provider::CohereChatRequest {
                    model: target.model_ref().clone(),
                    messages: input,
                    max_tokens: Some(
                        self.targets
                            .output_bound(super::super::continuation::admission::OUTPUT_RESERVE)
                            .map_err(|detail| PlanRefusal::new(detail, None))?,
                    ),
                    tools,
                    tool_choice: Some(ToolChoice::Auto),
                })
            }
            ProtocolFamily::VertexGemini => {
                ProviderRequest::VertexGemini(grapt_provider::GoogleGeminiRequest {
                    model: target.model_ref().clone(),
                    messages: input,
                    max_tokens: grapt_provider::MaxOutputTokens::new(
                        super::super::continuation::admission::OUTPUT_RESERVE,
                    ),
                    tools,
                    tool_choice: Some(ToolChoice::Auto),
                })
            }
            family @ (ProtocolFamily::OpenAiCompatible | ProtocolFamily::WatsonxChat) => {
                let request = grapt_provider::OpenAiCompatibleRequest {
                    reasoning_replay: self.targets.compatible_reasoning_replay(),
                    prompt_cache: compatible_prompt_cache(target.model_ref()),
                    model: target.model_ref().clone(),
                    messages: input,
                    max_tokens: grapt_provider::MaxOutputTokens::new(
                        super::super::continuation::admission::OUTPUT_RESERVE,
                    ),
                    tools,
                    tool_choice: Some(ToolChoice::Auto),
                    routing_session: (family == ProtocolFamily::OpenAiCompatible)
                        .then(|| self.prompt_cache_key.clone()),
                };
                if family == ProtocolFamily::WatsonxChat {
                    ProviderRequest::WatsonxChat(request)
                } else {
                    ProviderRequest::OpenAiCompatible(request)
                }
            }
        };
        let context = self.finalize_context(&mut request, &mut boundary, context)?;
        Ok(PreparedRequest {
            operation_mail,
            mail,
            authority: builder,
            request,
            context,
        })
    }
}
