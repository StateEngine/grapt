//! Reply usage comes only from the public assistant projection, never current
//! model selection or a local pricing calculation.
use crate::wire::{AssistantFooter, Spelled};

pub fn text(footer: &AssistantFooter) -> String {
    let model = format!(
        "{}/{}",
        footer.model.provider_id().get().spelled(),
        footer.model.id().get().spelled()
    );
    let input = count(footer.input);
    let output = count(footer.output);
    let cache = match (footer.input, footer.cache_read) {
        (Some(input), Some(read)) if input > 0 && read <= input => {
            format!("{:.1}%", read as f64 / input as f64 * 100.0)
        }
        _ => "—".into(),
    };
    let cost = footer
        .estimated_usd
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .map_or_else(
            || "—".into(),
            |cost| {
                if cost > 0.0 && cost < 0.000001 {
                    "<$0.000001".into()
                } else {
                    format!("${cost:.6}")
                }
            },
        );
    let warning = footer
        .usage_warning
        .map(|warning| {
            format!(
                "Warning: provider usage {} is {}; answer preserved · ",
                warning.field_word(),
                warning.issue.word()
            )
        })
        .unwrap_or_default();
    format!("{warning}{model} · input {input} · output {output} · cache {cache} · API est. {cost}")
}

fn count(value: Option<u64>) -> String {
    value
        .map(crate::format::count)
        .unwrap_or_else(|| "—".into())
}
