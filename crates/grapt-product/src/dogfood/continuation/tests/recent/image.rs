//! Captured images stay binary in model input and metadata in prose context.
use super::*;

#[test]
fn captured_image_pixels_replay_fully_but_recent_prose_contains_only_the_description(
) -> Result<(), String> {
    use base64::Engine as _;
    let fixture = Fixture::new("image-recent")?;
    let mut png = std::io::Cursor::new(Vec::new());
    ::image::DynamicImage::new_rgb8(8, 6)
        .write_to(&mut png, ::image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let mut png = png.into_inner();
    let end = png.split_off(png.len() - 12);
    let payload = [b"note\0".as_slice(), &vec![b'x'; 128 * 1024]].concat();
    png.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let checksum = crc32fast::hash(&[b"tEXt".as_slice(), &payload].concat());
    png.extend_from_slice(b"tEXt");
    png.extend_from_slice(&payload);
    png.extend_from_slice(&checksum.to_be_bytes());
    png.extend_from_slice(&end);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
    let receipt = format!(
        "{{\"kind\":\"secode-image-v1\",\"data\":\"{encoded}\",\"mime\":\"image/png\",\"width\":8,\"height\":6,\"detail\":\"original\"}}"
    );
    assert!(receipt.len() > 65_536);
    {
        let conn = raw_conn(&fixture)?;
        seed_promoted_rows(&conn, &fixture.session)?;
    }
    fixture.record_exchange_output(true, false, false, "view_image", "read", receipt.as_bytes())?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let request = continuation.request_input("use the tool", true)?;
    let image = request
        .iter()
        .find_map(|item| match item {
            CodexInputItem::ImageToolOutput { images, .. } => images.items().first(),
            _ => None,
        })
        .ok_or("captured image missing from actual continuation")?;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(image.encoded())
            .map_err(|e| e.to_string())?,
        png
    );
    commit_descriptor(
        &fixture,
        "evt_image_recent",
        &anchor_descriptor(&fixture, "image-recent", 0, 2)?,
    )?;
    let head = max_session_seq(&raw_conn(&fixture)?, &fixture.session)?;
    let inbox = lock(fixture.inbox()?);
    let recent = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, head)?;
    let text = recent.as_str().ok_or("recent image context is not text")?;
    assert!(text.contains("Viewed 8x6 image/png image."), "{text}");
    assert!(!text.contains("secode-image-v1"));
    assert!(!text.contains(&encoded[..64]));
    Ok(())
}

#[test]
fn malformed_native_image_receipts_degrade_in_recent_context_but_legacy_mcp_results_remain_text(
) -> Result<(), String> {
    for (label, action, output, degrades) in [
        (
            "bad-dimensions",
            "read",
            r#"{"kind":"secode-image-v1","data":"YWJj","mime":"image/png","width":0,"height":8,"detail":"high"}"#,
            true,
        ),
        (
            "bad-version",
            "read",
            r#"{"kind":"grapt-image-v9","data":"YWJj","mime":"image/png","width":8,"height":8,"detail":"high"}"#,
            true,
        ),
        (
            "bad-marker",
            "read",
            r#"{"kind":"foreign","data":"YWJj","mime":"image/png","width":8,"height":8,"detail":"high"}"#,
            true,
        ),
        (
            "legacy-collision",
            "view_image",
            r#"{"kind":"secode-image-v1","data":"LEGACY_MCP_TEXT"}"#,
            false,
        ),
    ] {
        let fixture = Fixture::new(label)?;
        {
            let conn = raw_conn(&fixture)?;
            seed_promoted_rows(&conn, &fixture.session)?;
        }
        fixture.record_exchange_output(
            true,
            false,
            false,
            "view_image",
            action,
            output.as_bytes(),
        )?;
        commit_descriptor(
            &fixture,
            "evt_image_bad",
            &anchor_descriptor(&fixture, "image-bad", 0, 2)?,
        )?;
        let head = max_session_seq(&raw_conn(&fixture)?, &fixture.session)?;
        let inbox = lock(fixture.inbox()?);
        let recent = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, head)?;
        let text = recent.as_str().ok_or("recent text absent")?;
        if degrades {
            assert!(text.contains("unavailable or corrupt"), "{text}");
            assert!(
                !text.contains("YWJj"),
                "corrupt native receipt leaked: {text}"
            );
        } else {
            assert!(text.contains("LEGACY_MCP_TEXT"), "{text}");
        }
    }
    Ok(())
}

#[test]
fn hosted_image_name_collision_remains_provider_data_in_all_history_views() -> Result<(), String> {
    let fixture = Fixture::new("hosted-image-collision")?;
    let output = r#"{"kind":"secode-image-v1","data":"HOSTED_RESULT"}"#;
    {
        let conn = raw_conn(&fixture)?;
        seed_promoted_rows(&conn, &fixture.session)?;
    }
    fixture.record_owned_exchange_call(
        (true, false, false),
        "view_image",
        "read",
        output.as_bytes(),
        "{}",
        ToolExecution::ProviderHosted,
    )?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let request = continuation.request_input("use the tool", true)?;
    assert!(!request
        .iter()
        .any(|item| matches!(item, CodexInputItem::ImageToolOutput { .. })));
    let call = ToolCallId::new("call_d2a_read").ok_or("call identity")?;
    let projection = tool_projection(
        fixture.inbox()?,
        fixture.artifacts()?,
        &fixture.session,
        &call,
    )?;
    let public =
        crate::dogfood::history_json::tool_state_json(&projection, fixture.artifacts()?, true)?;
    assert!(public.contains("HOSTED_RESULT"));
    commit_descriptor(
        &fixture,
        "evt_hosted_image",
        &anchor_descriptor(&fixture, "hosted-image", 0, 2)?,
    )?;
    let head = max_session_seq(&raw_conn(&fixture)?, &fixture.session)?;
    let inbox = lock(fixture.inbox()?);
    let recent = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, head)?;
    let text = recent.as_str().ok_or("hosted recent context absent")?;
    assert!(text.contains("HOSTED_RESULT"));
    Ok(())
}

/// An explicit local image read needs no permission row or leading marker:
/// its valid receipt renders in public history as the description, not data.
#[test]
fn an_unpermitted_explicit_image_receipt_renders_its_description() -> Result<(), String> {
    use base64::Engine as _;
    let fixture = Fixture::new("image-explicit-history")?;
    let mut png = std::io::Cursor::new(Vec::new());
    ::image::DynamicImage::new_rgb8(8, 6)
        .write_to(&mut png, ::image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    let receipt = format!(
        " {{\"data\":\"{encoded}\",\"kind\":\"secode-image-v1\",\"mime\":\"image/png\",\"width\":8,\"height\":6,\"detail\":\"original\"}}"
    );
    let tool = crate::dogfood::tools::image::TOOL;
    fixture.record_exchange_output(true, false, false, tool, "read", receipt.as_bytes())?;
    raw_conn(&fixture)?
        .execute("DELETE FROM event WHERE type LIKE 'permission.%'", [])
        .map_err(|e| e.to_string())?;
    let call = ToolCallId::new("call_d2a_read").ok_or("call")?;
    let artifacts = fixture.artifacts()?;
    let projection = tool_projection(fixture.inbox()?, artifacts, &fixture.session, &call)?;
    let state = crate::dogfood::history_json::tool_state_json(&projection, artifacts, true)?;
    assert!(!state.contains(&encoded), "{state}");
    assert!(!state.contains("unavailable or corrupt"), "{state}");
    Ok(())
}
