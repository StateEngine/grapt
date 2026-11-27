//! Explicit startup choices, shared by headless and interactive CLI entry points.
//! The server validates catalog membership before creating the Session.
//!

use grapt_types::{AgentId, ModelId, ProviderId};

use crate::CommandError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedModel {
    pub provider: ProviderId,
    pub model: ModelId,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupSelection {
    pub agent: Option<AgentId>,
    pub model: Option<RequestedModel>,
}

impl StartupSelection {
    pub fn is_empty(&self) -> bool {
        self.agent.is_none() && self.model.is_none()
    }

    pub(crate) fn set_agent(&mut self, value: &str) -> Result<(), CommandError> {
        if self.agent.is_some() {
            return Err(CommandError::Duplicate("--agent"));
        }
        validate(value)?;
        self.agent =
            Some(AgentId::new(value).ok_or(CommandError::Selection("invalid --agent identity"))?);
        Ok(())
    }

    pub(crate) fn set_model(&mut self, value: &str) -> Result<(), CommandError> {
        if self.model.is_some() {
            return Err(CommandError::Duplicate("--model"));
        }
        validate(value)?;
        let (provider, model) = value
            .split_once('/')
            .ok_or(CommandError::Selection("--model requires provider/model"))?;
        self.model = Some(RequestedModel {
            provider: ProviderId::new(provider)
                .ok_or(CommandError::Selection("invalid --model provider"))?,
            model: ModelId::new(model)
                .ok_or(CommandError::Selection("invalid --model identity"))?,
        });
        Ok(())
    }
}

fn validate(value: &str) -> Result<(), CommandError> {
    if value.is_empty()
        || value.len() > 4096
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(CommandError::Selection("--model and --agent require nonempty identities of at most 4096 bytes without whitespace or control characters"));
    }
    Ok(())
}
