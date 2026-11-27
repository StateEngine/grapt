//! A command diagnostic keeps its process class through orchestration.
use crate::{ApiClientError, ExitClass};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CommandFailure {
    class: ExitClass,
    message: String,
}

impl CommandFailure {
    pub(crate) fn new(class: ExitClass, message: String) -> Self {
        Self { class, message }
    }

    pub const fn exit_class(&self) -> ExitClass {
        self.class
    }

    pub(crate) fn context(self, context: &str) -> Self {
        Self::new(self.class, format!("{context}: {}", self.message))
    }
}

impl From<ApiClientError> for CommandFailure {
    fn from(error: ApiClientError) -> Self {
        Self::new(error.exit_class(), error.to_string())
    }
}

impl From<String> for CommandFailure {
    fn from(message: String) -> Self {
        Self::new(ExitClass::Usage, message)
    }
}

impl From<&str> for CommandFailure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
