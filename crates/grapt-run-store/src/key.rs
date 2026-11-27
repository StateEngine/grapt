use std::fmt;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
#[error("invalid request key")]
pub struct InvalidRequestKey;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RequestKey(Arc<str>);

impl RequestKey {
    pub fn new(key: &str) -> Result<Self, InvalidRequestKey> {
        if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err(InvalidRequestKey);
        }
        Ok(Self(key.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
