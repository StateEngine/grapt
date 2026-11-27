use super::*;

#[test]
fn patch_catalog_stays_identical_across_requests_and_retries() -> Result<(), String> {
    for family in [
        ProtocolFamily::OpenAiResponses,
        ProtocolFamily::ChatGptCodex,
        ProtocolFamily::AnthropicMessages,
        ProtocolFamily::OpenAiCompatible,
    ] {
        let native = matches!(
            family,
            ProtocolFamily::OpenAiResponses | ProtocolFamily::ChatGptCodex
        );
        let (offered, withheld) = if native {
            ("apply_patch", "patch")
        } else {
            ("patch", "apply_patch")
        };
        let mut previous = None;
        for prompt in ["first request", "later request"] {
            let concrete = targets(family)?;
            let step = StepNumber::new(1).ok_or("Step")?;
            let (continuation, _scratch) =
                DurableContinuation::with_prompt_for_test("patch_catalog", prompt)?;
            let mut plan = ProductPlan::new(
                family,
                prompt.into(),
                cache_key("patch_catalog")?,
                None,
                concrete,
                Arc::new(crate::dogfood::mcp::PublishedTools::default()),
                continuation,
                step,
            )
            .with_shell_description("Available shells: fixture-shell-list.".into());
            for ordinal in 1..=2 {
                let planned = plan
                    .plan(step, ordinal, HistoryGeneration::new(1))
                    .map_err(|error| error.detail)?;
                let tools = planned.request.tools();
                assert_eq!(
                    tools
                        .iter()
                        .find(|tool| tool.name == "exec_command")
                        .and_then(|tool| tool.description.as_deref()),
                    Some("Available shells: fixture-shell-list.")
                );
                assert_eq!(tools.iter().filter(|tool| tool.name == offered).count(), 1);
                assert!(!tools.iter().any(|tool| tool.name == withheld));
                let visible = ToolName::new(offered.to_owned()).ok_or("offered tool")?;
                let hidden = ToolName::new(withheld.to_owned()).ok_or("withheld tool")?;
                assert!(planned.authority.tool_is_visible(&visible));
                assert!(!planned.authority.tool_is_visible(&hidden));
                if let Some(previous) = &previous {
                    assert_eq!(tools, previous);
                }
                previous = Some(tools.to_vec());
            }
        }
        let all = crate::dogfood::tools::definitions();
        assert!(all.iter().any(|tool| tool.name == offered));
        assert!(all.iter().any(|tool| tool.name == withheld));
    }
    Ok(())
}
