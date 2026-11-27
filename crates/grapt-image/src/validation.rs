//! Container completeness before lenient pixel decoders or original passthrough.
//! Copyright 2023-2026 SpaceXAI. Apache-2.0. Modified by Grapt:
//! caller-bounded bytes, closed raster formats, first-frame GIF transcoding.
use ::image::ImageFormat;

pub(super) fn complete(format: ImageFormat, bytes: &[u8]) -> bool {
    match format {
        ImageFormat::Jpeg => jpeg_reaches_eoi(bytes),
        ImageFormat::Png => png_structurally_valid(bytes),
        ImageFormat::WebP => webp_riff_complete(bytes),
        // GIF is always decoded and re-encoded as a static image.
        ImageFormat::Gif => true,
        _ => false,
    }
}

pub fn jpeg_reaches_eoi(bytes: &[u8]) -> bool {
    let [0xFF, 0xD8, ..] = bytes else {
        return false;
    };
    let mut i = 2;
    loop {
        // Find the next marker: skip stray garbage, then FF fill bytes.
        while bytes.get(i).is_some_and(|&b| b != 0xFF) {
            i += 1;
        }
        while bytes.get(i).copied() == Some(0xFF) {
            i += 1;
        }
        let Some(&marker) = bytes.get(i) else {
            return false;
        };
        i += 1;
        match marker {
            // Not a marker (stuffed/stray `FF00`): keep scanning.
            0x00 => {}
            0xD9 => return true, // EOI
            // Standalone markers without a length field.
            0x01 | 0xD0..=0xD7 => {}
            0xDA => {
                // SOS: skip the length-prefixed header, then scan the
                // entropy-coded stream for the next real marker.
                let Some(next) = skip_segment(bytes, i) else {
                    return false;
                };
                i = next;
                loop {
                    while bytes.get(i).is_some_and(|&b| b != 0xFF) {
                        i += 1;
                    }
                    match bytes.get(i + 1).copied() {
                        // Byte-stuffed FF or fill byte: still entropy data.
                        Some(0x00) => i += 2,
                        Some(0xFF) => i += 1,
                        // Restart marker: entropy data continues after it.
                        Some(0xD0..=0xD7) => i += 2,
                        Some(_) => break,
                        None => return false,
                    }
                }
            }
            _ => {
                let Some(next) = skip_segment(bytes, i) else {
                    return false;
                };
                i = next;
            }
        }
    }
}

/// Skip a length-prefixed JPEG segment starting at its 2-byte length field.
/// Returns the offset just past the segment, or `None` if it runs off the end.
fn skip_segment(bytes: &[u8], at: usize) -> Option<usize> {
    let len_bytes = bytes.get(at..at + 2)?;
    let [hi, lo] = len_bytes else {
        return None;
    };
    let len = usize::from(*hi) << 8 | usize::from(*lo);
    if len < 2 {
        return None;
    }
    let end = at.checked_add(len)?;
    (end <= bytes.len()).then_some(end)
}

pub fn png_structurally_valid(bytes: &[u8]) -> bool {
    const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    let n = bytes.len();
    if !bytes.starts_with(PNG_SIG) {
        return false;
    }
    let mut i = PNG_SIG.len();
    // Each chunk: 4-byte length, 4-byte type, data, 4-byte CRC (over
    // type + data).
    while let Some(header) = bytes.get(i..i + 8) {
        let [l0, l1, l2, l3, t0, t1, t2, t3] = header else {
            return false;
        };
        let len = u32::from_be_bytes([*l0, *l1, *l2, *l3]) as usize;
        let is_iend = [*t0, *t1, *t2, *t3] == *b"IEND";
        let data_start = i + 8;
        let Some(data_end) = data_start.checked_add(len) else {
            return false;
        };
        let Some(end) = data_end.checked_add(4) else {
            return false;
        };
        if end > n {
            return false;
        }
        let Some(crc) = bytes.get(data_end..data_end + 4) else {
            return false;
        };
        let [c0, c1, c2, c3] = crc else {
            return false;
        };
        let expected = u32::from_be_bytes([*c0, *c1, *c2, *c3]);
        let Some(crc_span) = bytes.get(i + 4..data_end) else {
            return false;
        };
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(crc_span);
        if hasher.finalize() != expected {
            return false;
        }
        if is_iend {
            return true;
        }
        i = end;
    }
    false
}

pub fn webp_riff_complete(bytes: &[u8]) -> bool {
    let Some(header) = bytes.get(..12) else {
        return false;
    };
    if header.get(..4) != Some(&b"RIFF"[..]) || header.get(8..12) != Some(&b"WEBP"[..]) {
        return false;
    }
    let Some(size_bytes) = header.get(4..8) else {
        return false;
    };
    let [s0, s1, s2, s3] = size_bytes else {
        return false;
    };
    let riff_size = u32::from_le_bytes([*s0, *s1, *s2, *s3]) as usize;
    riff_size
        .checked_add(8)
        .is_some_and(|end| end <= bytes.len())
}
