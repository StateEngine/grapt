//! Stable and bounded provider prompt-cache keys.

use grapt_types::SessionId;

/// Maximum UTF-8 bytes accepted by OpenAI's prompt-cache bucketing key.
pub const MAX_PROMPT_CACHE_KEY_BYTES: usize = 64;

/// A bounded, stable provider cache bucket.
///
/// This is not cache truth: providers still match exact prompt prefixes. It
/// only keeps one durable Session's related requests in the same provider
/// bucket. Oracle-native `ses_` plus 64-lowercase-hex identities shed their
/// prefix; every other imported identity must already fit. There is no
/// truncation that can collapse distinct Sessions; only the opt-in diagnostic
/// project scope ([`PromptCacheKey::for_project`]) shares one bucket on
/// purpose.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PromptCacheKey(String, Option<String>);

/// The hash domain of a project prompt-cache key, held at its pre-rename value
/// so existing provider cache buckets keep their hit rates.
const PROMPT_CACHE_DOMAIN: &str = "secode-prompt-cache-v1"; // grapt-rename: keep

impl PromptCacheKey {
    pub fn try_new(value: String) -> Result<Self, PromptCacheKeyError> {
        let bytes = value.len();
        if bytes == 0 || bytes > MAX_PROMPT_CACHE_KEY_BYTES {
            return Err(PromptCacheKeyError { bytes });
        }
        Ok(Self(value, None))
    }

    /// One bucket for every Session of a project on one model: the lowercase
    /// hex SHA-256 of the project and model identity. A diagnostic scope
    /// (`prompt_cache_scope` in service.conf), never the default.
    pub fn for_project(project: &str, provider: &str, model: &str) -> Self {
        use sha2::Digest;
        let mut digest = sha2::Sha256::new();
        for part in [PROMPT_CACHE_DOMAIN, project, provider, model] {
            digest.update(part.as_bytes());
            digest.update([0]);
        }
        let value = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Self(value, None)
    }

    /// The same body key, with the Codex `session-id` header taken from
    /// `session` instead of from the key itself.
    pub fn with_session_header(mut self, session: &Self) -> Self {
        self.1 = Some(session.0.clone());
        self
    }

    pub fn for_session(session: &SessionId) -> Result<Self, PromptCacheKeyError> {
        let raw = session.as_str();
        let value = match raw.strip_prefix("ses_") {
            Some(suffix)
                if suffix.len() == MAX_PROMPT_CACHE_KEY_BYTES
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
            {
                suffix.to_owned()
            }
            _ => raw.to_owned(),
        };
        Self::try_new(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// What the Codex route sends as `session-id`: the key itself unless a
    /// separate Session value was attached.
    pub fn session_header(&self) -> &str {
        self.1.as_deref().unwrap_or(&self.0)
    }
}

impl std::fmt::Debug for PromptCacheKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PromptCacheKey([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("prompt cache key must contain 1..={MAX_PROMPT_CACHE_KEY_BYTES} UTF-8 bytes, got {bytes}")]
pub struct PromptCacheKeyError {
    pub bytes: usize,
}
