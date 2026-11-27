//! Canonical per-input options, committed with the user's prompt.
use super::*;
use std::num::NonZeroU32;

#[derive(Debug, Clone, Copy, Default)]
pub struct UserInputOptions<'a> {
    pub images: &'a [ArtifactRef],
    pub origin: UserInputOrigin,
    pub max_steps: Option<NonZeroU32>,
}

impl UserInputOptions<'_> {
    pub(super) fn metadata(self) -> Result<Option<Value>, InboxError> {
        let mut metadata = images::metadata(self.images)?;
        if self.origin == UserInputOrigin::SeatStartup {
            metadata.get_or_insert_with(|| json!({}))["origin"] = json!("seat-startup");
        }
        if let Some(maximum) = self.max_steps {
            metadata.get_or_insert_with(|| json!({}))["maxSteps"] = json!(maximum.get());
        }
        Ok(metadata)
    }
}

/// Missing is uncapped; a present malformed bound is always a refusal.
pub fn max_steps(metadata: Option<&Value>) -> Result<Option<NonZeroU32>, &'static str> {
    let Some(value) = metadata.and_then(|value| value.get("maxSteps")) else {
        return Ok(None);
    };
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .and_then(NonZeroU32::new)
        .map(Some)
        .ok_or("maxSteps must be an integer from 1 to 4294967295")
}
