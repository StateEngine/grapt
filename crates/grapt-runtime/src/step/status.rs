//! Payload-free live observations of driver-owned preparation and backoff.
use std::time::Duration;

/// The Step driver's actual, non-durable backoff decision. It grants no retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryNotice {
    /// Physical ordinal, or timeout count when retrying pre-admission context.
    pub attempt: u32,
    pub after: Duration,
    pub class: Option<grapt_types::ProviderFailureClass>,
    /// Present only for pre-admission context retries, never provider retries.
    pub preparation_stage: Option<ContextPreparationStage>,
    /// Elapsed local-capacity wait; absent for provider and context retries.
    pub capacity_wait: Option<Duration>,
}

/// Payload-free diagnosis of the context operation currently being prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPreparationStage {
    Context,
    Lifecycle,
    Continuity,
    NativeMail,
    Request,
}

impl ContextPreparationStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Lifecycle => "lifecycle",
            Self::Continuity => "continuity",
            Self::NativeMail => "native-mail",
            Self::Request => "request",
        }
    }
}
