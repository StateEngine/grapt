//! IBM project identity is distinct from the application's local projects.

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WatsonxProjectId(String);

impl WatsonxProjectId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)))
        .then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Native chat version pinned to IBM's provider contract.
pub const WATSONX_API_VERSION: &str = "2026-04-20";
