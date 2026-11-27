//! Actual provider image input survives reopening without the mutable source file.
use super::*;
use base64::Engine as _;

#[path = "image_refusals.rs"]
mod refusals;

#[test]
fn local_image_reaches_the_provider_and_reopens_without_rereading_the_path(
) -> Result<(), Box<dyn std::error::Error>> {
    image_roundtrip("openai-responses", true, true)
}

#[test]
fn a_model_without_original_detail_receives_high_and_keeps_the_exact_pixels(
) -> Result<(), Box<dyn std::error::Error>> {
    image_roundtrip("openai-responses", false, true)
}

#[test]
fn changing_to_a_text_model_keeps_the_stored_image_and_sends_an_explicit_text_result(
) -> Result<(), Box<dyn std::error::Error>> {
    image_roundtrip("openai-responses", true, false)
}

#[test]
fn codex_local_image_reaches_the_provider_and_reopens() -> Result<(), Box<dyn std::error::Error>> {
    image_roundtrip("chatgpt-codex", true, true)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn image_roundtrip(
    family: &str,
    original_supported: bool,
    reopened_supports_images: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root(if original_supported {
        "view-image-native-original"
    } else {
        "view-image-native-high"
    });
    prepare(&root, 200_000);
    let config_path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&config_path)?)?;
    config["provider"]["family"] = serde_json::json!(family);
    fs::write(config_path, serde_json::to_vec(&config)?)?;
    let catalog_path = root.join("model-catalog.json");
    let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&catalog_path)?)?;
    catalog["openai"]["models"]["gpt-live"]["modalities"]["input"] =
        serde_json::json!(["text", "image"]);
    fs::write(catalog_path, serde_json::to_vec(&catalog)?)?;
    let account_path = root.join("account-models.json");
    let mut account: serde_json::Value = serde_json::from_slice(&fs::read(&account_path)?)?;
    account["models"][0]["supports_image_detail_original"] = serde_json::json!(original_supported);
    fs::write(account_path, serde_json::to_vec(&account)?)?;
    let path = root.join("pixels.png");
    image::RgbaImage::from_pixel(8, 6, image::Rgba([22, 132, 245, 255])).save(&path)?;
    let original = fs::read(&path)?;
    let mut script = vec![
        Script::ToolInput {
            call: "call_view_image",
            tool: "view_image",
            arguments: serde_json::json!({"path":"pixels.png", "detail":"original"}),
        },
        Script::Success,
        Script::Success,
    ];
    if !reopened_supports_images {
        script.push(Script::Success);
    }
    let provider = ProviderFixture::start(script);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/preset"),
        r#"{"preset":"inspect"}"#,
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        json(address, "GET", &format!("/api/session/{session}"), "")["data"]["preset"],
        "inspect"
    );
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"look at pixels.png"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )
    .to_string();
    assert!(history.contains("Viewed 8x6 image/png image."), "{history}");
    assert!(
        !history.contains("secode-image-v1"),
        "public history exposed the binary receipt"
    );
    child.stop_after_witness(&root);
    drop(child);
    fs::remove_file(path)?;
    if !reopened_supports_images {
        let path = root.join("model-catalog.json");
        let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        catalog["openai"]["models"]["gpt-live"]["modalities"]["input"] =
            serde_json::json!(["text"]);
        fs::write(path, serde_json::to_vec(&catalog)?)?;
    }
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"continue after reopening"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    if !reopened_supports_images {
        child.stop_after_witness(&root);
        drop(child);
        let path = root.join("model-catalog.json");
        let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        catalog["openai"]["models"]["gpt-live"]["modalities"]["input"] =
            serde_json::json!(["text", "image"]);
        fs::write(path, serde_json::to_vec(&catalog)?)?;
        child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"use the image again after restoring vision"}"#,
        );
        permission_residuals::wait_session_idle(address, &session);
    }
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        if reopened_supports_images { 3 } else { 4 }
    );
    let bodies = locked(&provider.bodies).clone();
    for (index, body) in bodies.iter().enumerate().skip(1) {
        let body: serde_json::Value = serde_json::from_str(body)?;
        let result = body["input"]
            .as_array()
            .ok_or("input list absent")?
            .iter()
            .find(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "call_view_image"
            })
            .ok_or("committed image result absent")?;
        if index == 2 && !reopened_supports_images {
            let text = result["output"]
                .as_str()
                .ok_or("nonvision result is not text")?;
            assert!(text.contains("Viewed 8x6 image/png image."));
            assert!(text.contains("selected model does not advertise image input"));
            assert!(!text.contains("data:image"));
            assert!(body["tools"]
                .as_array()
                .ok_or("tool list absent")?
                .iter()
                .all(|tool| tool["name"] != "view_image"));
            continue;
        }
        let image = result["output"]
            .as_array()
            .ok_or("image result was flattened to text")?
            .iter()
            .find(|item| item["type"] == "input_image")
            .ok_or("provider did not receive image input")?;
        let data = image["image_url"]
            .as_str()
            .and_then(|url| url.strip_prefix("data:image/png;base64,"))
            .ok_or("provider image URL absent")?;
        assert_eq!(
            base64::engine::general_purpose::STANDARD.decode(data)?,
            original
        );
        assert_eq!(
            image["detail"],
            if original_supported {
                "original"
            } else {
                "high"
            }
        );
    }
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn anthropic_local_image_reaches_the_provider_and_reopens() -> Result<(), Box<dyn std::error::Error>>
{
    messages_image_roundtrip(true)
}

#[test]
fn compatible_local_image_reaches_the_provider_and_reopens(
) -> Result<(), Box<dyn std::error::Error>> {
    messages_image_roundtrip(false)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn messages_image_roundtrip(anthropic: bool) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root(if anthropic {
        "image-anthropic"
    } else {
        "image-compatible"
    });
    prepare(&root, 200_000);
    let (provider_id, model) = if anthropic {
        context_quality::configure_anthropic(&root, 200_000);
        ("anthropic", "claude-live")
    } else {
        compatible_cache::configure_openrouter(&root, "vision-live");
        ("openrouter", "vision-live")
    };
    let catalog_path = root.join("model-catalog.json");
    let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&catalog_path)?)?;
    catalog[provider_id]["models"][model]["modalities"]["input"] =
        serde_json::json!(["text", "image"]);
    fs::write(catalog_path, serde_json::to_vec(&catalog)?)?;
    let path = root.join("pixels.png");
    image::RgbImage::from_pixel(8, 6, image::Rgb([22, 132, 245])).save(&path)?;
    let original = fs::read(&path)?;
    let arguments = serde_json::json!({"path":"pixels.png", "detail":"original"});
    let script = if anthropic {
        vec![
            Script::Raw(context_quality::anthropic_stream(
                &[serde_json::json!({
                    "type":"tool_use", "id":"call_image", "name":"view_image", "input":arguments
                })],
                "tool_use",
                serde_json::json!({"input_tokens":10}),
            )),
            context_quality::anthropic_text("seen", serde_json::json!({"input_tokens":10})),
            context_quality::anthropic_text("still seen", serde_json::json!({"input_tokens":10})),
        ]
    } else {
        let delta = serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{
            "index":0,"id":"call_image","type":"function",
            "function":{"name":"view_image","arguments":arguments.to_string()}
        }]},"finish_reason":null}]});
        vec![Script::Raw(format!("data: {delta}\n\ndata: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n")),
            Script::CompatibleSuccess, Script::CompatibleSuccess]
    };
    let provider = ProviderFixture::start(script);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/model"),
        &serde_json::json!({"model":{"providerID":provider_id,"id":model}}).to_string(),
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/preset"),
        r#"{"preset":"inspect"}"#,
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"view pixels.png"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    child.stop_after_witness(&root);
    drop(child);
    fs::remove_file(path)?;
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"inspect the saved image again"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    for body in locked(&provider.bodies).iter().skip(1) {
        let body: serde_json::Value = serde_json::from_str(body)?;
        let messages = body["messages"]
            .as_array()
            .ok_or("provider messages absent")?;
        let encoded = if anthropic {
            let result = messages
                .iter()
                .filter_map(|m| m["content"].as_array())
                .flatten()
                .find(|c| c["type"] == "tool_result" && c["tool_use_id"] == "call_image")
                .ok_or("image tool result absent")?;
            let image = result["content"]
                .as_array()
                .ok_or("image result flattened")?
                .iter()
                .find(|c| c["type"] == "image")
                .ok_or("binary image absent")?;
            assert_eq!(image["source"]["media_type"], "image/png");
            image["source"]["data"]
                .as_str()
                .ok_or("image bytes absent")?
        } else {
            assert!(messages
                .iter()
                .any(|m| m["role"] == "tool" && m["tool_call_id"] == "call_image"));
            let image = messages
                .iter()
                .filter(|m| m["role"] == "user")
                .filter_map(|m| m["content"].as_array())
                .flatten()
                .find(|c| c["type"] == "image_url")
                .ok_or("image user block absent")?;
            assert_eq!(image["image_url"]["detail"], "high");
            image["image_url"]["url"]
                .as_str()
                .and_then(|s| s.strip_prefix("data:image/png;base64,"))
                .ok_or("binary image URL absent")?
        };
        assert_eq!(
            base64::engine::general_purpose::STANDARD.decode(encoded)?,
            original
        );
    }
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
