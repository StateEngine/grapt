//! Copyright 2025 OpenAI.
//! Apache-2.0. Modified by Grapt: plain bounded rows, public catalog account
//! labels, and existing provider-reported history/cache projections.
use crate::client::{Client, Connection};
use crate::format::count as compact;
use crate::wire::Spelled;

/// Bounded presentation fields; the protocol client owns all accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    fields: Vec<(&'static str, String)>,
    latest: String,
    history: String,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            fields: vec![
                ("Usage", "not reported".into()),
                ("Cost", "not reported".into()),
            ],
            latest: "-".into(),
            history: "-".into(),
        }
    }
}

/// Capture existing selected-session projections without fetching or recounting.
pub fn snapshot<C: Connection>(client: &Client<C>) -> Snapshot {
    let model = client.model();
    let view = client.view();
    let mut fields = vec![
        (
            "Model",
            model
                .map(|model| format!("{}/{}", model.provider_id.spelled(), model.id.spelled()))
                .unwrap_or_else(|| "unavailable".to_owned()),
        ),
        (
            "Effort",
            model
                .map(|model| {
                    model
                        .variant
                        .as_ref()
                        .map(|value| value.spelled())
                        .unwrap_or("default")
                })
                .unwrap_or("unavailable")
                .to_owned(),
        ),
        (
            "Account",
            match client.status_account() {
                Ok(Some(label)) => label.to_owned(),
                Ok(None) => "not reported".to_owned(),
                Err(error) => format!("unavailable: {error}"),
            },
        ),
        (
            "Context",
            context(
                view.and_then(|view| view.cache.latest.input),
                model.and_then(|model| model.context_window),
            ),
        ),
        (
            "Tools",
            model.map(tools_status).unwrap_or("unavailable").to_owned(),
        ),
        (
            "Tokens",
            view.map(|view| {
                format!(
                    "input {} · output {} (last reported)",
                    count(view.usage.input),
                    count(view.usage.output)
                )
            })
            .unwrap_or_else(|| "not reported".to_owned()),
        ),
        (
            "Cache",
            view.map(|view| view.cache.status())
                .unwrap_or_else(|| "not reported".to_owned()),
        ),
    ];
    fields.push((
        "Calls",
        view.and_then(|view| view.cache.calls_status())
            .unwrap_or_else(|| "not reported".into()),
    ));
    fields.push((
        "Cost",
        view.map(|view| view.cache.cost_status())
            .unwrap_or_else(|| "unavailable: usage unreported".into()),
    ));
    fields.push((
        "Scope",
        "tokens/cache: latest report; history: session assistant messages; calls: observed capture"
            .into(),
    ));
    if let Some(miss) = view.and_then(|view| view.cache.miss_status()) {
        fields.push(("Miss", miss));
    }
    fields.push((
        "Session",
        client
            .selected()
            .map(|id| id.spelled())
            .unwrap_or("none")
            .to_owned(),
    ));
    let percent = |share: Option<f64>| {
        share
            .map(|value| format!("{:.1}%", value * 100.0))
            .unwrap_or_else(|| "-".into())
    };
    Snapshot {
        fields,
        latest: percent(view.and_then(|view| view.cache.latest_share())),
        history: percent(view.and_then(|view| view.cache.history_share())),
    }
}

impl Snapshot {
    /// Always-visible cache share; details explain missing coverage and cost.
    pub fn compact(&self, width: usize) -> String {
        crate::cells::cut(
            &format!("Cache last {} · session {} · F4", self.latest, self.history),
            width,
        )
    }

    pub fn lines(&self, width: usize) -> Vec<String> {
        let label_width = self
            .fields
            .iter()
            .map(|(label, _)| crate::cells::cells(label))
            .max()
            .unwrap_or(0);
        let mut lines = vec![crate::cells::cut("STATUS", width.max(1))];
        for (label, value) in &self.fields {
            let padding = " ".repeat(label_width.saturating_sub(crate::cells::cells(label)));
            let mut safe = String::new();
            for ch in value.chars() {
                if ch.is_control() || matches!(ch,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}') {
                    safe.extend(ch.escape_default());
                } else {
                    safe.push(ch);
                }
            }
            lines.extend(crate::cells::wrap(
                &format!("  {label}:{padding} {safe}"),
                width.max(1),
            ));
        }
        lines
    }
}

fn context(input: Option<u64>, window: Option<u64>) -> String {
    match (input, window) {
        (Some(input), Some(window)) => format!(
            "{} / {} tokens · {:.1}% used (last request)",
            compact(input),
            compact(window),
            input as f64 / window as f64 * 100.0
        ),
        (None, Some(window)) => format!("{} tokens capacity · usage not reported", compact(window)),
        (Some(input), None) => format!("{} input tokens · capacity not reported", compact(input)),
        (None, None) => "usage and capacity not reported".to_owned(),
    }
}

fn count(value: Option<u64>) -> String {
    value
        .map(compact)
        .unwrap_or_else(|| "not reported".to_owned())
}

/// The reported tool surface; an unreported capability is unknown, not a denial.
fn tools_status(model: &crate::wire::ModelRow) -> &'static str {
    match model.capabilities.tools {
        Some(true) => "supported",
        Some(false) => "not supported",
        None => "—",
    }
}

#[cfg(test)]
mod tests;
