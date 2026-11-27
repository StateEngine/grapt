//! One prompt preparation/admission path for foreground and background runs.
use super::{RunError, Runner};
use grapt_api::PromptResponseDto;
use grapt_types::{MessageId, SessionId};

impl Runner {
    /// Use a caller-owned key for initial admission into an exact existing Session.
    /// This does not enable replay after an uncertain transport outcome.
    pub fn with_input_id(mut self, input_id: Option<MessageId>) -> Self {
        self.input_id = input_id;
        self
    }

    /// Bound this prompt's logical provider calls at server Step admission.
    pub fn with_max_steps(mut self, maximum: Option<std::num::NonZeroU32>) -> Self {
        self.max_steps = maximum;
        self
    }

    pub(super) async fn admit_prompt(
        &self,
        session: &SessionId,
        prompt: String,
    ) -> Result<PromptResponseDto, RunError> {
        if self.resident && self.max_steps.is_some() {
            return Err(RunError::Execution(
                crate::CommandError::MaxSteps.to_string(),
            ));
        }
        if let Some(preset) = self.preset {
            self.client.select_preset(session, preset).await?;
        }
        if let Some(effort) = &self.reasoning_effort {
            self.client.select_reasoning_effort(session, effort).await?;
        }
        if let Some(title) = &self.title {
            self.client.rename_session(session, title).await?;
        }
        // Non-idempotent admission is never hidden behind transport retry.
        Ok(self
            .client
            .prompt_with_input_id(
                session,
                prompt,
                &self.images,
                self.max_steps,
                self.input_id.as_ref(),
            )
            .await?)
    }
}
