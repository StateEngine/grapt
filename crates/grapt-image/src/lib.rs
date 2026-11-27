//! Bounded pixel preparation for explicit user images and governed image tools.
mod input;
mod prepare;
mod validation;
pub use input::read_path;
pub use prepare::{prepare, validate_capture, PreparedImage};

pub const PREPARED_BYTES: usize = 700 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    High,
    Original,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMedia {
    Png,
    Jpeg,
    Gif,
    Webp,
}
impl ImageMedia {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}
/// Clipboard RGBA is bounded before encoding and uses the same image pipeline.
pub fn prepare_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Result<PreparedImage, String> {
    let expected = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(4))
        .ok_or("clipboard dimensions overflow")?;
    if width == 0 || height == 0 || expected > 128_000_000 || expected != pixels.len() as u64 {
        return Err("clipboard image exceeds 32 megapixels or has invalid pixels".into());
    }
    let transparent = pixels.chunks_exact(4).any(|pixel| pixel[3] != u8::MAX);
    let image = image::RgbaImage::from_raw(width, height, pixels)
        .ok_or("clipboard pixel dimensions disagree")?;
    let image = image::DynamicImage::ImageRgba8(image);
    let dimension = 2048.min(width.max(height));
    let image = image.resize(dimension, dimension, image::imageops::FilterType::Triangle);
    let mut encoded = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Png)
        .map_err(|e| format!("clipboard image encode: {e}"))?;
    prepare::prepare_with_transparency(encoded.into_inner(), Detail::High, transparent)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipboard_dimensions_and_pixel_count_are_checked_before_encoding() {
        assert!(prepare_rgba(0, 1, vec![]).is_err());
        assert!(prepare_rgba(100_000, 100_000, vec![]).is_err());
        assert!(prepare_rgba(2, 2, vec![0; 15]).is_err());
    }
    #[test]
    fn clipboard_png_preserves_small_geometry_and_alpha() -> Result<(), String> {
        let prepared = prepare_rgba(2, 2, vec![128; 16])?;
        assert_eq!((prepared.width, prepared.height), (2, 2));
        assert_eq!(prepared.media, ImageMedia::Png);
        validate_capture(&prepared.bytes, prepared.media, (2, 2))?;
        Ok(())
    }
    /// Bounded noise: PNG stays large while JPEG fits.
    fn noise(width: u32, height: u32, alpha: u8) -> Vec<u8> {
        (0..width * height)
            .flat_map(|index| {
                let hash = u32::wrapping_mul(index, 0x9e37_79b9);
                let hash = (hash ^ (hash >> 16)).wrapping_mul(0x85eb_ca6b);
                let [r, g, b, _] = hash.to_le_bytes().map(|value| 112 + value / 8);
                [r, g, b, if index == 0 { alpha } else { 255 }]
            })
            .collect()
    }

    /// The PNG size of the same buffer, taken through the same reduction the
    /// preparation applies before it chooses a format.
    fn png_bytes_of(width: u32, height: u32, alpha: u8) -> Result<usize, String> {
        let image = ::image::RgbaImage::from_raw(width, height, noise(width, height, alpha))
            .ok_or("pixel dimensions disagree")?;
        let image = ::image::DynamicImage::ImageRgba8(image);
        let dimension = 2048.min(width.max(height));
        let reduced = image.resize(
            dimension,
            dimension,
            ::image::imageops::FilterType::Triangle,
        );
        let mut encoded = std::io::Cursor::new(Vec::new());
        reduced
            .write_to(&mut encoded, ::image::ImageFormat::Png)
            .map_err(|error| format!("fixture PNG encode: {error}"))?;
        Ok(encoded.get_ref().len())
    }

    #[test]
    fn opaque_rgba_uses_jpeg_before_shrinking_but_transparency_stays_png() -> Result<(), String> {
        for (width, height, alpha, media) in [
            (1024, 1024, 255, ImageMedia::Jpeg),
            (1024, 512, 254, ImageMedia::Png),
            (4096, 512, 254, ImageMedia::Png),
        ] {
            let prepared = prepare_rgba(width, height, noise(width, height, alpha))?;
            assert_eq!(prepared.media, media);
            if alpha == 255 {
                // Byte proof ALONGSIDE the geometry: the opaque source's PNG is
                // over the bound, so JPEG is chosen for size, not by accident.
                let png = png_bytes_of(width, height, alpha)?;
                assert!(
                    png > PREPARED_BYTES,
                    "the opaque fixture's PNG is {png} B, within {PREPARED_BYTES}, so it \
                     never exercised the JPEG fallback"
                );
                // And no reduction preceded it: the source geometry survives.
                assert_eq!((prepared.width, prepared.height), (1024, 1024));
            }
        }
        Ok(())
    }
}
