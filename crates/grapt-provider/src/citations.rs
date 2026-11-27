//! Responses URL annotations become visible, bounded Markdown links. The TUI
//! renders their labels with separate terminal hyperlink destinations.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("provider citation is malformed or exceeds its bound")]
pub struct CitationError;

#[derive(Default)]
pub struct CitationTracker {
    seen: HashSet<[u8; 32]>,
    rendered_bytes: usize,
    pending: String,
}
impl std::fmt::Debug for CitationTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CitationTracker")
            .field("citations", &self.seen.len())
            .finish_non_exhaustive()
    }
}
impl CitationTracker {
    pub fn observe(&mut self, kind: &str, value: &Value) -> Result<(), CitationError> {
        match kind {
            "response.output_text.annotation.added" => {
                let item = value["item_id"].as_str().ok_or(CitationError)?;
                let part = value["content_index"]
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or(CitationError)?;
                self.queue(item, part, &value["annotation"])?;
            }
            "response.content_part.done" => {
                if let Some(annotations) = value["part"].get("annotations") {
                    let item = value["item_id"].as_str().ok_or(CitationError)?;
                    let part = value["content_index"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or(CitationError)?;
                    self.annotations(item, part, annotations)?;
                }
            }
            "response.output_item.done" => self.message(&value["item"])?,
            "response.completed" | "response.incomplete" => {
                if let Some(output) = value["response"].get("output") {
                    let output = output
                        .as_array()
                        .filter(|v| v.len() <= 256)
                        .ok_or(CitationError)?;
                    for item in output {
                        self.message(item)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    pub fn take_pending(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending))
        }
    }
    fn message(&mut self, item: &Value) -> Result<(), CitationError> {
        if item["type"] != "message" {
            return Ok(());
        }
        let Some(parts) = item.get("content") else {
            return Ok(());
        };
        let parts = parts
            .as_array()
            .filter(|v| v.len() <= 64)
            .ok_or(CitationError)?;
        for (index, part) in parts.iter().enumerate() {
            if let Some(annotations) = part.get("annotations") {
                if part["type"] != "output_text" {
                    continue;
                }
                let item = item["id"].as_str().ok_or(CitationError)?;
                self.annotations(item, index, annotations)?;
            }
        }
        Ok(())
    }
    fn annotations(&mut self, item: &str, part: usize, value: &Value) -> Result<(), CitationError> {
        let annotations = value
            .as_array()
            .filter(|v| v.len() <= 128)
            .ok_or(CitationError)?;
        for annotation in annotations {
            self.queue(item, part, annotation)?;
        }
        Ok(())
    }
    fn queue(&mut self, item: &str, part: usize, annotation: &Value) -> Result<(), CitationError> {
        if let Some(text) = self.append(item, part, annotation)? {
            self.pending.push_str(&text);
        }
        Ok(())
    }
    pub fn append(
        &mut self,
        item: &str,
        part: usize,
        annotation: &Value,
    ) -> Result<Option<String>, CitationError> {
        if annotation["type"] != "url_citation" {
            return Ok(None);
        }
        if item.is_empty() || item.len() > 256 || item.chars().any(char::is_control) || part >= 64 {
            return Err(CitationError);
        }
        let raw = annotation["url"]
            .as_str()
            .filter(|url| {
                !url.is_empty()
                    && url.len() <= 8192
                    && url.trim() == *url
                    && !url.chars().any(char::is_control)
            })
            .ok_or(CitationError)?;
        let url = reqwest::Url::parse(raw).map_err(|_| CitationError)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(CitationError);
        }
        let title = annotation["title"]
            .as_str()
            .filter(|title| title.len() <= 1024 && !title.chars().any(char::is_control))
            .ok_or(CitationError)?;
        let start = annotation["start_index"].as_u64().ok_or(CitationError)?;
        let end = annotation["end_index"]
            .as_u64()
            .filter(|end| *end >= start && *end <= 16 * 1024 * 1024)
            .ok_or(CitationError)?;
        let mut hash = Sha256::new();
        for field in [
            item.as_bytes(),
            &part.to_be_bytes(),
            raw.as_bytes(),
            title.as_bytes(),
            &start.to_be_bytes(),
            &end.to_be_bytes(),
        ] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field);
        }
        let key = hash.finalize().into();
        if self.seen.contains(&key) {
            return Ok(None);
        }
        if self.seen.len() == 128 {
            return Err(CitationError);
        }
        // Keep titles readable without allowing their punctuation to change a
        // Markdown link's destination, image/code semantics, or label bounds.
        let label = title
            .replace(['[', ']', '\\', '`', '<', '>'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let label = if label.is_empty() {
            url.host_str().unwrap_or("source")
        } else {
            &label
        };
        let rendered = format!(" [Source {}: {}](<{}>)", self.seen.len() + 1, label, url);
        let bytes = self
            .rendered_bytes
            .checked_add(rendered.len())
            .filter(|bytes| *bytes <= 128 * 1024)
            .ok_or(CitationError)?;
        self.seen.insert(key);
        self.rendered_bytes = bytes;
        Ok(Some(rendered))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn annotations_are_visible_links_and_aggregate_repeats_do_not_duplicate_them(
    ) -> Result<(), CitationError> {
        let mut citations = CitationTracker::default();
        let value = json!({"type":"url_citation","start_index":2,"end_index":8,"title":"Source [title] `code`","url":"https://example.com/Case?q=Private#section"});
        let link = citations
            .append("message", 0, &value)?
            .ok_or(CitationError)?;
        assert!(link.contains(
            "[Source 1: Source title code](<https://example.com/Case?q=Private#section>)"
        ));
        assert!(citations.append("message", 0, &value)?.is_none());
        assert!(citations.append("another-message", 0, &value)?.is_some());
        assert!(!format!("{citations:?}").contains("Private"));
        Ok(())
    }
    #[test]
    fn unsafe_or_unbounded_citations_cannot_create_links() {
        let valid = json!({"type":"url_citation","start_index":0,"end_index":1,"title":"title","url":"https://example.com"});
        for (field, value) in [
            ("url", json!("javascript:bad")),
            ("url", json!("https://user:secret@example.com")),
            ("url", json!("https://example.com/\x1b]8;;bad")),
            ("title", json!("x".repeat(1025))),
            ("end_index", json!(u64::MAX)),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(CitationTracker::default()
                .append("message", 0, &invalid)
                .is_err());
        }
        let mut citations = CitationTracker::default();
        for index in 0..128 {
            assert!(citations
                .append(&format!("message-{index}"), 0, &valid)
                .is_ok());
        }
        assert!(citations.append("one-too-many", 0, &valid).is_err());
    }
}
