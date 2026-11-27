//! Real wire requests keep chronological effort across selection and process restart.
use super::*;

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn selected_effort_changes_append_controls_and_recover_from_durable_history(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for native in [false, true] {
        let root = root(if native {
            "native-effort-updates"
        } else {
            "codex-effort-updates"
        });
        prepare(&root, 120_000);
        let (vendor, model) = if native {
            context_quality::configure_anthropic(&root, 200_000);
            for name in ["model-catalog.json", "anthropic-account-models.json"] {
                let path = root.join(name);
                fs::write(
                    &path,
                    fs::read_to_string(&path)?.replace("claude-live", "claude-opus-5-5"),
                )?;
            }
            let path = root.join("anthropic-account-models.json");
            let mut inventory: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            inventory["models"][0]["supported_reasoning_levels"] = serde_json::json!([
                {"effort":"low"}, {"effort":"medium"}, {"effort":"high"}
            ]);
            fs::write(path, serde_json::to_vec(&inventory)?)?;
            ("anthropic", "claude-opus-5-5")
        } else {
            configure_codex_effort(&root, Some("medium"))?;
            ("openai", "gpt-live")
        };
        let scripts = if native {
            vec![
                Script::Raw(context_quality::anthropic_stream(
                    &[
                        serde_json::json!({"type":"thinking","thinking":"first analysis","signature":"EFFORT-SIGNATURE"}),
                        serde_json::json!({"type":"text","text":"first answer"}),
                    ],
                    "end_turn",
                    serde_json::json!({"input_tokens":20}),
                )),
                context_quality::anthropic_text(
                    "second answer",
                    serde_json::json!({"input_tokens":30}),
                ),
                context_quality::anthropic_text(
                    "third answer",
                    serde_json::json!({"input_tokens":40}),
                ),
            ]
        } else {
            vec![
                Script::SuccessText("first answer"),
                Script::SuccessText("second answer"),
                Script::SuccessText("third answer"),
            ]
        };
        let provider = ProviderFixture::start(scripts);
        let mut child = spawn(&root, provider.address);
        let mut address = await_ready(&root, &mut child);
        let session = create(address);
        for (index, effort) in ["medium", "high", "low"].iter().enumerate() {
            if index == 2 {
                child.stop_after_witness(&root);
                drop(child);
                child = spawn(&root, provider.address);
                address = await_ready(&root, &mut child);
            }
            let (status, detail) = request(
                address,
                "POST",
                &format!("/api/session/{session}/model"),
                &serde_json::json!({"model":{"providerID":vendor,"id":model,"variant":effort}})
                    .to_string(),
            );
            assert_eq!(
                status,
                204,
                "selection native={native} effort={effort}: {}",
                String::from_utf8_lossy(&detail)
            );
            json(address, "POST", &format!("/api/session/{session}/prompt"),
                &serde_json::json!({"id":format!("msg_effort_{index}"),"text":format!("input {index}"),"delivery":"queue","resume":true}).to_string());
            permission_residuals::wait_session_idle(address, &session);
            assert_eq!(provider.calls.load(Ordering::SeqCst), index + 1);
        }
        let bodies = locked(&provider.bodies)
            .iter()
            .map(|raw| serde_json::from_str::<serde_json::Value>(raw))
            .collect::<Result<Vec<_>, _>>()?;
        for body in &bodies {
            assert_eq!(body["model"], model);
            if native {
                assert_eq!(body["output_config"]["effort"], "medium");
            } else {
                assert_eq!(body["reasoning"]["effort"], "medium");
            }
        }
        if native {
            let messages = bodies[2]["messages"].as_array().ok_or("native messages")?;
            let controls: Vec<_> = messages
                .iter()
                .filter(|message| {
                    message["role"] == "system" && message["content"] == serde_json::json!([])
                })
                .collect();
            assert_eq!(controls.len(), 2);
            assert_eq!(controls[0]["output_config"]["effort"], "high");
            assert_eq!(controls[1]["output_config"]["effort"], "low");
            assert!(messages
                .iter()
                .flat_map(|message| message["content"].as_array().into_iter().flatten())
                .any(
                    |block| block["type"] == "thinking" && block["signature"] == "EFFORT-SIGNATURE"
                ));
            assert!(locked(&provider.heads).iter().all(|head| head
                .to_ascii_lowercase()
                .contains("anthropic-beta: mid-conversation-output-config-2026-07-01")));
            assert_eq!(bodies[0]["system"], bodies[2]["system"]);
        } else {
            for pair in bodies.windows(2) {
                let earlier = pair[0]["input"].as_array().ok_or("earlier input")?;
                let later = pair[1]["input"].as_array().ok_or("later input")?;
                assert_eq!(&later[..earlier.len()], earlier);
            }
            let controls: Vec<_> = bodies[2]["input"]
                .as_array()
                .ok_or("input")?
                .iter()
                .filter(|item| item["type"] == "configuration_update")
                .collect();
            assert_eq!(controls.len(), 2);
            assert_eq!(controls[0]["reasoning"]["effort"], "high");
            assert_eq!(controls[1]["reasoning"]["effort"], "low");
        }
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&root);
    }
    Ok(())
}

fn configure_codex_effort(
    root: &Path,
    default: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut model = serde_json::json!({
        "slug":"gpt-live","visibility":"list","context_window":128_000,
        "supports_reasoning_effort_updates":true,
        "supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"high"}]
    });
    if let Some(default) = default {
        model["default_reasoning_level"] = serde_json::json!(default);
    }
    fs::write(
        root.join("account-models.json"),
        serde_json::to_vec(&serde_json::json!({"models":[model]}))?,
    )?;
    Ok(())
}

#[test]
fn discovering_a_default_does_not_invent_an_earlier_effort(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("unknown-default-effort");
    prepare(&root, 120_000);
    configure_codex_effort(&root, None)?;
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("unknown effort answer"),
        Script::SuccessText("medium effort answer"),
        Script::SuccessText("low effort answer"),
    ]);
    let mut child = spawn(&root, provider.address);
    let mut address = await_ready(&root, &mut child);
    let session = create(address);
    for (index, effort) in [None, Some("medium"), Some("low")].iter().enumerate() {
        if index == 2 {
            child.stop_after_witness(&root);
            drop(child);
            configure_codex_effort(&root, Some("high"))?;
            child = spawn(&root, provider.address);
            address = await_ready(&root, &mut child);
        }
        let mut model = serde_json::json!({"providerID":"openai","id":"gpt-live"});
        if let Some(effort) = effort {
            model["variant"] = serde_json::json!(effort);
        }
        let (status, detail) = request(
            address,
            "POST",
            &format!("/api/session/{session}/model"),
            &serde_json::json!({"model":model}).to_string(),
        );
        assert_eq!(
            status,
            204,
            "selection: {}",
            String::from_utf8_lossy(&detail)
        );
        json(address, "POST", &format!("/api/session/{session}/prompt"),
            &serde_json::json!({"id":format!("msg_unknown_effort_{index}"),"text":format!("input {index}"),"delivery":"queue","resume":true}).to_string());
        permission_residuals::wait_session_idle(address, &session);
        assert_eq!(provider.calls.load(Ordering::SeqCst), index + 1);
    }
    let bodies = locked(&provider.bodies)
        .iter()
        .map(|raw| serde_json::from_str::<serde_json::Value>(raw))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(bodies[0]["reasoning"].get("effort").is_none());
    assert_eq!(bodies[1]["reasoning"]["effort"], "medium");
    assert_eq!(bodies[2]["reasoning"]["effort"], "medium");
    let earlier = bodies[1]["input"].as_array().ok_or("earlier input")?;
    let later = bodies[2]["input"].as_array().ok_or("later input")?;
    assert_eq!(&later[..earlier.len()], earlier);
    let controls: Vec<_> = later
        .iter()
        .filter(|item| item["type"] == "configuration_update")
        .collect();
    assert_eq!(controls.len(), 1);
    assert_eq!(controls[0]["reasoning"]["effort"], "low");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
