//! One bounded human-input wait, shared by permissions and question forms.
use crate::dogfood::{
    human_input::{Kind, Waits},
    session_status::RetryFeedback,
};
use std::sync::Arc;

pub(super) struct DecisionWait {
    waiting: Arc<Waits>,
    kind: Kind,
    feedback: Option<RetryFeedback>,
}

impl DecisionWait {
    pub(super) fn begin(waiting: &Arc<Waits>, kind: Kind) -> Result<Self, String> {
        waiting.begin(kind)?;
        Ok(Self {
            waiting: Arc::clone(waiting),
            kind,
            feedback: None,
        })
    }

    pub(super) fn with_feedback(mut self, feedback: Option<RetryFeedback>) -> Self {
        self.feedback = feedback;
        if let Some(feedback) = &self.feedback {
            feedback.input_changed();
        }
        self
    }
}

impl Drop for DecisionWait {
    fn drop(&mut self) {
        if let Err(error) = self.waiting.end(self.kind) {
            crate::server_log::warn(format_args!("human input wait release failed: {error}"));
        }
        if let Some(feedback) = &self.feedback {
            feedback.input_changed();
        }
    }
}
