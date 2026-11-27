//! Async lifecycle preparation sits beside plugin context preparation, before
//! synchronous request planning. It is not repeated on transport retries.
use super::{Event, HookRuntime};
use grapt_types::SessionId;
use std::sync::Arc;

pub(in crate::dogfood) struct LifecyclePlan {
    runtime: Arc<HookRuntime>,
    session: SessionId,
    attached: bool,
    pending_prompt: bool,
    failure: Option<String>,
    notes: Vec<String>,
    prompt_notes: Vec<String>,
}
impl LifecyclePlan {
    pub(in crate::dogfood) fn new(runtime: Arc<HookRuntime>, session: SessionId) -> Self {
        Self {
            runtime,
            session,
            attached: false,
            pending_prompt: true,
            failure: None,
            notes: Vec::new(),
            prompt_notes: Vec::new(),
        }
    }
    pub(in crate::dogfood) fn promoted(&mut self) {
        self.pending_prompt = true;
    }
    pub(in crate::dogfood) async fn prepare(&mut self) {
        if self.failure.is_some() {
            return;
        }
        if !self.attached {
            self.attached = true;
            self.failure = Some(
                "SessionStart hook preparation did not complete before cancellation or deadline"
                    .into(),
            );
            match self
                .runtime
                .fire(Event::SessionStart, &self.session, None)
                .await
            {
                Ok(notes) => {
                    self.failure = None;
                    self.notes = notes;
                }
                Err(reason) => {
                    self.failure = Some(reason);
                    return;
                }
            }
        }
        if self.pending_prompt {
            self.pending_prompt = false;
            self.failure =
                Some("UserPromptSubmit hook preparation did not complete before cancellation or deadline".into());
            match self
                .runtime
                .fire(Event::UserPromptSubmit, &self.session, None)
                .await
            {
                Ok(notes) => {
                    self.failure = None;
                    if self
                        .notes
                        .iter()
                        .chain(&notes)
                        .map(String::len)
                        .sum::<usize>()
                        > 8192
                    {
                        self.failure = Some("lifecycle context exceeds 8192 bytes".into());
                    } else {
                        self.prompt_notes = notes;
                    }
                }
                Err(reason) => self.failure = Some(reason),
            }
        }
    }
    pub(in crate::dogfood) fn apply(
        &self,
        input: &mut Vec<grapt_provider::CodexInputItem>,
    ) -> Result<(), String> {
        self.check()?;
        if !self.notes.is_empty() || !self.prompt_notes.is_empty() {
            input.push(grapt_provider::CodexInputItem::Message(
                grapt_provider::InputMessage {
                    role: grapt_provider::MessageRole::User,
                    content: format!(
                        "Lifecycle hook context (not permission grants): {}",
                        serde_json::json!(self
                            .notes
                            .iter()
                            .chain(&self.prompt_notes)
                            .collect::<Vec<_>>())
                    ),
                },
            ));
        }
        Ok(())
    }
    pub(in crate::dogfood) fn check(&self) -> Result<(), String> {
        match &self.failure {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }
    pub(in crate::dogfood) async fn stop(&self) -> Result<(), String> {
        if !self.attached {
            return Ok(());
        }
        let stop = self.runtime.fire(Event::Stop, &self.session, None).await;
        // Always notify, even when Stop failed. No hook may restart a turn or
        // cause a tool to be retried after it already ran.
        let notification = self
            .runtime
            .fire(Event::Notification, &self.session, None)
            .await;
        match (stop, notification) {
            (Err(reason), _) | (_, Err(reason)) => Err(reason),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
