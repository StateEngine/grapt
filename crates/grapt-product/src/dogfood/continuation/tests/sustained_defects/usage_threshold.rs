//! Reported context ceiling across schema refreshes and compaction.
use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn provider_reported_context_compacts_at_ninety_percent_before_the_next_call() -> Result<(), String>
{
    use grapt_provider::ProtocolFamily;
    for family in [
        ProtocolFamily::ChatGptCodex,
        ProtocolFamily::OpenAiResponses,
        ProtocolFamily::AnthropicMessages,
        ProtocolFamily::OpenAiCompatible,
    ] {
        for (limit, output, compact) in [
            (None, 49_999, false),
            (None, 50_000, true),
            (Some(850_000), 0, true),
            (Some(950_000), 50_000, true),
        ] {
            for auto in [true, false] {
                let fixture = seeded(&format!("early-compact-{family:?}-{output}"))?;
                fixture.record_exchange()?;
                let targets = crate::dogfood::production::tests::projection_targets_with_window(
                    family, 1_000_000,
                )?;
                let first_target = targets.issue()?;
                let slot = admission::DiagnosisSlot::default();
                let mut plan = crate::dogfood::production::ProductPlan::new(
                    family,
                    "use the tool".into(),
                    grapt_provider::PromptCacheKey::for_session(&fixture.session)
                        .map_err(|error| error.to_string())?,
                    Some(first_target),
                    targets,
                    Arc::new(crate::dogfood::mcp::PublishedTools::default()),
                    fixture.continuation(fixture.input.clone())?,
                    StepNumber::new(1).ok_or("step")?,
                )
                .with_admission_diagnosis(slot.clone())
                .with_compaction(
                    crate::dogfood::compaction_config::CompactionSettings {
                        auto,
                        model_auto_compact_token_limit: limit,
                        server_side: Some(false),
                        ..Default::default()
                    },
                );
                let first = plan
                    .plan(
                        StepNumber::new(2).ok_or("step")?,
                        1,
                        grapt_runtime::HistoryGeneration::new(1),
                    )
                    .map_err(|error| error.detail)?;
                assert_eq!(plan.context_admission(&first), Ok(None));
                let attempt = usage_step_output(&fixture, 2, 850_000, output)?;
                plan.admitted(&attempt);
                assert_eq!(
                    fixture
                        .continuation(fixture.input.clone())?
                        .confirmed_usage(&attempt)
                        .map(|usage| usage.context),
                    Ok(Some(850_000 + output))
                );
                let mut next = plan
                    .plan(
                        StepNumber::new(3).ok_or("step")?,
                        1,
                        grapt_runtime::HistoryGeneration::new(1),
                    )
                    .map_err(|error| error.detail)?;
                // Refresh the actual schema sent to admission, preserving target
                // and committed context. The old prefix digest must change.
                let tools = match &mut next.request {
                    grapt_provider::ProviderRequest::ChatGptCodex(request) => &mut request.tools,
                    grapt_provider::ProviderRequest::OpenAiResponses(request) => &mut request.tools,
                    grapt_provider::ProviderRequest::AnthropicMessages(request)
                    | grapt_provider::ProviderRequest::VertexAnthropic(request) => {
                        &mut request.tools
                    }
                    grapt_provider::ProviderRequest::BedrockConverse(request) => &mut request.tools,
                    grapt_provider::ProviderRequest::OpenAiCompatible(request)
                    | grapt_provider::ProviderRequest::WatsonxChat(request) => &mut request.tools,
                    grapt_provider::ProviderRequest::GoogleGemini(request)
                    | grapt_provider::ProviderRequest::VertexGemini(request) => &mut request.tools,
                    grapt_provider::ProviderRequest::CohereChat(request) => &mut request.tools,
                };
                tools
                    .first_mut()
                    .ok_or("fixture has no tools to refresh")?
                    .input_schema =
                    r#"{"type":"object","properties":{"refreshed":{"type":"boolean"}}}"#.into();
                assert_ne!(
                    grapt_provider::prefix_digest(&first.request, 0),
                    grapt_provider::prefix_digest(&next.request, 0)
                );
                assert_eq!(first.context, next.context);
                assert_eq!(
                    plan.context_admission(&next),
                    Ok(compact.then_some(grapt_types::CompactionTrigger::PolicyThreshold)),
                    "{family:?}: use the earlier configured limit or 90% of the context window",
                );
                // The normal trigger leaves room for the provider compaction call.
                assert_eq!(slot.retain_fitting_request(1_000_000), Ok(()));
                slot.record_tokens(1_000_001, Some(850_000));
                assert_eq!(slot.retain_fitting_request(1_000_000), Ok(()));
                assert_eq!(slot.coding_request(), Some(first.request));
                if compact {
                    stage_cut_at_head(&fixture, "reported-context-replaced")?;
                    let rebuilt = plan
                        .plan(
                            StepNumber::new(3).ok_or("step")?,
                            1,
                            grapt_runtime::HistoryGeneration::new(2),
                        )
                        .map_err(|error| error.detail)?;
                    assert_eq!(
                        plan.context_admission(&rebuilt),
                        Ok(None),
                        "compacted context reused old usage"
                    );
                }
            }
        }
    }
    Ok(())
}
