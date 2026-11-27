// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Modified by Grapt:
// fixed RGB diff surfaces and an eight-step search, with no global color cache.
//! Keep syntax hues readable on the actual added/deleted line fill.
use ratatui::style::Color;

const MIN_CONTRAST: f64 = 4.5;

pub(super) fn foreground(preferred: Color, background: Color) -> Color {
    let (Color::Rgb(r, g, b), Color::Rgb(br, bg, bb)) = (preferred, background) else {
        return preferred;
    };
    let preferred = (r, g, b);
    let background = (br, bg, bb);
    if ratio(preferred, background) >= MIN_CONTRAST {
        return Color::Rgb(r, g, b);
    }
    let endpoint = if ratio((0, 0, 0), background) >= ratio((255, 255, 255), background) {
        0
    } else {
        255
    };
    let blend = |step: u16| {
        let mix = |channel: u8| ((u16::from(channel) * (255 - step) + endpoint * step) / 255) as u8;
        (mix(r), mix(g), mix(b))
    };
    let (mut low, mut high) = (0, 255);
    for _ in 0..8 {
        let middle = (low + high) / 2;
        if ratio(blend(middle), background) >= MIN_CONTRAST {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let (r, g, b) = blend(high);
    Color::Rgb(r, g, b)
}

fn ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn luminance(rgb: (u8, u8, u8)) -> f64 {
    let [r, g, b] = [rgb.0, rgb.1, rgb.2].map(|channel| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * r + 0.7152 * g + 0.0722 * b
}
