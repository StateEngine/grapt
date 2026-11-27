//! Actual model/effort selection and emitted requests, including restart and CLI.
use super::*;

#[test]
fn reasoning_effort_crosses_each_provider_and_survives_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for family in [
        "openai-responses",
        "chatgpt-codex",
        "anthropic-messages",
        "openai-compatible",
    ] {
        run(family, false, false)?;
    }
    Ok(())
}
#[test]
fn reasoning_effort_config_default_uses_discovered_levels_without_a_second_catalog(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    run("openai-responses", true, false)
}
#[test]
fn reasoning_effort_headless_flag_refuses_unsupported_and_reaches_the_provider(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    run("openai-responses", false, true)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn run(family: &str, config_default: bool, cli: bool) -> Result<(), Box<dyn std::error::Error>> {
    let root = root(&format!("reasoning-effort-{family}-{config_default}-{cli}"));
    prepare(&root, 200_000);
    let (provider_id, model, listing, script) = match family {
        "anthropic-messages" => {
            context_quality::configure_anthropic(&root, 200_000);
            for name in ["model-catalog.json", "anthropic-account-models.json"] {
                let path = root.join(name);
                let text = fs::read_to_string(&path)?;
                fs::write(path, text.replace("claude-live", "claude-opus-5-5"))?;
            }
            (
                "anthropic",
                "claude-opus-5-5",
                "anthropic-account-models.json",
                vec![
                    context_quality::anthropic_text("done", serde_json::json!({"input_tokens":20})),
                    context_quality::anthropic_text("done", serde_json::json!({"input_tokens":20})),
                ],
            )
        }
        "openai-compatible" => {
            compatible_cache::configure_openrouter(&root, "reasoning-live");
            (
                "openrouter",
                "reasoning-live",
                "openrouter-account-models.json",
                vec![Script::CompatibleSuccess, Script::CompatibleSuccess],
            )
        }
        _ => (
            "openai",
            "gpt-live",
            "account-models.json",
            vec![Script::Success, Script::Success],
        ),
    };
    let path = root.join(listing);
    let mut account: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    account["models"][0]["supported_reasoning_levels"] =
        serde_json::json!([{"effort":"low"},{"effort":"high"}]);
    account["models"][0]["default_reasoning_level"] = serde_json::json!("low");
    fs::write(path, serde_json::to_vec(&account)?)?;
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    if provider_id == "openai" {
        config["provider"]["family"] = serde_json::json!(family);
    }
    if config_default {
        config["provider"]["selected_variant"] = serde_json::json!("high");
    }
    fs::write(path, serde_json::to_vec(&config)?)?;
    let mut script = script;
    if cli {
        script.push(Script::Success);
    }
    let provider = ProviderFixture::start(script);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let selection =
        serde_json::json!({"model":{"providerID":provider_id,"id":model,"variant":"high"}});
    if !config_default && !cli {
        assert_eq!(
            request(
                address,
                "POST",
                &format!("/api/session/{session}/model"),
                &selection.to_string()
            )
            .0,
            204
        );
        let mut invalid = selection.clone();
        invalid["model"]["variant"] = serde_json::json!("unsupported");
        assert_eq!(
            request(
                address,
                "POST",
                &format!("/api/session/{session}/model"),
                &invalid.to_string()
            )
            .0,
            400
        );
        assert_eq!(
            json(address, "GET", &format!("/api/session/{session}"), "")["data"]["model"]
                ["variant"],
            "high"
        );
    }
    if cli {
        let service = root.to_str().ok_or("service path")?;
        let bad = cli_continuity::cli(
            &root,
            &[
                "run",
                "--service",
                service,
                "--session",
                &session,
                "--reasoning-effort",
                "unsupported",
                "refuse this",
            ],
        );
        assert_ne!(bad.code, Some(0));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let good = cli_continuity::cli(
            &root,
            &[
                "run",
                "--service",
                service,
                "--session",
                &session,
                "--reasoning-effort",
                "high",
                "use this effort",
            ],
        );
        assert_eq!(good.code, Some(0), "{}", good.stderr);
        let reset = cli_continuity::cli(
            &root,
            &[
                "run",
                "--service",
                service,
                "--session",
                &session,
                "--reasoning-effort",
                "default",
                "return to account effort",
            ],
        );
        assert_eq!(reset.code, Some(0), "{}", reset.stderr);
    } else {
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"use the selected effort"}"#,
        );
        permission_residuals::wait_session_idle(address, &session);
    }
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"continue after reopening"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        if cli { 3 } else { 2 }
    );
    for (index, body) in locked(&provider.bodies).iter().enumerate() {
        let body: serde_json::Value = serde_json::from_str(body)?;
        assert_eq!(body["model"], model);
        let effort = match family {
            "anthropic-messages" => {
                assert_eq!(body["thinking"]["type"], "adaptive");
                &body["output_config"]["effort"]
            }
            "openai-compatible" => &body["reasoning_effort"],
            _ => &body["reasoning"]["effort"],
        };
        assert_eq!(
            effort,
            if cli && index > 0 { "low" } else { "high" },
            "{body}"
        );
    }
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
