//! Redact complete values and split prefixes before retaining stderr bytes.
use std::collections::VecDeque;

const TAIL_BYTES: usize = 4_096;
const SECRET_BYTES: usize = 65_536;
const SECRET_COUNT: usize = 256;
const PRIVATE: &[u8] = b"[private]";

#[derive(Default)]
pub(crate) struct Tail {
    patterns: Vec<Vec<u8>>,
    pattern_bytes: usize,
    pending: Vec<u8>,
    bytes: VecDeque<u8>,
    suppressed: bool,
    prefix: bool,
}

impl Tail {
    pub(crate) fn protect(&mut self, value: &str) {
        if value.is_empty() || self.suppressed {
            return;
        }
        self.pattern(value.as_bytes());
        if let Ok(escaped) = serde_json::to_string(value) {
            if let Some(inner) = escaped.get(1..escaped.len().saturating_sub(1)) {
                self.pattern(inner.as_bytes());
            }
        }
    }

    fn pattern(&mut self, value: &[u8]) {
        if self.suppressed || self.patterns.iter().any(|pattern| pattern == value) {
            return;
        }
        if self.patterns.len() >= SECRET_COUNT
            || value.len() > SECRET_BYTES.saturating_sub(self.pattern_bytes)
        {
            // An incomplete secret set cannot safely disclose any tail.
            self.suppressed = true;
            self.pending.clear();
            self.bytes.clear();
            self.patterns.clear();
            return;
        }
        // A newly learned private value may already be present, partially,
        // in an older tail. Discard that tail before changing the secret set.
        self.pending.clear();
        self.bytes.clear();
        self.pattern_bytes += value.len();
        self.patterns.push(value.to_vec());
    }

    /// Input chunks are capped by the reader, so pending is bounded by one
    /// chunk plus the longest admitted private value, even without newlines.
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        if self.suppressed {
            return;
        }
        for chunk in chunk.chunks(1_024) {
            self.pending.extend_from_slice(chunk);
            let mut at = 0;
            while at < self.pending.len() {
                let rest = &self.pending[at..];
                if self
                    .patterns
                    .iter()
                    .any(|pattern| pattern.len() > rest.len() && pattern.starts_with(rest))
                {
                    break;
                }
                if let Some(length) = self
                    .patterns
                    .iter()
                    .filter(|pattern| rest.starts_with(pattern))
                    .map(Vec::len)
                    .max()
                {
                    self.emit(PRIVATE);
                    at += length;
                } else {
                    let byte = self.pending[at];
                    self.emit(&[byte]);
                    at += 1;
                }
            }
            self.pending.drain(..at);
        }
    }

    pub(crate) fn finish(&mut self) {
        if !self.pending.is_empty() {
            // EOF or cancellation in the middle of a value must not publish a
            // suffix that a bounded-tail cut would later disguise as ordinary text.
            self.emit(PRIVATE);
            self.pending.clear();
        }
    }

    fn emit(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if self.bytes.len() == TAIL_BYTES {
                if self.prefix {
                    continue;
                }
                self.bytes.pop_front();
            }
            self.bytes.push_back(*byte);
        }
    }

    pub(crate) fn sanitise(&self, text: &str) -> String {
        self.sanitise_with_lines(text, false, false)
    }

    pub(crate) fn sanitise_lines(&self, text: &str) -> String {
        self.sanitise_with_lines(text, true, false)
    }

    pub(crate) fn sanitise_prefix(&self, text: &str) -> String {
        self.sanitise_with_lines(text, true, true)
    }

    fn sanitise_with_lines(&self, text: &str, lines: bool, prefix: bool) -> String {
        let mut copy = Self {
            patterns: self.patterns.clone(),
            pattern_bytes: self.pattern_bytes,
            suppressed: self.suppressed,
            prefix,
            ..Self::default()
        };
        copy.push(text.as_bytes());
        copy.finish();
        copy.render(lines)
    }

    pub(crate) fn text(&self) -> String {
        self.render(false)
    }

    pub(crate) fn text_lines(&self) -> String {
        self.render(true)
    }

    fn render(&self, lines: bool) -> String {
        if self.suppressed {
            return "stderr omitted: private-value redaction bound reached".into();
        }
        let bytes: Vec<u8> = self.bytes.iter().copied().collect();
        let text: String = String::from_utf8_lossy(&bytes)
            .chars()
            .map(|character| {
                if character.is_control() && !(lines && character == '\n') {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        if self.prefix {
            let mut end = text.len().min(TAIL_BYTES);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            return text[..end].trim().to_owned();
        }
        let mut start = text.len().saturating_sub(TAIL_BYTES);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text[start..].trim().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_precedes_chunks_and_tail_eviction_even_for_long_values() {
        let secret = format!("token-{}-end", "s".repeat(6_000));
        let mut tail = Tail::default();
        tail.protect(&secret);
        tail.push(&vec![b'x'; TAIL_BYTES - 3]);
        for chunk in secret.as_bytes().chunks(37) {
            tail.push(chunk);
        }
        tail.push(b" exit reason");
        let text = tail.text();
        assert!(text.ends_with("[private] exit reason"));
        assert!(!text.contains("sss"));
        assert!(text.len() <= TAIL_BYTES);
    }

    #[test]
    fn escaped_controls_partial_values_and_invalid_utf8_stay_bounded() {
        let secret = "cap\"private\\handle\n";
        let escaped = match serde_json::to_string(secret) {
            Ok(value) => value,
            Err(error) => panic!("secret fixture: {error}"),
        };
        let mut tail = Tail::default();
        tail.protect(secret);
        for byte in escaped.as_bytes() {
            tail.push(&[*byte]);
        }
        tail.push(b"\n\x1bexit\xff");
        tail.finish();
        let text = tail.text();
        assert!(text.contains("[private]"));
        assert!(!text.contains("handle"));
        assert!(!text.chars().any(char::is_control));
        let mut partial = Tail::default();
        partial.protect(secret);
        partial.push(b"cap\"");
        partial.finish();
        assert_eq!(partial.text(), "[private]");
        let mut invalid = Tail::default();
        invalid.push(&vec![0xff; 8_000]);
        assert!(invalid.text().len() <= TAIL_BYTES);
    }

    #[test]
    fn secret_capacity_refuses_tail_instead_of_disclosing_partial_coverage() {
        let mut tail = Tail::default();
        tail.push(b"prior detail");
        tail.protect(&"a".repeat(SECRET_BYTES + 1));
        tail.push(b"private suffix");
        assert_eq!(
            tail.text(),
            "stderr omitted: private-value redaction bound reached"
        );
    }
}
