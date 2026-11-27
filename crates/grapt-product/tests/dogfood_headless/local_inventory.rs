//! A local compatible provider supplies its own bounded context metadata.
use super::*;

fn configure(root: &Path, context: Option<u64>) -> Result<(), Box<dyn std::error::Error>> {
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config
        .as_object_mut()
        .ok_or("fixture config")?
        .remove("catalog");
    config["provider"]["id"] = "vllm".into();
    config["provider"]["model"] = "qwen3.8-27b".into();
    config["provider"]["base_model"] = "qwen3.8-27b".into();
    config["provider"]["family"] = "qwen3".into();
    config["provider"]["protocol_family"] = "openai-compatible".into();
    config["provider"]["endpoint"] = "https://local.example/v1".into();
    fs::write(path, serde_json::to_vec(&config)?)?;
    fs::remove_file(root.join("model-catalog.json"))?;
    let listing = serde_json::json!({"data":[
        {"id":"qwen3.8-27b","max_model_len":context}, {"id":"unannounced-model"}
    ]});
    fs::write(
        root.join("account-models.json"),
        serde_json::to_vec(&listing)?,
    )?;
    Ok(())
}

#[test]
fn sc_fr01_local_context_reaches_inference_without_a_public_catalog(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("sc-fr01-local-context");
    prepare(&root, 120_000);
    configure(&root, Some(200_000))?;
    let provider = ProviderFixture::start(vec![Script::CompatibleSuccess]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let (status, bytes) = request(address, "GET", "/api/model", "");
    assert_eq!(status, 200);
    let _: grapt_api::u3a::ModelListResponseDto =
        grapt_api::u3a::decode_page(&bytes, grapt_api::u3a::U3aLimits::production()?)?;
    let models: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(models["data"].as_array().ok_or("model list")?.len(), 2);
    assert_eq!(models["data"][0]["limit"]["context"], 200000);
    assert_eq!(models["data"][1]["limit"]["context"], 0);
    assert!(models["data"][1]["settings"]["metadata"]["unknown"]
        .as_array()
        .ok_or("unknown metadata")?
        .contains(&serde_json::json!("context")));
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_sc_fr01_local","text":"reply briefly","delivery":"queue","resume":true}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    child.stop_after_witness(&root);
    drop(child);
    let bodies = locked(&provider.bodies);
    assert_eq!(bodies.len(), 1, "the local model must reach inference");
    let body: serde_json::Value = serde_json::from_str(&bodies[0])?;
    assert_eq!(body["model"], "qwen3.8-27b");
    drop(bodies);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn sc_fr01_missing_context_fails_startup_before_any_provider_request(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for missing_listing in [false, true] {
        let root = root(if missing_listing {
            "sc-fr01-missing-listing"
        } else {
            "sc-fr01-missing-context"
        });
        prepare(&root, 120_000);
        configure(&root, None)?;
        if missing_listing {
            fs::remove_file(root.join("account-models.json"))?;
        }
        let provider = ProviderFixture::start(vec![Script::CompatibleSuccess]);
        let mut child = spawn(&root, provider.address);
        let mut exited = false;
        let wait = PollDeadline::within(Duration::from_secs(30));
        while wait.pending() {
            if child.try_wait()?.is_some() {
                exited = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            exited,
            "missing metadata did not refuse startup within 30 seconds"
        );
        let output = child.wait_with_output()?;
        assert!(!output.status.success());
        let diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(diagnostic.contains("account-models.json"), "{diagnostic}");
        if !missing_listing {
            assert!(diagnostic.contains("context limit"), "{diagnostic}");
            assert!(diagnostic.contains("vllm/qwen3.8-27b"), "{diagnostic}");
        }
        assert!(!root.join("serve-ready.json").exists());
        assert!(locked(&provider.bodies).is_empty());
        process_fixture::cleanup(&root);
    }
    Ok(())
}
