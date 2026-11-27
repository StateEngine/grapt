//! Explicit local image paths are prepared before any session is created.
use grapt_api::u3a::PromptInputFileAttachmentDto;
use std::path::PathBuf;
pub(super) fn load(paths: &[PathBuf]) -> Result<Vec<PromptInputFileAttachmentDto>, String> {
    if paths.len() > 4 {
        return Err("a prompt supports at most four images".into());
    }
    paths
        .iter()
        .map(|path| {
            let image = grapt_image::read_path(path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default();
            serde_json::from_value(serde_json::json!({"uri":image.data_url(),"name":name}))
                .map_err(|error| format!("image attachment: {error}"))
        })
        .collect()
}
