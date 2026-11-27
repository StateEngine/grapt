#[path = "tests/mail_outage.rs"]
mod mail_outage;
#[path = "tests/patch_selection.rs"]
mod patch_selection;

use std::sync::atomic::{AtomicUsize, Ordering};

use grapt_runtime::AttemptPlan;
use grapt_types::{
    AccountId, BaseModelId, CredentialVersion, ModelCatalog, ModelCatalogRecord,
    ModelCatalogVersion, ModelFamilyId, ModelId, ModelRegistry, ModelRouteId, PolicyVersion,
    ProviderId, RoutingRequest, SessionId,
};

use super::*;

pub(in crate::dogfood) fn projection_target() -> Result<ResolvedModelTarget, String> {
    targets(ProtocolFamily::AnthropicMessages)?.issue()
}

pub(in crate::dogfood) fn projection_targets(
    family: ProtocolFamily,
) -> Result<Arc<dyn ProductTargetAuthority>, String> {
    Ok(targets(family)?)
}

/// The projection targets with the model window `tokens`.
pub(in crate::dogfood) fn projection_targets_with_window(
    family: ProtocolFamily,
    tokens: u64,
) -> Result<Arc<dyn ProductTargetAuthority>, String> {
    let concrete = targets(family)?;
    concrete.context_tokens.store(tokens, Ordering::SeqCst);
    Ok(concrete)
}

struct CountingTargets {
    family: ProtocolFamily,
    route: ModelRouteId,
    registry: ModelRegistry,
    issued: AtomicUsize,
    context_tokens: std::sync::atomic::AtomicU64,
}

impl ProductTargetAuthority for CountingTargets {
    fn anthropic_max_tokens(&self) -> Result<grapt_provider::MaxOutputTokens, String> {
        grapt_provider::MaxOutputTokens::new(32000).ok_or_else(|| "fixture output limit".into())
    }
    fn context_tokens(&self) -> Option<u64> {
        Some(self.context_tokens.load(Ordering::SeqCst))
    }

    fn protocol_family(&self) -> ProtocolFamily {
        self.family
    }

    fn issue(&self) -> Result<ResolvedModelTarget, String> {
        self.issued.fetch_add(1, Ordering::SeqCst);
        let provider =
            ProviderId::new("openai").ok_or_else(|| "provider identity was refused".to_owned())?;
        let model =
            ModelId::new("gpt-5-codex").ok_or_else(|| "model identity was refused".to_owned())?;
        let account = AccountId::new("acct_product")
            .ok_or_else(|| "account identity was refused".to_owned())?;
        let credential_version = CredentialVersion::new("v1")
            .ok_or_else(|| "credential version was refused".to_owned())?;
        let policy = PolicyVersion::new("policy_product")
            .ok_or_else(|| "policy identity was refused".to_owned())?;
        self.registry
            .issue(
                &provider,
                &model,
                None,
                RoutingRequest {
                    route: self.route.clone(),
                    account,
                    credential_version,
                    policy,
                },
            )
            .map_err(|error| format!("target issuance refused: {error:?}"))
    }
}

fn targets(family: ProtocolFamily) -> Result<Arc<CountingTargets>, String> {
    let provider =
        ProviderId::new("openai").ok_or_else(|| "provider identity was refused".to_owned())?;
    let model =
        ModelId::new("gpt-5-codex").ok_or_else(|| "model identity was refused".to_owned())?;
    let base = BaseModelId::new("gpt-5-codex")
        .ok_or_else(|| "base-model identity was refused".to_owned())?;
    let model_family = ModelFamilyId::new("openai")
        .ok_or_else(|| "model-family identity was refused".to_owned())?;
    let version = ModelCatalogVersion::new("real-model-headless-test")
        .ok_or_else(|| "catalog version was refused".to_owned())?;
    let catalog = ModelCatalog::load(
        version,
        vec![ModelCatalogRecord::new(
            provider,
            model,
            base,
            model_family,
            None,
        )],
    )
    .ok_or_else(|| "catalog was refused".to_owned())?;
    let route = ModelRouteId::new("route_product")
        .ok_or_else(|| "route identity was refused".to_owned())?;
    Ok(Arc::new(CountingTargets {
        family,
        route,
        registry: ModelRegistry::install(catalog),
        issued: AtomicUsize::new(0),
        context_tokens: std::sync::atomic::AtomicU64::new(128_000),
    }))
}

fn cache_key(label: &str) -> Result<PromptCacheKey, String> {
    let session = format!("ses_cache_{label}")
        .parse::<SessionId>()
        .map_err(|error| error.to_string())?;
    PromptCacheKey::for_session(&session).map_err(|error| error.to_string())
}

#[test]
fn resume_tool_remains_available_after_harness_context_acquisition() -> Result<(), String> {
    let concrete = targets(ProtocolFamily::ChatGptCodex)?;
    let step = StepNumber::new(1).ok_or("Step")?;
    let (continuation, _scratch) = DurableContinuation::empty_for_test("resumetools")?;
    let name = "stateengine_stateengine_resume".to_owned();
    let mut published = crate::dogfood::mcp::PublishedTools::default();
    published.definitions.push(grapt_provider::ToolDefinition {
        input_format: grapt_provider::ToolInputFormat::Json,
        name: name.clone(),
        description: None,
        input_schema: "{}".into(),
    });
    published.routes.insert(
        name.clone(),
        crate::dogfood::mcp::McpRoute {
            description: None,
            output_schema: None,
            server: "stateengine".into(),
            tool: grapt_mcp::McpToolName::new("stateengine_resume").ok_or("tool")?,
            launch: crate::dogfood::mcp::LaunchIdentity {
                declaration: std::sync::Arc::new(crate::dogfood::mcp::Declaration::Disabled),
                program: "fixture".into(),
                cwd: "/fixture".into(),
                arguments: Vec::new(),
                incarnation: 1,
            },
        },
    );
    let published = Arc::new(published);
    let mut plan = ProductPlan::new(
        ProtocolFamily::ChatGptCodex,
        "hello".into(),
        cache_key("resumetools")?,
        None,
        concrete,
        published.clone(),
        continuation,
        step,
    );
    let planned = plan
        .plan(step, 1, HistoryGeneration::new(1))
        .map_err(|error| error.detail)?;
    let ProviderRequest::ChatGptCodex(request) = planned.request else {
        return Err("family".into());
    };
    assert!(request
        .tools
        .iter()
        .any(|definition| definition.name == name));
    assert!(
        published.routes.contains_key(&name),
        "harness still needs the exact discovered route"
    );
    Ok(())
}

#[test]
fn first_attempt_consumes_preissued_target_and_retry_issues_fresh() -> Result<(), String> {
    let concrete = targets(ProtocolFamily::ChatGptCodex)?;
    let first_target = concrete.issue()?;
    let authority: Arc<dyn ProductTargetAuthority> = concrete.clone();
    let step = StepNumber::new(1).ok_or_else(|| "Step was refused".to_owned())?;
    let (continuation, _scratch) = DurableContinuation::empty_for_test("target-reissue")?;
    let mut plan = ProductPlan {
        ephemeral: false,
        retry_feedback: None,
        family: ProtocolFamily::ChatGptCodex,
        text_only: false,
        web_search: None,
        prompt: "hello".to_owned(),
        prompt_cache_key: cache_key("target_reissue")?,
        first_target: Some(first_target),
        targets: authority,
        published: Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        context: PlanContext::Durable(continuation),
        initial_step: step,
        instructions: None,
        continuity_resume: None,
        preparation_progress: tokio::sync::watch::channel(
            grapt_runtime::step::ContextPreparationStage::Context,
        )
        .0,
        native_mail: None,
        accounting: super::super::continuation::admission::Accounting::default(),
        codex_turn: Default::default(),
        admission_diagnosis: super::super::continuation::admission::DiagnosisSlot::default(),
        trim_feedback: None,
        trimmed_request: false,
        context_hook: None,
        lifecycle: None,
        compaction: Default::default(),
        policy: None,
        shell_description: None,
        output_continuation: Default::default(),
    };
    let first = plan
        .plan(step, 1, HistoryGeneration::new(1))
        .map_err(|error| error.detail)?;
    assert_eq!(
        concrete.issued.load(Ordering::SeqCst),
        1,
        "the preissued first target was not consumed exactly once"
    );
    let retry = plan
        .plan(step, 2, HistoryGeneration::new(1))
        .map_err(|error| error.detail)?;
    assert_eq!(
        concrete.issued.load(Ordering::SeqCst),
        2,
        "the retry did not issue one fresh linear target"
    );
    let (ProviderRequest::ChatGptCodex(first), ProviderRequest::ChatGptCodex(retry)) =
        (first.request, retry.request)
    else {
        return Err("the Codex plan changed protocol families on retry".to_owned());
    };
    assert!(
        first.prompt_cache_key.is_some(),
        "the initial attempt lost its Session-stable provider cache coordinate"
    );
    assert_eq!(first.prompt_cache_key, retry.prompt_cache_key);
    Ok(())
}

#[test]
fn compatible_selection_builds_chat_messages_and_reissues_on_retry() -> Result<(), String> {
    let concrete = targets(ProtocolFamily::OpenAiCompatible)?;
    let first_target = concrete.issue()?;
    let step = StepNumber::new(1).ok_or("Step")?;
    let (continuation, _scratch) = DurableContinuation::empty_for_test("compatible-target")?;
    let mut plan = ProductPlan::new(
        ProtocolFamily::OpenAiCompatible,
        "hello".into(),
        cache_key("compatible")?,
        Some(first_target),
        concrete.clone(),
        Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        continuation,
        step,
    );
    for ordinal in 1..=2 {
        let planned = plan
            .plan(step, ordinal, HistoryGeneration::new(1))
            .map_err(|error| error.detail)?;
        let ProviderRequest::OpenAiCompatible(request) = planned.request else {
            return Err("wrong family".into());
        };
        assert!(
            matches!(request.messages.last(), Some(grapt_provider::CodexInputItem::Message(message)) if message.content == "hello")
        );
        assert!(!request.tools.is_empty());
    }
    assert_eq!(concrete.issued.load(Ordering::SeqCst), 2);
    Ok(())
}

#[test]
fn swr_c5_h1_description_bytes_drive_exact_product_admission_boundary() -> Result<(), String> {
    let concrete = targets(ProtocolFamily::ChatGptCodex)?;
    let first = concrete.issue()?;
    let step = StepNumber::new(1).ok_or("step")?;
    let (continuation, _scratch) = DurableContinuation::empty_for_test("swr-diagnosis")?;
    let diagnosis = super::super::continuation::admission::DiagnosisSlot::default();
    let mut published = super::super::mcp::PublishedTools::default();
    published.definitions.push(grapt_provider::ToolDefinition {
        input_format: grapt_provider::ToolInputFormat::Json,
        name: "described_tool".into(),
        description: Some("d".repeat(512)),
        input_schema: "{}".into(),
    });
    let mut plan = ProductPlan::new(
        ProtocolFamily::ChatGptCodex,
        "hello".into(),
        cache_key("swr_diagnosis")?,
        Some(first),
        concrete.clone(),
        Arc::new(published),
        continuation,
        step,
    )
    .with_admission_diagnosis(diagnosis.clone());
    let planned = plan
        .plan(step, 1, HistoryGeneration::new(1))
        .map_err(|error| error.detail)?;
    assert_eq!(plan.context_admission(&planned), Ok(None));
    let recorded = diagnosis
        .matching(grapt_types::CompactionTrigger::ContextOverflow)
        .ok_or("actual admission estimate was not retained")?;
    assert_eq!(
        recorded.estimate_basis,
        super::super::continuation::admission::EstimateBasis::Estimated
    );
    assert!(recorded.estimate > 0);
    assert_eq!(recorded.budget, 128_000);
    assert_eq!(
        recorded.trigger,
        grapt_types::CompactionTrigger::ContextOverflow
    );
    let mut omitted = planned.request.clone();
    let ProviderRequest::ChatGptCodex(omitted_request) = &mut omitted else {
        return Err("description admission changed provider family".into());
    };
    omitted_request
        .tools
        .iter_mut()
        .find(|tool| tool.name == "described_tool")
        .ok_or("described tool missing")?
        .description = None;
    let omitted = super::super::continuation::admission::estimate(&omitted)
        .map_err(|error| format!("omitted estimate failed: {error:?}"))?;
    assert_eq!(
        recorded.estimate.checked_sub(omitted),
        Some(512),
        "description bytes escaped conservative admission"
    );
    let threshold = recorded
        .estimate
        .checked_add(u64::from(
            super::super::continuation::admission::OUTPUT_RESERVE,
        ))
        .and_then(|value| value.checked_add(super::super::continuation::admission::HEADROOM))
        .ok_or("admission threshold overflow")?;
    concrete
        .context_tokens
        .store(threshold + 1, Ordering::SeqCst);
    assert_eq!(plan.context_admission(&planned), Ok(None));
    concrete.context_tokens.store(threshold, Ordering::SeqCst);
    assert_eq!(
        plan.context_admission(&planned),
        Ok(Some(grapt_types::CompactionTrigger::PolicyThreshold))
    );
    Ok(())
}

#[test]
fn pc_all_families_preserve_exact_target_input_tools_and_retry_issuance() -> Result<(), String> {
    for family in [
        ProtocolFamily::OpenAiResponses,
        ProtocolFamily::ChatGptCodex,
        ProtocolFamily::AnthropicMessages,
        ProtocolFamily::OpenAiCompatible,
    ] {
        let concrete = targets(family)?;
        let first = concrete.issue()?;
        let step = StepNumber::new(1).ok_or("Step")?;
        let (continuation, _scratch) = DurableContinuation::empty_for_test("pc-families")?;
        let mut plan = ProductPlan::new(
            family,
            "hello".into(),
            cache_key("pc_family")?,
            Some(first),
            concrete.clone(),
            Arc::new(super::super::mcp::PublishedTools::default()),
            continuation,
            step,
        );
        for ordinal in 1..=2 {
            let planned = plan
                .plan(step, ordinal, HistoryGeneration::new(1))
                .map_err(|error| error.detail)?;
            assert_eq!(planned.request.family(), family);
            assert_eq!(
                planned.request.model(),
                planned.authority.model().model_ref()
            );
            assert_eq!(planned.request.model().model().as_str(), "gpt-5-codex");
            assert_eq!(planned.request.model().provider().as_str(), "openai");
            assert!(!planned.request.tools().is_empty());
            let input = match &planned.request {
                ProviderRequest::OpenAiResponses(request) => &request.input,
                ProviderRequest::ChatGptCodex(request) => &request.input,
                ProviderRequest::AnthropicMessages(request)
                | ProviderRequest::VertexAnthropic(request) => &request.messages,
                ProviderRequest::OpenAiCompatible(request)
                | ProviderRequest::WatsonxChat(request) => &request.messages,
                ProviderRequest::BedrockConverse(request) => &request.messages,
                ProviderRequest::GoogleGemini(request) | ProviderRequest::VertexGemini(request) => {
                    &request.messages
                }
                ProviderRequest::CohereChat(request) => &request.messages,
            };
            assert!(
                matches!(input.last(), Some(grapt_provider::CodexInputItem::Message(message)) if message.content == "hello")
            );
            assert_eq!(concrete.issued.load(Ordering::SeqCst), ordinal as usize);
        }
    }
    Ok(())
}

#[test]
fn pc_anthropic_selection_builds_a_bounded_tool_request() -> Result<(), String> {
    let concrete = targets(ProtocolFamily::AnthropicMessages)?;
    let first_target = concrete.issue()?;
    let authority: Arc<dyn ProductTargetAuthority> = concrete.clone();
    let step = StepNumber::new(1).ok_or_else(|| "Step was refused".to_owned())?;
    let (continuation, _scratch) = DurableContinuation::empty_for_test("unsupported-family")?;
    let mut plan = ProductPlan {
        ephemeral: false,
        retry_feedback: None,
        family: ProtocolFamily::AnthropicMessages,
        text_only: false,
        prompt: "hello".to_owned(),
        prompt_cache_key: cache_key("unsupported_family")?,
        first_target: Some(first_target),
        targets: authority,
        published: Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        context: PlanContext::Durable(continuation),
        initial_step: step,
        instructions: None,
        continuity_resume: None,
        preparation_progress: tokio::sync::watch::channel(
            grapt_runtime::step::ContextPreparationStage::Context,
        )
        .0,
        native_mail: None,
        web_search: None,
        accounting: super::super::continuation::admission::Accounting::default(),
        codex_turn: Default::default(),
        admission_diagnosis: super::super::continuation::admission::DiagnosisSlot::default(),
        trim_feedback: None,
        trimmed_request: false,
        context_hook: None,
        lifecycle: None,
        compaction: Default::default(),
        policy: None,
        shell_description: None,
        output_continuation: Default::default(),
    };
    let result = plan
        .plan(step, 1, HistoryGeneration::new(1))
        .map_err(|error| error.detail)?;
    let ProviderRequest::AnthropicMessages(request) = result.request else {
        return Err("selected Anthropic family changed".into());
    };
    assert_eq!(
        request.max_tokens.value(),
        concrete.anthropic_max_tokens()?.value()
    );
    assert_eq!(
        request.prompt_cache,
        grapt_provider::AnthropicPromptCache::StablePrefix,
        "ordinary coding requests ask Anthropic to cache their stable prefix"
    );
    assert!(
        matches!(request.messages.last(), Some(grapt_provider::CodexInputItem::Message(message)) if message.content == "hello")
    );
    assert!(!request.tools.is_empty());
    assert_eq!(concrete.issued.load(Ordering::SeqCst), 1);
    Ok(())
}

fn catalog_model(vendor: &str, name: &str) -> grapt_types::ModelRef {
    use grapt_types::{
        BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
        ProviderId,
    };
    let (Some(provider), Some(model), Some(base), Some(family), Some(version)) = (
        ProviderId::new(vendor),
        ModelId::new(name),
        BaseModelId::new(name),
        ModelFamilyId::new(vendor),
        ModelCatalogVersion::new("test-catalog"),
    ) else {
        panic!("test model coordinates must be non-empty")
    };
    let record = ModelCatalogRecord::new(provider.clone(), model.clone(), base, family, None);
    let Some(catalog) = ModelCatalog::load(version, vec![record]) else {
        panic!("test catalog")
    };
    let Some(entry) = catalog.entry(&provider, &model, None) else {
        panic!("catalog entry")
    };
    grapt_types::ModelRef::from_catalog(entry)
}

#[test]
fn only_anthropic_models_get_compatible_cache_breakpoints() {
    use grapt_provider::CompatiblePromptCache::{AnthropicBreakpoints, Off};
    for (vendor, name, expected) in [
        (
            "openrouter",
            "anthropic/claude-sonnet-5.5",
            AnthropicBreakpoints,
        ),
        ("openrouter", "some-host/claude-like", AnthropicBreakpoints),
        ("anthropic", "sonnet", AnthropicBreakpoints),
        ("openrouter", "openai/gpt-6-luna", Off),
        ("xai", "grok-4", Off),
    ] {
        assert_eq!(
            super::compatible_prompt_cache(&catalog_model(vendor, name)),
            expected,
            "{vendor}/{name}"
        );
    }
}

/// A target and the large tool descriptions that separate the judged size
/// from the packing margin.
fn threshold_target() -> Result<
    (
        Arc<CountingTargets>,
        Arc<super::super::mcp::PublishedTools>,
        StepNumber,
    ),
    String,
> {
    let concrete = targets(ProtocolFamily::ChatGptCodex)?;
    let step = StepNumber::new(1).ok_or("step")?;
    let mut published = super::super::mcp::PublishedTools::default();
    for index in 0..10 {
        published.definitions.push(grapt_provider::ToolDefinition {
            input_format: grapt_provider::ToolInputFormat::Json,
            name: format!("described_tool_{index}"),
            description: Some("d".repeat(4096)),
            input_schema: "{}".into(),
        });
    }
    Ok((concrete, Arc::new(published), step))
}

/// A plan for a fresh Session whose earlier execution, on this exact target
/// issuance, confirmed 0.3 tokens per estimated unit.
fn calibrated_threshold_plan(
    concrete: &Arc<CountingTargets>,
    published: &Arc<super::super::mcp::PublishedTools>,
    step: StepNumber,
    label: &str,
    buffer: Option<u64>,
) -> Result<
    (
        ProductPlan,
        super::super::continuation::admission::Calibration,
        super::super::continuation::TestContinuationScratch,
    ),
    String,
> {
    use super::super::compaction_config::CompactionSettings;
    use super::super::continuation::admission::{estimate, Accounting, SessionCalibrations};
    let target = concrete.issue()?;
    let owner: grapt_types::SessionId = format!("ses_{}", label.replace('-', "_"))
        .parse()
        .map_err(|_| "session")?;
    let sessions = SessionCalibrations::default();
    let (continuation, scratch) = DurableContinuation::empty_for_test(label)?;
    let earlier = estimate_request(published)?;
    let units = estimate(&earlier).map_err(|error| format!("{error:?}"))?;
    let mut primer = Accounting::for_session(sessions.clone(), owner.clone());
    primer
        .estimate(&earlier, &target, None)
        .map_err(|error| format!("{error:?}"))?;
    primer.admitted(&grapt_types::PhysicalAttemptId::new(label).ok_or("attempt")?);
    primer
        .estimate(&earlier, &target, Some(units * 3 / 10))
        .map_err(|error| format!("{error:?}"))?;
    let calibration = primer.calibration_for(&target).ok_or("no calibration")?;
    let plan = ProductPlan::new(
        ProtocolFamily::ChatGptCodex,
        "hello".into(),
        cache_key("cq_threshold")?,
        Some(target),
        concrete.clone(),
        published.clone(),
        continuation,
        step,
    )
    .with_session_calibration(sessions, owner)
    .with_compaction(CompactionSettings {
        buffer,
        ..CompactionSettings::default()
    });
    Ok((plan, calibration, scratch))
}

/// The planned request with its judged size, packing charge and reserve.
fn judged_threshold_request(
    plan: &mut ProductPlan,
    calibration: &super::super::continuation::admission::Calibration,
    step: StepNumber,
) -> Result<(grapt_runtime::PlannedAttempt, u64, u64, u64), String> {
    use super::super::continuation::admission::{estimate, output_reserve};
    let planned = plan
        .plan(step, 1, HistoryGeneration::new(1))
        .map_err(|error| error.detail)?;
    let full = estimate(&planned.request).map_err(|error| format!("{error:?}"))?;
    let tokens = calibration
        .tokens(full)
        .map_err(|error| format!("{error:?}"))?;
    let packed = calibration
        .charge(full)
        .map_err(|error| format!("{error:?}"))?;
    let reserve = u64::from(output_reserve(&planned.request));
    Ok((planned, tokens, packed, reserve))
}

/// One honest size guards the threshold, judged at this target's observed
/// ratio: a request under it does not compact, and one over it does, however
/// far the configured buffer moves the packing margin. A margin above the
/// ratio would compact a session at about half its real usage and refuse the
/// summary that would have fit.
#[test]
fn cq_the_threshold_is_judged_at_the_observed_ratio() -> Result<(), String> {
    use super::super::continuation::admission::HEADROOM;
    let (concrete, published, step) = threshold_target()?;
    // The margin still bounds packing, but never the judged size.
    let (mut plan, calibration, _scratch) =
        calibrated_threshold_plan(&concrete, &published, step, "cq-threshold-ratio", None)?;
    let (_, tokens, packed, _) = judged_threshold_request(&mut plan, &calibration, step)?;
    assert!(packed > tokens + HEADROOM, "{tokens} {packed}");
    // A buffer above SE's own headroom, as C04's is. A request under the
    // threshold does not compact, even though it would exceed that threshold
    // at the packing margin.
    let buffer = packed - tokens + 2 * HEADROOM;
    let (mut plan, calibration, _scratch) = calibrated_threshold_plan(
        &concrete,
        &published,
        step,
        "cq-threshold-buffer",
        Some(buffer),
    )?;
    let (planned, tokens, packed, reserve) =
        judged_threshold_request(&mut plan, &calibration, step)?;
    concrete
        .context_tokens
        .store(tokens + reserve + buffer + 1, Ordering::SeqCst);
    assert!(packed + reserve + HEADROOM < tokens + reserve + buffer + 1);
    assert_eq!(plan.context_admission(&planned), Ok(None));
    // Over the threshold at the judged size: compaction.
    concrete
        .context_tokens
        .store(tokens + reserve + buffer, Ordering::SeqCst);
    assert_eq!(
        plan.context_admission(&planned),
        Ok(Some(grapt_types::CompactionTrigger::PolicyThreshold))
    );
    Ok(())
}

/// Without a configured buffer the window fit is the threshold, so the same
/// judged size decides both. A request the judged count places inside the
/// window does not compact just because the packing margin would put it
/// outside; that difference is exactly the defect this seam closes.
#[test]
fn cq_the_window_is_judged_at_the_observed_ratio() -> Result<(), String> {
    use super::super::continuation::admission::HEADROOM;
    let (concrete, published, step) = threshold_target()?;
    let (mut plan, calibration, _scratch) =
        calibrated_threshold_plan(&concrete, &published, step, "cq-threshold-window", None)?;
    let (planned, tokens, packed, reserve) =
        judged_threshold_request(&mut plan, &calibration, step)?;
    concrete
        .context_tokens
        .store(packed + reserve + HEADROOM, Ordering::SeqCst);
    assert_eq!(plan.context_admission(&planned), Ok(None));
    // A request the judged count itself places outside the window compacts.
    concrete
        .context_tokens
        .store(tokens + reserve + HEADROOM, Ordering::SeqCst);
    assert_eq!(
        plan.context_admission(&planned),
        Ok(Some(grapt_types::CompactionTrigger::PolicyThreshold))
    );
    Ok(())
}

/// The coding request a plan with these tools builds for "hello".
fn estimate_request(
    published: &Arc<super::super::mcp::PublishedTools>,
) -> Result<ProviderRequest, String> {
    let concrete = targets(ProtocolFamily::ChatGptCodex)?;
    let step = StepNumber::new(1).ok_or("step")?;
    let (continuation, _scratch) = DurableContinuation::empty_for_test("cq-threshold-request")?;
    let mut plan = ProductPlan::new(
        ProtocolFamily::ChatGptCodex,
        "hello".into(),
        cache_key("cq_threshold")?,
        Some(concrete.issue()?),
        concrete,
        published.clone(),
        continuation,
        step,
    );
    Ok(plan
        .plan(step, 1, HistoryGeneration::new(1))
        .map_err(|error| error.detail)?
        .request)
}

#[test]
fn configured_native_search_is_snapshotted_and_enters_the_existing_attempt_authority(
) -> Result<(), String> {
    for family in [
        ProtocolFamily::ChatGptCodex,
        ProtocolFamily::OpenAiResponses,
        ProtocolFamily::AnthropicMessages,
    ] {
        let concrete = targets(family)?;
        let step = StepNumber::new(1).ok_or("Step")?;
        let (continuation, _scratch) = DurableContinuation::empty_for_test("native_search_plan")?;
        let mut plan = ProductPlan::new(
            family,
            "hello".into(),
            cache_key("native_search")?,
            None,
            concrete,
            Arc::new(crate::dogfood::mcp::PublishedTools::default()),
            continuation,
            step,
        )
        .with_web_search(Some(grapt_provider::WebSearchSpec::new(
            grapt_provider::WebSearchAccess::Cached,
        )));
        let planned = plan
            .plan(step, 1, HistoryGeneration::new(1))
            .map_err(|e| e.detail)?;
        let supported = matches!(
            family,
            ProtocolFamily::ChatGptCodex | ProtocolFamily::OpenAiResponses
        );
        assert_eq!(planned.request.web_search().is_some(), supported);
        let name = ToolName::new("web_search".to_owned()).ok_or("tool name")?;
        assert_eq!(planned.authority.tool_is_visible(&name), supported);
        assert!(!planned
            .request
            .tools()
            .iter()
            .any(|tool| tool.name == "web_search"));
    }
    Ok(())
}

#[test]
fn anthropic_admission_reserves_the_selected_total_output_ceiling() -> Result<(), String> {
    let concrete = targets(ProtocolFamily::AnthropicMessages)?;
    let step = StepNumber::new(1).ok_or("step")?;
    let (continuation, _scratch) = DurableContinuation::empty_for_test("rel06-output-reserve")?;
    let mut plan = ProductPlan::new(
        ProtocolFamily::AnthropicMessages,
        "hello".into(),
        cache_key("rel06_output")?,
        None,
        concrete.clone(),
        Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        continuation,
        step,
    );
    let planned = plan
        .plan(step, 1, HistoryGeneration::new(1))
        .map_err(|error| error.detail)?;
    assert_eq!(
        super::super::continuation::admission::output_reserve(&planned.request),
        32000
    );
    assert_eq!(plan.context_admission(&planned), Ok(None));
    // The input alone fits, but this window cannot hold the catalog's output.
    concrete.context_tokens.store(32000, Ordering::SeqCst);
    assert_ne!(plan.context_admission(&planned), Ok(None));
    Ok(())
}

/// After a no-progress compaction the recovery request is sized by encoding
/// it, so a switched target's thinking must be gone before it is sized, and
/// the trim's boundary must still name the current input after that drop.
#[test]
fn recovery_trim_sizes_a_switched_target_without_foreign_thinking() -> Result<(), String> {
    use grapt_provider::{CodexInputItem, InputMessage, MessageRole};
    let concrete = targets(ProtocolFamily::OpenAiCompatible)?;
    let step = StepNumber::new(1).ok_or("Step")?;
    let (continuation, _scratch) = DurableContinuation::empty_for_test("switched-trim")?;
    let continuation = continuation.after_no_progress_for_test(&concrete.issue()?)?;
    let mut plan = ProductPlan::new(
        ProtocolFamily::OpenAiCompatible,
        "hello".into(),
        cache_key("switched_trim")?,
        None,
        concrete.clone(),
        Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        continuation,
        step,
    );
    let mut request = plan
        .plan(step, 1, HistoryGeneration::new(1))
        .map_err(|error| error.detail)?
        .request;
    let ProviderRequest::OpenAiCompatible(compatible) = &mut request else {
        return Err("wrong family".into());
    };
    let thinking = serde_json::json!({"kind":"anthropic-thinking-v1","thinking":"t",
        "signature":"s","binding":vec![0u8; 32]});
    let replay = grapt_provider::ProviderReplay::restore(
        ProviderId::new("anthropic").ok_or("provider")?,
        ModelId::new("claude-test").ok_or("model")?,
        None,
        &thinking.to_string(),
    )
    .map_err(|error| error.to_string())?;
    let message = |role, content: String| CodexInputItem::Message(InputMessage { role, content });
    let current = compatible.messages.pop().ok_or("current input")?;
    let start = compatible.messages.len();
    // Anchor, an old exchange holding the foreign thinking, then a recent
    // exchange too large to keep: trimming must stop at the current input.
    compatible.messages.extend([
        message(MessageRole::User, "anchor".into()),
        message(MessageRole::User, "old".into()),
        CodexInputItem::AssistantResponseStart,
        CodexInputItem::ProviderReplay(replay),
        message(MessageRole::Assistant, "old answer".into()),
        message(MessageRole::User, "recent".into()),
        CodexInputItem::AssistantResponseStart,
        message(MessageRole::Assistant, "r".repeat(400_000)),
        current,
    ]);
    let mut boundary = RequestBoundary {
        history_start: start,
        protected_from: start + 8,
    };
    plan.trim_context(&mut request, &mut boundary)
        .map_err(|error| error.detail)?;
    let ProviderRequest::OpenAiCompatible(compatible) = &request else {
        return Err("wrong family".into());
    };
    assert!(!format!("{request:?}").contains("ProviderReplay"));
    assert!(matches!(compatible.messages.last(),
        Some(CodexInputItem::Message(message)) if message.content == "hello"));
    assert_eq!(boundary.protected_from, start + 7);
    Ok(())
}
