//! Decode and resize once before immutable capture; no filesystem or authority.
//! Modified by Grapt: decode, pixel and encoded-output bounds; no shared cache.
//! PNG/JPEG quality selection also.
//! Copyright 2023-2026 SpaceXAI. Apache-2.0.
use crate::{validation, Detail, ImageMedia, PREPARED_BYTES};
use ::image::{DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader};
use std::io::Cursor;

#[derive(Debug)]
pub struct PreparedImage {
    pub bytes: Vec<u8>,
    pub media: ImageMedia,
    pub width: u32,
    pub height: u32,
}

pub fn prepare(bytes: Vec<u8>, detail: Detail) -> Result<PreparedImage, String> {
    prepare_with_transparency(bytes, detail, false)
}

pub(crate) fn prepare_with_transparency(
    bytes: Vec<u8>,
    detail: Detail,
    source_transparency: bool,
) -> Result<PreparedImage, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("image source exceeds 16 MiB".into());
    }
    let format = ::image::guess_format(&bytes).map_err(|_| "invalid or unsupported image data")?;
    let media = match format {
        ImageFormat::Png => ImageMedia::Png,
        ImageFormat::Jpeg => ImageMedia::Jpeg,
        ImageFormat::Gif => ImageMedia::Gif,
        ImageFormat::WebP => ImageMedia::Webp,
        _ => return Err("supported image formats are PNG, JPEG, GIF and WebP".into()),
    };
    let (mut decoded, orientation) = decode(&bytes, format, None)?;
    let (width, height) = decoded.dimensions();
    if matches!(detail, Detail::Original) && format != ImageFormat::Gif {
        if bytes.len() > PREPARED_BYTES || width > 4096 || height > 4096 {
            return Err("original image exceeds 700 KiB or 4096 pixels; use high detail".into());
        }
        return Ok(PreparedImage {
            bytes,
            media,
            width,
            height,
        });
    }
    decoded.apply_orientation(orientation);
    let (width, height) = decoded.dimensions();
    // Check the source once: resizing can round away a transparent pixel.
    let opaque = !source_transparency && is_opaque(&decoded);
    // Preserve alpha by using PNG, with a fixed, decreasing sequence of sizes.
    // Each encoding is independently bounded even for incompressible pixels.
    let original = matches!(detail, Detail::Original);
    if original && (width > 4096 || height > 4096) {
        return Err("original image exceeds 4096 pixels; use high detail".into());
    }
    let longest = width.max(height);
    let mut previous = None;
    for ceiling in [2048, 1536, 1024, 768, 512, 256] {
        let dimension = if original {
            longest
        } else {
            ceiling.min(longest)
        };
        if previous == Some(dimension) {
            continue;
        }
        previous = Some(dimension);
        let resized = decoded.resize(
            dimension,
            dimension,
            ::image::imageops::FilterType::Triangle,
        );
        let mut encoded = Cursor::new(Vec::new());
        resized
            .write_to(&mut encoded, ImageFormat::Png)
            .map_err(|error| format!("image encode failed: {error}"))?;
        if encoded.get_ref().len() <= PREPARED_BYTES {
            return Ok(PreparedImage {
                bytes: encoded.into_inner(),
                media: ImageMedia::Png,
                width: resized.width(),
                height: resized.height(),
            });
        }
        // Preserve alpha losslessly; opaque photos can keep more detail as JPEG.
        if opaque {
            for quality in [85, 70, 50, 40] {
                let mut jpeg = Vec::new();
                ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality)
                    .encode_image(&resized)
                    .map_err(|error| format!("image JPEG encode failed: {error}"))?;
                if jpeg.len() <= PREPARED_BYTES {
                    return Ok(PreparedImage {
                        bytes: jpeg,
                        media: ImageMedia::Jpeg,
                        width: resized.width(),
                        height: resized.height(),
                    });
                }
            }
        }
    }
    Err("prepared image exceeds the capture bound".into())
}

/// Inspect native alpha samples without allocating or rounding 16-bit values.
/// GenericImageView::pixels converts to RGBA8 and can hide slight transparency.
fn is_opaque(image: &DynamicImage) -> bool {
    use DynamicImage::{ImageLumaA16, ImageLumaA8, ImageRgba16, ImageRgba8};
    match image {
        ImageLumaA8(image) => image.pixels().all(|pixel| pixel.0[1] == u8::MAX),
        ImageRgba8(image) => image.pixels().all(|pixel| pixel.0[3] == u8::MAX),
        ImageLumaA16(image) => image.pixels().all(|pixel| pixel.0[1] == u16::MAX),
        ImageRgba16(image) => image.pixels().all(|pixel| pixel.0[3] == u16::MAX),
        // Any other alpha-bearing format remains lossless conservatively.
        other => !other.color().has_alpha(),
    }
}

/// Captured pixels must agree with the immutable receipt before provider input.
pub fn validate_capture(
    bytes: &[u8],
    media: ImageMedia,
    expected: (u32, u32),
) -> Result<(), String> {
    let format = match media {
        ImageMedia::Png => ImageFormat::Png,
        ImageMedia::Jpeg => ImageFormat::Jpeg,
        ImageMedia::Gif => ImageFormat::Gif,
        ImageMedia::Webp => ImageFormat::WebP,
    };
    decode(bytes, format, Some(expected))?;
    Ok(())
}

fn decode(
    bytes: &[u8],
    format: ImageFormat,
    expected: Option<(u32, u32)>,
) -> Result<(DynamicImage, ::image::metadata::Orientation), String> {
    if !validation::complete(format, bytes) {
        return Err("image container is truncated or corrupt".into());
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = ::image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(192 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| format!("image decoder refused input: {error}"))?;
    let (header_width, header_height) = decoder.dimensions();
    if u64::from(header_width) * u64::from(header_height) > 32_000_000 {
        return Err("image exceeds the 32 megapixel decode bound".into());
    }
    if expected.is_some_and(|size| size != (header_width, header_height)) {
        return Err("image pixels disagree with the captured dimensions".into());
    }
    let orientation = decoder
        .orientation()
        .map_err(|error| format!("image orientation: {error}"))?;
    let decoded = DynamicImage::from_decoder(decoder)
        .map_err(|error| format!("image decode failed: {error}"))?;
    Ok((decoded, orientation))
}
