//! Recovery advisory; reconstructed historically without a synthetic human input.
//!
//! Copyright 2023-2026 SpaceXAI. Apache-2.0. Modified by Grapt: append only
//! for the existing Step's next request; retain consumed cues in request history.

use grapt_provider::{CodexInputItem, InputMessage, MessageRole};

/// Once a text answer is retained, later Steps need not contain a tool
/// exchange. A newer human input clears the cue without erasing that fact.
#[derive(Default)]
pub(super) enum State {
    #[default]
    Unstarted,
    Cued,
    Retained,
}

impl State {
    pub(super) fn set(&mut self, enabled: bool) {
        if enabled {
            *self = Self::Cued;
        } else if matches!(self, Self::Cued) {
            *self = Self::Retained;
        }
    }

    pub(super) fn started(&self) -> bool {
        !matches!(self, Self::Unstarted)
    }

    pub(super) fn cued(&self) -> bool {
        matches!(self, Self::Cued)
    }
}

pub(super) fn append(input: &mut Vec<CodexInputItem>) {
    input.push(item());
}

pub(in crate::dogfood) fn item() -> CodexInputItem {
    CodexInputItem::Message(InputMessage {
        role: MessageRole::User,
        content: "[Output limit recovery] Your previous response reached the output token \
            limit and was cut off. Continue from exactly where it stopped. If a newer \
            user message follows this note, answer that instead. Do not repeat completed \
            tool effects."
            .to_owned(),
    })
}
