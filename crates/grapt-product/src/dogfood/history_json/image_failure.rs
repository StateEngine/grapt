//! Safe public causes for a durably identified local image read.
//!
//! Decoder errors map to a fixed public
//! refusal. Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Modified by Grapt: durable provenance, bounded causes and no receipt bytes.
use super::*;

pub(super) fn detail(
    projection: &continuation::ToolProjection,
    cause: &str,
) -> Option<&'static str> {
    use grapt_types::ToolFailureReason as Failure;
    if projection.image_read.is_none() || projection.execution != grapt_types::ToolExecution::Local
    {
        return None;
    }
    match projection.failure_reason? {
        Failure::EffectFailed => Some(match cause {
            "invalid or unsupported image data" => "invalid or unsupported image data; use PNG, JPEG, GIF or WebP",
            "supported image formats are PNG, JPEG, GIF and WebP" => "supported image formats are PNG, JPEG, GIF and WebP",
            "image container is truncated or corrupt" => "image container is truncated or corrupt",
            "file byte bound exceeded" | "image source exceeds 16 MiB" => "image source exceeds 16 MiB; use a smaller image",
            "image exceeds the 32 megapixel decode bound" => "image exceeds the 32 megapixel decode bound; use a smaller image",
            "original image exceeds 700 KiB or 4096 pixels; use high detail" => "original image exceeds 700 KiB or 4096 pixels; use high detail",
            "original image exceeds 4096 pixels; use high detail" => "original image exceeds 4096 pixels; use high detail",
            "image path is not a regular file" => "image path is not a regular file",
            "image target changed after authorization" | "image target changed during reading" => "image target changed; retry with a stable workspace file",
            cause if cause.starts_with("tool preparation failed: ") => "image path or arguments could not be prepared; use an existing workspace file and high or original detail",
            cause if cause.starts_with("image decoder refused input: ") || cause.starts_with("image decode failed: ") => "image decoder refused the input; check that the image is valid and within the decode limits",
            cause if cause.starts_with("unreadable: ") || cause.starts_with("image metadata: ") => "image file could not be read; check that it exists and is readable",
            _ => "image could not be processed; check the image format, size and detail setting",
        }),
        Failure::AuthorityUnavailable if cause.starts_with("read is denied by ")
            && cause.ends_with("; do not repeat the call") =>
        {
            Some("image read is denied by this Session's permission policy; do not repeat the call unless the policy changes")
        }
        Failure::AuthorityUnavailable if cause == "the permission request was rejected; do not repeat the call unless the decision changes" => Some("the permission request was rejected; do not repeat the call unless the decision changes"),
        Failure::ApprovalUnavailable => Some("image read needs an attached approval client; attach a client and retry"),
        _ => None,
    }
}
