//! Image-tool adapter to the common bounded pixel preparation owner.
use super::*;
pub(super) struct PreparedImage {
    pub bytes: Vec<u8>,
    pub media: ImageMedia,
    pub width: u32,
    pub height: u32,
}
pub(super) fn prepare(bytes: Vec<u8>, detail: Detail) -> Result<PreparedImage, String> {
    let detail = match detail {
        Detail::High => grapt_image::Detail::High,
        Detail::Original => grapt_image::Detail::Original,
    };
    let image = grapt_image::prepare(bytes, detail)?;
    Ok(PreparedImage {
        bytes: image.bytes,
        media: provider_media(image.media),
        width: image.width,
        height: image.height,
    })
}
fn provider_media(media: grapt_image::ImageMedia) -> ImageMedia {
    match media {
        grapt_image::ImageMedia::Png => ImageMedia::Png,
        grapt_image::ImageMedia::Jpeg => ImageMedia::Jpeg,
        grapt_image::ImageMedia::Gif => ImageMedia::Gif,
        grapt_image::ImageMedia::Webp => ImageMedia::Webp,
    }
}
pub(super) fn validate_capture(
    bytes: &[u8],
    media: ImageMedia,
    size: (u32, u32),
) -> Result<(), String> {
    let media = match media {
        ImageMedia::Png => grapt_image::ImageMedia::Png,
        ImageMedia::Jpeg => grapt_image::ImageMedia::Jpeg,
        ImageMedia::Gif => grapt_image::ImageMedia::Gif,
        ImageMedia::Webp => grapt_image::ImageMedia::Webp,
    };
    grapt_image::validate_capture(bytes, media, size)
}
