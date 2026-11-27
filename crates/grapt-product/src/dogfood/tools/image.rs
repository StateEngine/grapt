//! Local image viewing through existing read authority and immutable output capture.
//!
//! Modified by Grapt: bounded decoding, artifact-backed replay, governed reads.

mod prepare_image;

use super::*;
use base64::Engine as _;
use grapt_provider::{CodexToolText, ImageDetail, ImageMedia, InputImage, InputImages};
use serde::Serialize;

pub(in crate::dogfood) const TOOL: &str = "view_image";

/// Older auto-approved MCP calls can share this name without a permission
/// question. Recognize our captured representation before decoding it; this
/// marker does not grant authority or make a failure detail public.
pub(in crate::dogfood) fn has_receipt(output: &str) -> bool {
    output.starts_with("{\"kind\":\"secode-image-")
}
pub(super) const DESCRIPTION: &str = "View workspace PNG/JPEG/GIF/WebP (GIF first frame) with read permission. high (default) ≤ 2048px; original ≤ 4096px/700 KiB, falling back to high without model support. Source ≤ 16 MiB/32 MP.";
const PREPARED_BYTES: usize = 700 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Detail {
    High,
    Original,
}

impl ProviderFieldSchema for Detail {
    fn provider_schema() -> serde_json::Value {
        serde_json::json!({"type":"string", "enum":["high","original"]})
    }
}

impl Detail {
    fn provider(self) -> ImageDetail {
        match self {
            Self::High => ImageDetail::High,
            Self::Original => ImageDetail::Original,
        }
    }
}

tool_arguments! {
    pub(super) struct Arguments => TOOL {
        required path: NonEmptyString,
        optional detail: Detail,
    }
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: Arguments = decode_input(input)?;
    let target = resolve_existing(root, args.path.as_str(), false)?;
    let resource = path_text(&target);
    Ok(PreparedEffect {
        capability: filesystem_capability(EffectClass::FilesystemRead, &resource, false),
        target: EffectTarget::Image(target),
        action: "read".into(),
        resource,
    })
}

/// Image bytes are captured inside the ordinary tool-output artifact. A
/// receipt cannot name another artifact or acquire another session's pixels.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    kind: String,
    data: String,
    mime: String,
    width: u32,
    height: u32,
    detail: Detail,
}

pub(super) fn run(
    root: &Path,
    expected: &Path,
    input: &BoundedToolInput,
) -> Result<Vec<u8>, String> {
    let args: Arguments = decode_input(input)?;
    let target = resolve_existing(root, args.path.as_str(), false)?;
    if target != expected {
        return Err("image target changed after authorization".into());
    }
    let metadata =
        std::fs::metadata(&target).map_err(|error| format!("image metadata: {error}"))?;
    if !metadata.is_file() {
        return Err("image path is not a regular file".into());
    }
    let bytes = super::coding::read_bounded(&target, 16 * 1024 * 1024)?;
    if resolve_existing(root, args.path.as_str(), false)? != target {
        return Err("image target changed during reading".into());
    }
    let detail = args.detail.unwrap_or(Detail::High);
    let prepared = prepare_image::prepare(bytes, detail)?;
    let receipt = serde_json::to_vec(&Receipt {
        kind: "secode-image-v1".into(),
        data: base64::engine::general_purpose::STANDARD.encode(prepared.bytes),
        mime: prepared.media.mime().into(),
        width: prepared.width,
        height: prepared.height,
        detail,
    })
    .map_err(|error| format!("image receipt: {error}"))?;
    if receipt.len() > FILE_EFFECT_MAX_BYTES as usize {
        return Err("image receipt exceeds its capture bound".into());
    }
    Ok(receipt)
}

/// Explicitly supplied user bytes reuse preparation and the immutable receipt format.
pub(in crate::dogfood) fn prepare_input(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let image = prepare_image::prepare(bytes, Detail::High)?;
    serde_json::to_vec(&Receipt {
        kind: "secode-image-v1".into(),
        data: base64::engine::general_purpose::STANDARD.encode(image.bytes),
        mime: image.media.mime().into(),
        width: image.width,
        height: image.height,
        detail: Detail::High,
    })
    .map_err(|e| format!("user image receipt: {e}"))
}

/// Rebuild pixels from the successful committed output, never the source path.
pub(in crate::dogfood) fn replay(output: &str) -> Result<(CodexToolText, InputImages), String> {
    if output.len() > FILE_EFFECT_MAX_BYTES as usize {
        return Err("image receipt exceeds its byte bound".into());
    }
    let receipt: Receipt = serde_json::from_str(output)
        .map_err(|error| format!("image receipt is corrupt: {error}"))?;
    if receipt.kind != "secode-image-v1" {
        return Err("image receipt version is unsupported".into());
    }
    let media = match receipt.mime.as_str() {
        "image/png" => ImageMedia::Png,
        "image/jpeg" => ImageMedia::Jpeg,
        "image/webp" => ImageMedia::Webp,
        "image/gif" => ImageMedia::Gif,
        _ => return Err("image receipt has an unsupported media type".into()),
    };
    let pixels = base64::engine::general_purpose::STANDARD
        .decode(&receipt.data)
        .map_err(|_| "image receipt has malformed base64")?;
    if pixels.len() > PREPARED_BYTES {
        return Err("image receipt pixels exceed their byte bound".into());
    }
    prepare_image::validate_capture(&pixels, media, (receipt.width, receipt.height))?;
    let image = InputImage::try_new(
        media,
        receipt.data,
        receipt.detail.provider(),
        receipt.width,
        receipt.height,
    )?;
    let description = CodexToolText::try_new(format!(
        "Viewed {}x{} {} image.",
        receipt.width, receipt.height, receipt.mime
    ))
    .map_err(|error| error.to_string())?;
    Ok((description, InputImages::try_new(vec![image])?))
}
