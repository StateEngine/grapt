// Copyright 2025 OpenAI. Licensed under the Apache License, Version 2.0.
// . Grapt derives this preview
// from the server's bounded pending-input read, without a second input queue.
use crate::wire::QueuedInputPreview;

/// A small, removable preview beside the composer, separate from history.
pub fn lines(preview: &QueuedInputPreview, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    if let Some(reason) = &preview.unavailable {
        rows.push(super::truncate(
            &format!("Pending input unavailable: {}", super::visible(reason)),
            width,
        ));
    }
    for (heading, total, messages) in [
        (
            "Steering",
            preview.steering_total,
            &preview.steering_messages,
        ),
        ("Queued", preview.total, &preview.messages),
    ] {
        if total == 0 {
            continue;
        }
        rows.push(super::truncate(&format!("{heading} ({total})"), width));
        rows.extend(messages.iter().take(2).map(|message| {
            let text = super::visible(&message.replace(['\n', '\r'], " "));
            super::truncate(&format!("  {text}"), width)
        }));
    }
    rows
}
