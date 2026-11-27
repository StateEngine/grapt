//! Durable human-input waits notify through one Session-scoped observer.
use super::{Event, HookRuntime, LifecycleHookScope, SessionId, ToolCallId};

impl HookRuntime {
    pub(in crate::dogfood) async fn waiting_for_input(
        &self,
        session: &SessionId,
        question: Option<(&str, &ToolCallId)>,
    ) -> Result<(), String> {
        let mut runtime = self.clone();
        if let Some(context) = &mut runtime.notices {
            context.scope = LifecycleHookScope::Session;
        }
        let events = [
            Event::WaitingForInput,
            Event::PermissionRequest,
            Event::Notification,
        ];
        let mut failure = None;
        for event in events
            .into_iter()
            .take(if question.is_some() { 3 } else { 1 })
        {
            // These observers cannot settle a form, mint a ticket or grant an effect.
            // One failed notifier must not prevent the remaining observers from running.
            if let Err(reason) = runtime.fire(event, session, question).await {
                failure.get_or_insert(reason);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
