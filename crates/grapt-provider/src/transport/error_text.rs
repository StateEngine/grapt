//! Masked, bounded provider error text for the failure message, and the
//! opt-in local copy of it. The transport never writes a file.
use std::fmt;
use std::sync::Arc;

pub const ERROR_TEXT_BYTES: usize = 512;
/// The most of a provider's own message the failure row shows, counted in
/// characters so a multibyte message cannot occupy twice the screen.
pub const REFUSAL_DETAIL_CHARS: usize = 300;
const PATH_BYTES: usize = 2048;
const MASK: &str = "[redacted]";
const TRUNCATED: &str = crate::fragment::BoundedDetail::TRUNCATION_MARKER;

/// Deployment policy. Absence at the adapter preserves the library default.
#[derive(Debug, Clone)]
pub struct ProviderErrorText(Option<Arc<str>>);

/// One attempt's volatile 4xx observation. It has no serialization contract.
#[derive(Debug)]
pub struct ProviderErrorDetail {
    status: u16,
    enabled: bool,
    text: Option<MaskedErrorText>,
    terminal: Option<super::terminal_detail::MaskedTerminalFrame>,
}
impl ProviderErrorDetail {
    pub(crate) fn terminal(
        status: u16,
        frame: super::terminal_detail::MaskedTerminalFrame,
    ) -> Self {
        Self {
            status,
            enabled: true,
            text: None,
            terminal: Some(frame),
        }
    }
    pub fn terminal_frame(&self) -> Option<&super::terminal_detail::MaskedTerminalFrame> {
        self.terminal.as_ref()
    }
    pub fn status(&self) -> u16 {
        self.status
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn text(&self) -> Option<&MaskedErrorText> {
        self.text.as_ref()
    }
}

/// Already masked and bounded. Debug never renders provider-authored text.
pub struct MaskedErrorText(String);
impl fmt::Debug for MaskedErrorText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MaskedErrorText([local diagnostic])")
    }
}
impl MaskedErrorText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl ProviderErrorText {
    pub const fn disabled() -> Self {
        Self(None)
    }
    pub fn at(path: &str) -> Result<Self, &'static str> {
        if path.is_empty() || path.len() > PATH_BYTES || path.chars().any(char::is_control) {
            return Err(
                "provider error-text path is empty, too long or contains control characters",
            );
        }
        Ok(Self(Some(Arc::from(path))))
    }
    pub(crate) fn enabled(&self) -> bool {
        self.0.is_some()
    }
    /// The opt-in local copy of one client error's full body, and the volatile
    /// detail the observer writes it from.
    ///
    /// It produces no reader line: the failure row already states the status,
    /// the code and an excerpt, whether or not the opt-in is on. The file's own
    /// fate is reported where the file is written, so a per-attempt note about
    /// it would be a second, weaker copy of a fact the user did not ask for.
    pub(crate) fn report(&self, status: u16, text: Option<MaskedErrorText>) -> ProviderErrorDetail {
        ProviderErrorDetail {
            status,
            enabled: self.enabled(),
            // The failure row already states the masked excerpt; the observer
            // receives it only for the opt-in local copy of the full body.
            text: text.filter(|_| self.enabled()),
            terminal: None,
        }
    }
}

/// The one line a failed attempt shows the reader: the HTTP status, the
/// vendor's own machine code when it published one, and a bounded excerpt of
/// the vendor's message after masking.
///
/// The excerpt is taken from `text`, which the credential lease has already
/// masked for secrets, URL credentials, token shapes and home paths, so this
/// adds no new trust in provider bytes — only a display bound on top of an
/// existing one. Counted in characters, so a multibyte message cannot grow the
/// row to twice its bound. An empty or whitespace-only message adds nothing:
/// the status alone is a complete fact.
pub(crate) fn refusal_message(
    status: u16,
    code: Option<crate::VendorCode>,
    text: Option<&MaskedErrorText>,
) -> String {
    let mut message = format!("the provider returned HTTP {status}");
    if let Some(code) = code {
        message.push_str(" (");
        message.push_str(code.as_str());
        message.push(')');
    }
    if let Some(reason) = text.map(|text| text.as_str().trim()) {
        if !reason.is_empty() {
            message.push_str(": ");
            let mut characters = reason.chars();
            for character in characters.by_ref().take(REFUSAL_DETAIL_CHARS) {
                message.push(character);
            }
            if characters.next().is_some() {
                message.push('…');
            }
        }
    }
    message
}

/// Input is the classifier's hard-bounded prefix, not an independent body read.
/// Mask on **decoded** content and serialize only afterwards: masking
/// serialized bytes hides a value behind its JSON escape (an escaped home, a
/// query inside a message) because the finders read plain text. A JSON body is
/// therefore walked as values, each string masked, then serialized once; a
/// plain body is masked in place. Classification keeps its original bytes.
pub(crate) fn mask(bytes: &[u8], secret: &str) -> MaskedErrorText {
    let bytes = &bytes[..bytes.len().min(super::classify::MAX_ERROR_BYTES)];
    let prefix: String = secret
        .chars()
        .take(super::classify::MAX_ERROR_BYTES)
        .collect();
    let escaped = serde_json::to_string(&prefix).unwrap_or_default();
    let inner = escaped
        .get(1..escaped.len().saturating_sub(1))
        .unwrap_or_default();
    let mut text = match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(mut value) => {
            mask_json(&mut value, secret, inner);
            serde_json::to_string(&value)
                .unwrap_or_else(|_| "[provider error text could not be decoded]".to_owned())
        }
        // Malformed escaped content cannot be decoded safely.
        Err(_) if bytes.contains(&b'\\') => {
            return MaskedErrorText(
                "[provider error text withheld: malformed escaped content]".into(),
            )
        }
        Err(_) => {
            // Remove a truncated raw key before lossy UTF-8 decoding, so a cap
            // inside a multibyte key cannot turn the private prefix into
            // unmatched text.
            let cut = [secret, inner]
                .iter()
                .filter(|pattern| !pattern.is_empty())
                .filter_map(|pattern| {
                    (0..bytes.len()).find(|at| pattern.as_bytes().starts_with(&bytes[*at..]))
                })
                .min()
                .unwrap_or(bytes.len());
            let mut text = String::from_utf8_lossy(&bytes[..cut]).into_owned();
            if cut < bytes.len() {
                text.push_str(MASK);
            }
            mask_text(&mut text, secret, inner);
            text
        }
    };
    if text.len() > ERROR_TEXT_BYTES {
        let mut cut = ERROR_TEXT_BYTES - TRUNCATED.len();
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str(TRUNCATED);
    }
    MaskedErrorText(text)
}

/// Mask every string in a parsed body, keys included; the excerpt is a
/// diagnostic, not a contract. A key naming a credential redacts its whole
/// value: walked as values, the key is the only place that relation is readable.
fn mask_json(value: &mut serde_json::Value, secret: &str, inner: &str) {
    match value {
        serde_json::Value::String(text) => mask_text(text, secret, inner),
        serde_json::Value::Array(items) => {
            for item in items {
                mask_json(item, secret, inner);
            }
        }
        serde_json::Value::Object(entries) => {
            let mut rebuilt = serde_json::Map::with_capacity(entries.len());
            for (mut key, mut item) in std::mem::take(entries) {
                mask_text(&mut key, secret, inner);
                if super::secret_shapes::sensitive(&key) {
                    if !item.is_null() {
                        item = serde_json::Value::String(MASK.to_owned());
                    }
                } else {
                    mask_json(&mut item, secret, inner);
                }
                // Two masked keys can collide; keep both facts.
                if rebuilt.contains_key(&key) {
                    key.push_str(MASK);
                }
                rebuilt.insert(key, item);
            }
            *entries = rebuilt;
        }
        _ => (),
    }
}

/// Replace every known secret and every recognised shape in one decoded string.
fn mask_text(text: &mut String, secret: &str, inner: &str) {
    let mut ranges = Vec::new();
    mark_known(text, secret, &mut ranges);
    if !inner.is_empty() && inner != secret {
        mark_known(text, inner, &mut ranges);
    }
    super::secret_shapes::mark(text, &mut ranges);
    ranges.sort_unstable();
    let mut output = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end) in ranges {
        if end <= at {
            continue;
        }
        if start > at {
            output.push_str(&text[at..start]);
        }
        if start >= at {
            output.push_str(MASK);
        }
        at = end;
    }
    output.push_str(&text[at..]);
    *text = output;
}
fn mark_known(text: &str, secret: &str, ranges: &mut Vec<(usize, usize)>) {
    if secret.is_empty() {
        return;
    }
    ranges.extend(
        text.match_indices(secret)
            .map(|(at, _)| (at, at + secret.len())),
    );
    // A read cap can leave any proper prefix of the current key at the end.
    for (at, _) in text.char_indices() {
        if secret.starts_with(&text[at..]) {
            ranges.push((at, text.len()));
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A private value inside a JSON body must not reach the rendered row.
    #[test]
    fn review_json_windows_home_is_masked_before_rendering() {
        let body = serde_json::json!({"error": {
            "message": r"cannot read C:\Users\example\project\file"
        }})
        .to_string();
        let text = mask(body.as_bytes(), "unused");
        let row = refusal_message(400, None, Some(&text));
        assert!(!row.contains("example"), "{row}");
    }
    #[test]
    fn review_query_session_secret_is_masked_before_rendering() {
        let body = serde_json::json!({"error": {
            "message": "failed URL https://example.test/?session_id=private-session-marker"
        }})
        .to_string();
        let text = mask(body.as_bytes(), "unused");
        let row = refusal_message(400, None, Some(&text));
        assert!(!row.contains("private-session-marker"), "{row}");
    }
    #[test]
    fn secrets_are_masked_before_utf8_truncation_and_never_debugged() {
        for body in [
            "key arbitrary-key; Bearer another-token".into(),
            "Authorization: Bearer secret-token\nnext line".into(),
            "{\"x-api-key\":\"another-token\"}".into(),
            "prefix sk-other-private".into(),
            "read capped arbitrary-ke".into(),
            format!("{}arbitrary-key", "x".repeat(506)),
        ] {
            let held = mask(body.as_bytes(), "arbitrary-key");
            assert!(held.as_str().len() <= 512);
            assert!(!held.as_str().contains("arbitrary-k"));
            assert!(!held.as_str().contains("another-token"));
            assert!(!held.as_str().contains("secret-token"));
            assert!(!held.as_str().contains("sk-other-private"));
            assert!(!format!("{held:?}").contains("prefix"));
        }
        assert!(!mask(b"error unicode-k\xc3", "unicode-ké-secret")
            .as_str()
            .contains("unicode-k"));
        let long = mask("é".repeat(300).as_bytes(), "unused");
        assert!(long.as_str().len() <= 512 && long.as_str().ends_with(TRUNCATED));
        let escaped = serde_json::to_string("odd\"key").unwrap_or_else(|e| panic!("{e}"));
        assert!(!mask(escaped.as_bytes(), "odd\"key")
            .as_str()
            .contains("odd"));
    }
    #[test]
    fn json_escape_spellings_cannot_hide_credentials() {
        for body in [
            r#"{"error":{"message":"bad \u0061rbitrary-key"}}"#,
            r#"{"\u0061rbitrary-key":"invalid credential"}"#,
            r#"{"error":"Bearer \u0061nother-token"}"#,
        ] {
            let held = mask(body.as_bytes(), "arbitrary-key");
            assert!(held.as_str().contains(MASK));
            assert!(!held.as_str().contains("arbitrary-key"));
            assert!(!held.as_str().contains("another-token"));
            assert!(!held.as_str().contains(r"\u0061"));
        }
        for body in [
            r#"{"error":"bad \u0061rbitrary-ke"#,
            r#"{"error":"bad \u00"#,
        ] {
            let held = mask(body.as_bytes(), "arbitrary-key");
            assert_eq!(
                held.as_str(),
                "[provider error text withheld: malformed escaped content]"
            );
        }
    }
    #[test]
    fn the_opt_in_owns_the_file_and_never_the_row() -> Result<(), &'static str> {
        let policy = ProviderErrorText::at("/profile/logs/provider-error.txt")?;
        let detail = policy.report(400, Some(mask(b"first", "unused")));
        assert_eq!(detail.status(), 400);
        assert_eq!(detail.text().ok_or("missing text")?.as_str(), "first");
        assert!(!format!("{detail:?}").contains("first"));
        // Without the flag the row still shows the excerpt; the detail for the
        // file is absent, and nothing claims a file this run never wrote.
        let disabled = ProviderErrorText::disabled().report(400, Some(mask(b"first", "unused")));
        assert!(disabled.text().is_none());
        assert!(ProviderErrorText::at(&"x".repeat(2049)).is_err());
        Ok(())
    }
    #[test]
    fn a_rate_limit_row_states_status_code_and_reason() {
        // The same renderer the adapter uses for every failed status, asserted
        // at 429 because a rate limit retries and so cannot settle as a
        // failure row inside a headless test's bound.
        let text = mask(b"slow down and retry shortly", "unused");
        let code = crate::VendorCode::from_json(&serde_json::json!("rate_limit_error"))
            .unwrap_or_else(|| panic!("missing code"));
        assert_eq!(
            refusal_message(429, Some(code), Some(&text)),
            "the provider returned HTTP 429 (rate_limit_error): slow down and retry shortly"
        );
    }
    #[test]
    fn a_server_error_row_states_status_and_reason_without_a_code() {
        // 5xx bodies classify nothing, so a parsed code is not expected; the
        // status and the excerpt are, and a server error is a retry class too.
        let text = mask(b"the provider had an internal fault", "unused");
        assert_eq!(
            refusal_message(500, None, Some(&text)),
            "the provider returned HTTP 500: the provider had an internal fault"
        );
    }
    #[test]
    fn the_row_states_status_code_and_a_bounded_redacted_excerpt() {
        let text = mask(b"model refused this request", "unused");
        let line = refusal_message(400, None, Some(&text));
        assert_eq!(
            line,
            "the provider returned HTTP 400: model refused this request"
        );
        // A vendor machine code is named when the provider published one.
        let code = crate::VendorCode::from_json(&serde_json::json!("invalid_api_key"))
            .unwrap_or_else(|| panic!("missing code"));
        let line = refusal_message(401, Some(code), None);
        assert_eq!(line, "the provider returned HTTP 401 (invalid_api_key)");
        // An absent or blank message adds nothing: the status is the whole fact.
        assert_eq!(
            refusal_message(500, None, None),
            "the provider returned HTTP 500"
        );
        let blank = mask(b"   ", "unused");
        assert_eq!(
            refusal_message(500, None, Some(&blank)),
            "the provider returned HTTP 500"
        );
        // Exactly the bound is kept whole; one more character is marked, never
        // silently dropped.
        let at_bound = mask("x".repeat(REFUSAL_DETAIL_CHARS).as_bytes(), "unused");
        let line = refusal_message(429, None, Some(&at_bound));
        assert!(!line.contains('…') && line.ends_with("xxxx"));
        let over = mask("x".repeat(REFUSAL_DETAIL_CHARS + 1).as_bytes(), "unused");
        let line = refusal_message(429, None, Some(&over));
        assert!(line.contains('…'), "{line}");
        assert!(
            !line.contains(&"x".repeat(REFUSAL_DETAIL_CHARS + 1)),
            "{line}"
        );
    }
}
