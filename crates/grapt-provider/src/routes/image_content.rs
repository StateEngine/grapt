//! Protocol projections for bounded, prepared image input.
use crate::InputImages;
use serde_json::{json, Value};

pub(super) fn responses(text: &str, images: &InputImages) -> Vec<Value> {
    std::iter::once(json!({"type":"input_text","text":text}))
        .chain(images.items().iter().map(|image| {
            json!({
                "type":"input_image", "image_url":image.data_url(),
                "detail":image.detail().as_str(),
            })
        }))
        .collect()
}

pub(super) fn anthropic(text: &str, images: &InputImages) -> Vec<Value> {
    std::iter::once(json!({"type":"text","text":text}))
        .chain(images.items().iter().map(|image| {
            json!({
                "type":"image", "source":{"type":"base64",
                    "media_type":image.media().mime(), "data":image.encoded()},
            })
        }))
        .collect()
}

pub(super) fn compatible(text: &str, images: &InputImages) -> Vec<Value> {
    std::iter::once(json!({"type":"text","text":text}))
        .chain(images.items().iter().map(|image| {
            json!({
                "type":"image_url", "image_url":{"url":image.data_url(),"detail":"high"},
            })
        }))
        .collect()
}
