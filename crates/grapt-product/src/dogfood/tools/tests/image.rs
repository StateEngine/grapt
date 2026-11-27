use super::*;

#[test]
fn a_foreign_route_cannot_capture_the_reserved_local_image_name() -> Result<(), String> {
    let scratch = Scratch::new()?;
    std::fs::write(scratch.0.join("image.png"), b"not yet read")
        .map_err(|error| error.to_string())?;
    let hijack = routes_with(
        "view_image",
        mcp_route("foreign", "image", &["missing-server"]),
    );
    let prepared = prepare_effect(
        &scratch.0,
        "view_image",
        &input(r#"{"path":"image.png"}"#)?,
        &hijack,
    )?;
    assert!(
        matches!(prepared.target, EffectTarget::Image(_)),
        "foreign route replaced the native image read"
    );
    assert_eq!(prepared.action, "read");
    Ok(())
}

fn png(width: u32, height: u32) -> Result<Vec<u8>, String> {
    let image = ::image::DynamicImage::new_rgba8(width, height);
    let mut encoded = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, ::image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(encoded.into_inner())
}

#[test]
fn viewed_pixels_replay_after_the_source_is_deleted_without_public_base64() -> Result<(), String> {
    let scratch = Scratch::new()?;
    std::fs::write(scratch.0.join("image.png"), png(7, 5)?).map_err(|error| error.to_string())?;
    let input = input(r#"{"path":"image.png"}"#)?;
    let prepared = super::super::image::prepare(&scratch.0, &input)?;
    let EffectTarget::Image(expected) = prepared.target else {
        return Err("image effect missing".into());
    };
    assert_eq!(prepared.action, "read");
    let output = super::super::image::run(&scratch.0, &expected, &input)?;
    std::fs::remove_file(scratch.0.join("image.png")).map_err(|error| error.to_string())?;
    let output = std::str::from_utf8(&output).map_err(|error| error.to_string())?;
    let (description, images) = super::super::image::replay(output)?;
    assert_eq!(description.as_str(), "Viewed 7x5 image/png image.");
    assert_eq!(images.items()[0].dimensions(), (7, 5));
    let mut projection = crate::dogfood::continuation::ToolProjection {
        execution: grapt_types::ToolExecution::Local,
        name: "view_image".into(),
        permission_tool: Some(grapt_provider::ToolName::new("read").ok_or("permission tool")?),
        image_read: Some(crate::dogfood::continuation::LocalImageRead::Permission),
        input: serde_json::json!({"path":"image.png"}),
        input_invalid: None,
        output: output.into(),
        error: None,
        failure_reason: None,
        settled: true,
        never_ran: false,
        progress: None,
        files: Vec::new(),
    };
    let artifacts = FilesystemArtifactStore::open(
        scratch.0.join("artifacts"),
        b"image-projection".to_vec(),
        Box::new(SystemClock),
    )
    .map_err(|error| error.to_string())?;
    for permission in [projection.permission_tool.clone(), None] {
        projection.permission_tool = permission;
        let public = crate::dogfood::history_json::tool_state_json(&projection, &artifacts, true)?;
        assert!(public.contains("Viewed 7x5"));
        assert!(!public.contains(images.items()[0].encoded()));
    }
    let mut legacy = crate::dogfood::continuation::ToolProjection {
        execution: grapt_types::ToolExecution::Local,
        image_read: None,
        permission_tool: Some(
            grapt_provider::ToolName::new("view_image").ok_or("legacy MCP tool")?,
        ),
        output: "Legacy MCP result".into(),
        name: "view_image".into(),
        input: serde_json::json!({}),
        input_invalid: None,
        error: None,
        failure_reason: None,
        settled: true,
        never_ran: false,
        progress: None,
        files: Vec::new(),
    };
    for permission in [legacy.permission_tool.clone(), None] {
        legacy.permission_tool = permission;
        let public = crate::dogfood::history_json::tool_state_json(&legacy, &artifacts, true)?;
        assert!(public.contains("Legacy MCP result"));
        legacy.error = Some("Tool failed".into());
        legacy.output = "Tool failed\nLEGACY_PRIVATE".into();
        legacy.failure_reason = Some(ToolFailureReason::EffectFailed);
        let hidden = crate::dogfood::history_json::tool_state_json(&legacy, &artifacts, true)?;
        assert!(!hidden.contains("LEGACY_PRIVATE"));
        legacy.error = None;
        legacy.output = "Legacy MCP result".into();
        legacy.failure_reason = None;
    }
    projection.permission_tool = None;
    for corrupt in [
        "{\"kind\":\"secode-image-v1\"}".to_owned(),
        output.replace("secode-image-v1", "secode-image-v2"), // grapt-rename: keep
    ] {
        projection.output = corrupt;
        assert!(
            crate::dogfood::history_json::tool_state_json(&projection, &artifacts, true).is_err()
        );
    }
    // An explicit read action establishes native provenance for public detail.
    projection.permission_tool = Some(grapt_provider::ToolName::new("read").ok_or("read action")?);
    let failed = crate::dogfood::continuation::ToolProjection {
        error: Some("Tool failed".into()),
        output: "Tool failed\ninvalid or unsupported image data".into(),
        failure_reason: Some(ToolFailureReason::EffectFailed),
        ..projection
    };
    let failed = crate::dogfood::history_json::tool_state_json(&failed, &artifacts, true)?;
    assert!(failed.contains("invalid or unsupported image data"));

    Ok(())
}

#[test]
fn image_formats_limits_and_original_bytes_are_enforced() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let source = png(9, 6)?;
    std::fs::write(scratch.0.join("image.png"), &source).map_err(|error| error.to_string())?;
    let args = input(r#"{"path":"image.png","detail":"original"}"#)?;
    let target = resolve_existing(&scratch.0, "image.png", false)?;
    let output = super::super::image::run(&scratch.0, &target, &args)?;
    let (_, images) = super::super::image::replay(
        std::str::from_utf8(&output).map_err(|error| error.to_string())?,
    )?;
    use base64::Engine as _;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(images.items()[0].encoded())
            .map_err(|error| error.to_string())?,
        source
    );
    std::fs::write(&target, b"not an image").map_err(|error| error.to_string())?;
    assert!(super::super::image::run(&scratch.0, &target, &args).is_err());
    std::fs::write(&target, png(4097, 1)?).map_err(|error| error.to_string())?;
    assert!(super::super::image::run(&scratch.0, &target, &args).is_err());
    let high = input(r#"{"path":"image.png","detail":"high"}"#)?;
    let output = super::super::image::run(&scratch.0, &target, &high)?;
    let (_, images) = super::super::image::replay(
        std::str::from_utf8(&output).map_err(|error| error.to_string())?,
    )?;
    assert!(images.items()[0].dimensions().0 <= 2048);
    assert!(super::super::image::prepare(
        &scratch.0,
        &input(r#"{"path":"image.png","detail":"low"}"#)?
    )
    .is_err());
    assert!(super::super::image::prepare(
        &scratch.0,
        &input(r#"{"path":"image.png","environment_id":"other"}"#)?
    )
    .is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn image_authorization_rechecks_the_target_and_refuses_outside_links() -> Result<(), String> {
    let scratch = Scratch::new()?;
    std::fs::write(scratch.0.join("first.png"), png(3, 2)?).map_err(|error| error.to_string())?;
    std::fs::write(scratch.0.join("other.png"), png(4, 2)?).map_err(|error| error.to_string())?;
    let args = input(r#"{"path":"first.png"}"#)?;
    let expected = resolve_existing(&scratch.0, "first.png", false)?;
    std::fs::remove_file(&expected).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(scratch.0.join("other.png"), &expected)
        .map_err(|error| error.to_string())?;
    let refusal = super::super::image::run(&scratch.0, &expected, &args)
        .err()
        .ok_or("changed target read")?;
    assert!(refusal.contains("changed after authorization"), "{refusal}");
    std::fs::remove_file(&expected).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink("/etc/hosts", &expected).map_err(|error| error.to_string())?;
    let refusal = super::super::image::prepare(&scratch.0, &args)
        .err()
        .ok_or("outside link admitted")?;
    assert!(refusal.contains("outside the project root"), "{refusal}");
    Ok(())
}

#[test]
fn image_replay_refuses_forged_pixels_dimensions_and_foreign_references() -> Result<(), String> {
    let scratch = Scratch::new()?;
    std::fs::write(scratch.0.join("receipt.png"), png(9, 6)?).map_err(|e| e.to_string())?;
    let args = input(r#"{"path":"receipt.png"}"#)?;
    let expected = resolve_existing(&scratch.0, "receipt.png", false)?;
    let output = super::super::image::run(&scratch.0, &expected, &args)?;
    let value: serde_json::Value = serde_json::from_slice(&output).map_err(|e| e.to_string())?;
    for (key, bad) in [
        ("data", serde_json::json!("YWJj")),
        ("width", serde_json::json!(8)),
        ("uri", serde_json::json!("file:///foreign/pixels.png")),
    ] {
        let mut forged = value.clone();
        forged[key] = bad;
        assert!(
            super::super::image::replay(&forged.to_string()).is_err(),
            "forged {key} accepted"
        );
    }
    Ok(())
}

#[test]
fn image_normalization_rejects_truncated_containers_and_never_upscales_metadata(
) -> Result<(), String> {
    let scratch = Scratch::new()?;
    let args = input(r#"{"path":"bounded.png"}"#)?;
    let target = scratch.0.join("bounded.png");
    let rgb = ::image::DynamicImage::new_rgb8(24, 16);
    for format in [
        ::image::ImageFormat::Jpeg,
        ::image::ImageFormat::Png,
        ::image::ImageFormat::WebP,
    ] {
        let mut encoded = std::io::Cursor::new(Vec::new());
        rgb.write_to(&mut encoded, format)
            .map_err(|e| e.to_string())?;
        let mut encoded = encoded.into_inner();
        encoded.truncate(encoded.len().saturating_sub(2));
        std::fs::write(&target, encoded).map_err(|e| e.to_string())?;
        assert!(
            super::super::image::run(&scratch.0, &target, &args).is_err(),
            "truncated {format:?} accepted"
        );
    }
    // A valid small image with large metadata must retain its own resolution.
    let original = png(64, 32)?;
    let end = original.len().checked_sub(12).ok_or("missing PNG IEND")?;
    let mut body = b"note\0".to_vec();
    body.extend(std::iter::repeat_n(b'a', 800 * 1024));
    let mut extended = original.get(..end).ok_or("missing PNG prefix")?.to_vec();
    extended.extend_from_slice(&(body.len() as u32).to_be_bytes());
    extended.extend_from_slice(b"tEXt");
    extended.extend_from_slice(&body);
    let mut crc = crc32fast::Hasher::new();
    crc.update(b"tEXt");
    crc.update(&body);
    extended.extend_from_slice(&crc.finalize().to_be_bytes());
    extended.extend_from_slice(original.get(end..).ok_or("missing PNG suffix")?);
    std::fs::write(&target, extended).map_err(|e| e.to_string())?;
    let output = super::super::image::run(&scratch.0, &target, &args)?;
    let (_, images) =
        super::super::image::replay(std::str::from_utf8(&output).map_err(|e| e.to_string())?)?;
    assert_eq!(images.items()[0].dimensions(), (64, 32));
    assert!(images.items()[0].encoded().len() < 16 * 1024);
    Ok(())
}

#[test]
fn image_decoders_handle_each_format_and_bound_pixels_before_full_decode() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let path = scratch.0.join("input.image");
    let args = input(r#"{"path":"input.image","detail":"original"}"#)?;
    for format in [
        ::image::ImageFormat::Png,
        ::image::ImageFormat::Jpeg,
        ::image::ImageFormat::Gif,
        ::image::ImageFormat::WebP,
    ] {
        let pixels = if format == ::image::ImageFormat::Jpeg {
            ::image::DynamicImage::new_rgb8(17, 11)
        } else {
            ::image::DynamicImage::new_rgba8(17, 11)
        };
        let mut encoded = std::io::Cursor::new(Vec::new());
        pixels
            .write_to(&mut encoded, format)
            .map_err(|e| e.to_string())?;
        let original = encoded.into_inner();
        std::fs::write(&path, &original).map_err(|e| e.to_string())?;
        let receipt = super::super::image::run(&scratch.0, &path, &args)?;
        let (_, images) =
            super::super::image::replay(std::str::from_utf8(&receipt).map_err(|e| e.to_string())?)?;
        assert_eq!(images.items()[0].dimensions(), (17, 11));
        if format == ::image::ImageFormat::Gif {
            assert_eq!(images.items()[0].media(), grapt_provider::ImageMedia::Png);
        } else {
            use base64::Engine as _;
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(images.items()[0].encoded())
                    .map_err(|e| e.to_string())?,
                original
            );
        }
    }
    let mut bomb = png(8, 8)?;
    bomb[16..20].copy_from_slice(&6000_u32.to_be_bytes());
    bomb[20..24].copy_from_slice(&6000_u32.to_be_bytes());
    let checksum = crc32fast::hash(&bomb[12..29]);
    bomb[29..33].copy_from_slice(&checksum.to_be_bytes());
    std::fs::write(&path, bomb).map_err(|e| e.to_string())?;
    let error = super::super::image::run(&scratch.0, &path, &args)
        .err()
        .ok_or("pixel bomb accepted")?;
    assert!(error.contains("32 megapixel"), "{error}");
    Ok(())
}

#[test]
fn image_high_compresses_large_sources_and_refuses_oversized_files() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let path = scratch.0.join("large.png");
    std::fs::write(&path, png(3000, 2000)?).map_err(|e| e.to_string())?;
    let args = input(r#"{"path":"large.png"}"#)?;
    let receipt = super::super::image::run(&scratch.0, &path, &args)?;
    assert!(receipt.len() <= 1024 * 1024);
    let (_, images) =
        super::super::image::replay(std::str::from_utf8(&receipt).map_err(|e| e.to_string())?)?;
    assert_eq!(images.items()[0].dimensions(), (2048, 1365));
    std::fs::write(&path, vec![0_u8; 16 * 1024 * 1024 + 1]).map_err(|e| e.to_string())?;
    assert!(super::super::image::run(&scratch.0, &path, &args).is_err());
    Ok(())
}

#[test]
fn high_detail_rotates_exif_pixels_without_enlarging() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let path = scratch.0.join("portrait.jpg");
    let mut source = std::io::Cursor::new(Vec::new());
    ::image::DynamicImage::new_rgb8(20, 10)
        .write_to(&mut source, ::image::ImageFormat::Jpeg)
        .map_err(|e| e.to_string())?;
    let source = source.into_inner();
    // EXIF IFD0 orientation 6: a quarter-turn clockwise, stored little endian.
    let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
    let mut oriented = source[..2].to_vec();
    oriented.extend_from_slice(&[0xff, 0xe1]);
    oriented.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    oriented.extend_from_slice(exif);
    oriented.extend_from_slice(&source[2..]);
    std::fs::write(&path, oriented).map_err(|e| e.to_string())?;
    let args = input(r#"{"path":"portrait.jpg","detail":"high"}"#)?;
    let receipt = super::super::image::run(&scratch.0, &path, &args)?;
    let (_, images) =
        super::super::image::replay(std::str::from_utf8(&receipt).map_err(|e| e.to_string())?)?;
    assert_eq!(images.items()[0].dimensions(), (10, 20));
    Ok(())
}

#[test]
fn opaque_noisy_png_uses_bounded_jpeg_without_enlarging() -> Result<(), String> {
    use base64::Engine as _;
    let scratch = Scratch::new()?;
    let path = scratch.0.join("noise.png");
    // A fixed sequence gives incompressible pixels without ambient randomness.
    let mut state = 0x9e37_79b9_u32;
    let pixels = ::image::RgbImage::from_fn(512, 512, |_, _| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        ::image::Rgb([state as u8, (state >> 8) as u8, (state >> 16) as u8])
    });
    pixels.save(&path).map_err(|e| e.to_string())?;
    assert!(std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 700 * 1024);
    let original = input(r#"{"path":"noise.png","detail":"original"}"#)?;
    let error = super::super::image::run(&scratch.0, &path, &original)
        .err()
        .ok_or("oversized original accepted")?;
    assert!(error.contains("use high detail"), "{error}");
    let high = input(r#"{"path":"noise.png","detail":"high"}"#)?;
    let receipt = super::super::image::run(&scratch.0, &path, &high)?;
    let (_, images) =
        super::super::image::replay(std::str::from_utf8(&receipt).map_err(|e| e.to_string())?)?;
    let image = &images.items()[0];
    assert_eq!(image.media(), grapt_provider::ImageMedia::Jpeg);
    assert_eq!(image.dimensions(), (512, 512));
    let encoded = base64::engine::general_purpose::STANDARD
        .decode(image.encoded())
        .map_err(|e| e.to_string())?;
    assert!(encoded.len() <= 700 * 1024);
    assert_eq!(
        ::image::guess_format(&encoded).map_err(|e| e.to_string())?,
        ::image::ImageFormat::Jpeg
    );
    Ok(())
}
