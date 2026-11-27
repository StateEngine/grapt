//! Bounded provider refusals and masked usage diagnostics, written off the
//! execution path. Only the usage object is retained from malformed accounting;
//! prompt and response content never enter this file. The last-error copy keeps
//! the existing provider_error_text opt-in.

use std::path::Path;

use super::stream_diagnostics::{FileSink, Recorder};

/// The file, relative to the service root.
pub(crate) const LOG: &str = "diagnostics/provider-errors.jsonl";

/// Every line's `schema`.
pub(crate) const SCHEMA: &str = "grapt.provider-refusal.v1";

/// Lines waiting for the writer before new ones are dropped.
const QUEUE: usize = 64;

/// Records provider refusals under one service root.
pub(crate) struct ProviderErrors(Option<Recorder>);

impl std::fmt::Debug for ProviderErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderErrors").finish_non_exhaustive()
    }
}

impl ProviderErrors {
    /// Volatile execution never opens a diagnostic writer or its queue.
    pub(crate) fn disabled() -> Self {
        Self(None)
    }

    /// Without a writer thread nothing is recorded, and nothing else changes.
    pub(crate) fn open(service_root: &Path) -> Self {
        let mut sink = FileSink::new(service_root.join(LOG), SCHEMA);
        let root = service_root.to_owned();
        let save_usage =
            crate::input::read_provider_error_text(&root.join("service.conf")).unwrap_or(false);
        Self(Recorder::spawn(SCHEMA, QUEUE, move |line| {
            sink.write(line);
            if save_usage {
                save_usage_record(&root, line);
            }
        }))
    }

    pub(crate) fn record_usage(&self, sample: &grapt_provider::DispatchSample) {
        let (Some(recorder), Some(warning), Some(detail)) =
            (&self.0, sample.usage_warning, &sample.usage_detail)
        else {
            return;
        };
        let Ok(usage) = serde_json::from_str::<serde_json::Value>(detail.as_str()) else {
            return;
        };
        recorder.offer(serde_json::json!({
            "attempt":sample.attempt.as_str(),
            "usage_warning":{"field":warning.field_word(), "issue":warning.issue.word()},
            "usage_detail":usage,
        }));
    }

    pub(crate) fn record(&self, refusal: &grapt_provider::DispatchRefusal) {
        if let Some(recorder) = &self.0 {
            recorder.offer(line(refusal));
        }
    }
}

fn line(refusal: &grapt_provider::DispatchRefusal) -> serde_json::Value {
    serde_json::json!({
        "attempt": refusal.attempt.as_str(),
        "status": refusal.status,
        "error_type": refusal.error_type.as_ref().map(grapt_provider::VendorCode::as_str),
        "error_code": refusal.error_code.as_ref().map(grapt_provider::VendorCode::as_str),
    })
}

fn save_usage_record(root: &Path, line: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    if let Some(usage) = value.get("usage_detail") {
        let _saved = crate::provider_error_text::save_usage(root, &usage.to_string());
    }
}
