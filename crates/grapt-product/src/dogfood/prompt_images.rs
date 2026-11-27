//! Explicit user pixels: prepare, capture durably, then bind references in inbox admission.
use super::*;
use base64::Engine as _;
use grapt_artifact::{ArtifactKind, ArtifactStore, CaptureBound, Sensitivity};
use grapt_types::{ArtifactRef, CaptureDisposition};

pub(super) fn capture(
    app: &DogfoodApplication,
    request: &Value,
) -> Result<Vec<ArtifactRef>, String> {
    let files = request
        .get("files")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut images = Vec::new();
    for file in files {
        let uri = file
            .get("uri")
            .and_then(Value::as_str)
            .ok_or("attachment URI is absent")?;
        if !uri.starts_with("data:image/") {
            continue;
        }
        if images.len() == 4 {
            return Err("a prompt supports at most four images".into());
        }
        let (header, data) = uri.split_once(',').ok_or("inline image data is absent")?;
        if !matches!(
            header,
            "data:image/png;base64"
                | "data:image/jpeg;base64"
                | "data:image/gif;base64"
                | "data:image/webp;base64"
        ) {
            return Err("image attachment must be base64 PNG, JPEG, GIF or WebP".into());
        }
        if data.len() > 1024 * 1024 {
            return Err("image attachment exceeds its input bound".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| "image attachment base64 is invalid")?;
        // Decode the actual bytes rather than trusting the declared MIME header.
        let receipt = tools::image::prepare_input(bytes)?;
        let bound = CaptureBound::new(1024 * 1024, 8192).ok_or("image capture bound is invalid")?;
        let captured = app
            .executor
            .artifacts()
            .capture(
                std::io::Cursor::new(receipt),
                bound,
                ArtifactKind::CapturedOutput,
                Sensitivity::SecretBearing,
            )
            .map_err(|_| "image attachment capture failed")?;
        if !captured.disposition().is_complete() {
            return Err("image attachment capture was truncated".into());
        }
        images.push(captured.reference().clone());
    }
    Ok(images)
}

pub(super) fn input(
    artifacts: &grapt_artifact::FilesystemArtifactStore,
    user: &grapt_inbox::PromotedMessage,
) -> Result<grapt_provider::CodexInputItem, String> {
    use grapt_provider::{CodexInputItem, CodexToolText, InputImages, InputMessage, MessageRole};
    let refs = if user.kind == grapt_inbox::PromotedInputKind::User {
        grapt_inbox::user_image_references(user.metadata.as_ref())?
    } else {
        Vec::new()
    };
    if refs.is_empty() {
        return Ok(CodexInputItem::Message(InputMessage {
            role: MessageRole::User,
            content: user.text.clone(),
        }));
    }
    let mut images = Vec::new();
    for reference in refs {
        let receipt = continuation::read_text(
            artifacts,
            &reference,
            CaptureDisposition::Complete,
            "user image",
        )?;
        let (_, one) = tools::image::replay(receipt.as_str())?;
        images.extend_from_slice(one.items());
    }
    Ok(CodexInputItem::ImageUser {
        text: CodexToolText::try_new(user.text.clone()).map_err(|e| e.to_string())?,
        images: InputImages::try_new(images)?,
    })
}

#[cfg(test)]
mod tests;
