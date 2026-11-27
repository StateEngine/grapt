use std::fmt;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
#[error("submit key must be 1 to 128 bytes without control characters")]
pub struct InvalidSubmitKey;

/// A caller-chosen idempotency key, unique within one session. Resubmitting
/// the same key and event returns the first outcome instead of a new
/// transition.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SubmitKey(Arc<str>);

impl SubmitKey {
    pub fn new(key: &str) -> Result<Self, InvalidSubmitKey> {
        if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err(InvalidSubmitKey);
        }
        Ok(Self(key.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SubmitKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
