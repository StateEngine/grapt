//! Bounded image content for ephemeral provider requests.
//!
//! Modified by Grapt: closed media, dimensions and request budgets; no file I/O.
//! The product validates and prepares image bytes before constructing this value.

pub const MAX_IMAGE_ENCODED_BYTES: usize = 1_398_104;
pub const MAX_REQUEST_IMAGES: usize = 128;
const MAX_REQUEST_IMAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMedia {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl ImageMedia {
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageDetail {
    High,
    Original,
}

impl ImageDetail {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Original => "original",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct InputImage {
    media: ImageMedia,
    encoded: String,
    detail: ImageDetail,
    width: u32,
    height: u32,
}

impl std::fmt::Debug for InputImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputImage")
            .field("media", &self.media)
            .field("detail", &self.detail)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("encoded_bytes", &self.encoded.len())
            .finish_non_exhaustive()
    }
}

impl InputImage {
    /// The image decoder owns format and pixel validation. This constructor
    /// enforces the request's dimensions and base64 envelope before transport.
    pub fn try_new(
        media: ImageMedia,
        encoded: String,
        detail: ImageDetail,
        width: u32,
        height: u32,
    ) -> Result<Self, &'static str> {
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err("image dimensions must be between 1 and 4096 pixels");
        }
        if encoded.is_empty()
            || encoded.len() > MAX_IMAGE_ENCODED_BYTES
            || !encoded.len().is_multiple_of(4)
        {
            return Err("image base64 is empty or exceeds its byte bound");
        }
        let body = encoded.trim_end_matches('=');
        if encoded.len() - body.len() > 2
            || body.is_empty()
            || !body
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
        {
            return Err("image base64 envelope is malformed");
        }
        Ok(Self {
            media,
            encoded,
            detail,
            width,
            height,
        })
    }

    pub const fn media(&self) -> ImageMedia {
        self.media
    }
    pub fn encoded(&self) -> &str {
        &self.encoded
    }
    pub const fn detail(&self) -> ImageDetail {
        self.detail
    }
    pub const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media.mime(), self.encoded)
    }

    /// A geometry reserve for admission, never provider-confirmed token usage.
    pub fn estimated_tokens(&self) -> u64 {
        512 + 512 * u64::from(self.width.div_ceil(512)) * u64::from(self.height.div_ceil(512))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputImages(Vec<InputImage>);

impl InputImages {
    pub fn try_new(images: Vec<InputImage>) -> Result<Self, &'static str> {
        if images.is_empty() || images.len() > 4 {
            return Err("image content requires between one and four images");
        }
        if images
            .iter()
            .map(|image| image.encoded.len())
            .sum::<usize>()
            > MAX_REQUEST_IMAGE_BYTES
        {
            return Err("image content exceeds its aggregate byte bound");
        }
        Ok(Self(images))
    }

    pub fn items(&self) -> &[InputImage] {
        &self.0
    }
    /// Preserve pixels while falling back to high when the selected model
    /// does not advertise original-detail preprocessing.
    pub fn normalise_detail(&mut self, original_supported: bool) {
        if !original_supported {
            for image in &mut self.0 {
                if image.detail == ImageDetail::Original {
                    image.detail = ImageDetail::High;
                }
            }
        }
    }
    pub fn byte_len(&self) -> usize {
        self.0.iter().map(|image| image.encoded.len()).sum()
    }
    pub fn estimated_tokens(&self) -> u64 {
        self.0.iter().map(InputImage::estimated_tokens).sum()
    }

    pub(crate) fn digest_fields(&self, field: &mut impl FnMut(&[u8])) {
        field(&(self.0.len() as u64).to_be_bytes());
        for image in &self.0 {
            field(image.media.mime().as_bytes());
            field(image.detail.as_str().as_bytes());
            field(&image.width.to_be_bytes());
            field(&image.height.to_be_bytes());
            field(image.encoded.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_image_reserve_covers_the_frontier_patch_cost() -> Result<(), &'static str> {
        // The documented GPT-5.6 original-detail cost for 4096 square pixels:
        // ceil(128 * 128 * 1.2) = 19661. This is an admission regression floor,
        // not a claim that every provider uses this tokenizer or billing rate.
        let image = InputImage::try_new(
            ImageMedia::Png,
            "YWJj".into(),
            ImageDetail::Original,
            4096,
            4096,
        )?;
        assert!(image.estimated_tokens() >= 19_661);
        Ok(())
    }

    #[test]
    fn image_envelopes_are_bounded_and_debug_never_contains_pixels() -> Result<(), &'static str> {
        let image =
            InputImage::try_new(ImageMedia::Png, "YWJj".into(), ImageDetail::High, 800, 600)?;
        assert!(!format!("{image:?}").contains("YWJj"));
        assert_eq!(image.estimated_tokens(), 2560);
        assert!(InputImages::try_new(vec![image; 5]).is_err());
        assert!(InputImages::try_new(Vec::new()).is_err());
        for encoded in ["", "x", "====", "A===", "AA=A", "AA\n="] {
            assert!(
                InputImage::try_new(ImageMedia::Png, encoded.into(), ImageDetail::High, 1, 1)
                    .is_err()
            );
        }
        assert!(
            InputImage::try_new(ImageMedia::Png, "AAAA".into(), ImageDetail::High, 4097, 1)
                .is_err()
        );
        assert!(InputImage::try_new(
            ImageMedia::Png,
            "AAAA".repeat(349_527),
            ImageDetail::High,
            1,
            1
        )
        .is_err());
        Ok(())
    }
}
