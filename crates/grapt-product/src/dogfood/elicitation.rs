// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: URL consent through existing durable global form ownership.
//! A live MCP wait owns a durable global form; reopening never owns the wait.
mod progress;
use grapt_api::DogfoodEventHub;
use grapt_form::{
    catalog::FormState, global::GlobalFormBinding, global_catalog::global_form, FormService,
};
use grapt_inbox::Inbox;
use grapt_store::{AggregateId, OsRandomIds, SystemClock};
use grapt_types::{
    ClockAuthority, EventId, FormId, FormSuspensionReason, IdAuthority, LocationRef,
    McpElicitationId,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(test)]
#[path = "elicitation_tests.rs"]
mod tests;

pub(super) fn recover(
    store: &mut grapt_store::EventStore,
    ids: &dyn IdAuthority,
    clock: &dyn ClockAuthority,
) -> Result<(), String> {
    super::tools::ask::recover(store, ids, clock)?;
    for record in grapt_form::global_catalog::pending_global_forms(store)
        .map_err(|error| error.to_string())?
    {
        store
            .elect_writer(&AggregateId::Form(record.id.clone()))
            .map_err(|error| error.to_string())?;
        if record.state == FormState::Pending {
            let event: EventId = format!("evt_{}", ids.mint().as_str())
                .parse()
                .map_err(|_| "recovery event identity")?;
            FormService::suspend_on(
                store,
                &event,
                &record.id,
                FormSuspensionReason::ProcessLoss,
                clock.now().as_millis(),
            )
            .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(super) struct FormProducer {
    pub(super) inbox: Arc<Mutex<Inbox>>,
    pub(super) ids: Arc<OsRandomIds>,
    pub(super) clock: Arc<SystemClock>,
    pub(super) hub: Arc<DogfoodEventHub>,
    pub(super) hooks: Option<(grapt_types::SessionId, Arc<super::hooks::HookRuntime>)>,
}

impl FormProducer {
    fn lock(&self) -> MutexGuard<'_, Inbox> {
        match self.inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn event(&self) -> Result<EventId, String> {
        format!("evt_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|_| "elicitation event identity".into())
    }

    pub(super) fn connect(&self, location: &str, server: &str) -> Result<LiveForms, String> {
        Ok(LiveForms {
            producer: self.clone(),
            location: LocationRef::new(location, None).map_err(|error| error.to_string())?,
            server: server.into(),
            // This token is local to one admitted call on one held connection.
            // It is never reconstructed from a server name or reused after process loss.
            connection: McpElicitationId::new(self.ids.mint().as_str())
                .ok_or("elicitation connection identity")?,
            pending: None,
            form_mode: true,
            url_mode: true,
            startup: false,
            progress: None,
        })
    }

    pub(super) fn connect_startup(
        &self,
        location: &str,
        server: &str,
    ) -> Result<LiveForms, String> {
        let mut forms = self.connect(location, server)?;
        forms.startup = true;
        Ok(forms)
    }
}

pub(super) struct LiveForms {
    producer: FormProducer,
    location: LocationRef,
    server: String,
    connection: McpElicitationId,
    pending: Option<FormId>,
    form_mode: bool,
    url_mode: bool,
    startup: bool,
    progress: Option<Arc<progress::LiveProgress>>,
}

impl LiveForms {
    pub(super) fn observe_progress(
        &mut self,
        session: &grapt_types::SessionId,
        message: &grapt_types::MessageId,
        call: &grapt_types::ToolCallId,
        source_call: &grapt_types::ToolCallId,
        tool: &str,
    ) {
        self.progress = Some(Arc::new(progress::LiveProgress::new(
            self.producer.clone(),
            self.connection.as_str().into(),
            session.clone(),
            message.clone(),
            call.clone(),
            source_call.clone(),
            tool.into(),
        )));
    }
}

impl super::mcp::channel::ServerRequestHandler for LiveForms {
    fn protocol(&mut self, revision: &str) {
        self.form_mode = matches!(revision, "2025-06-18" | "2025-11-25");
        self.url_mode = revision == "2025-11-25";
    }

    fn progress(&self) -> Option<Arc<dyn super::mcp::channel::ProgressObserver>> {
        self.progress
            .clone()
            .map(|progress| progress as Arc<dyn super::mcp::channel::ProgressObserver>)
    }

    fn cancelled(&mut self) -> Result<(), String> {
        self.suspend()
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn answer<'a>(
        &'a mut self,
        request: &'a grapt_mcp::incoming::ServerRequest,
        dispatch: Option<&'a grapt_form::global::CallDispatch>,
    ) -> grapt_runtime::BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            self.check_recipient()?;
            let url = if request.params()["mode"] == "url" {
                if !self.url_mode {
                    return Err("URL elicitation was not negotiated".into());
                }
                Some(grapt_mcp::elicitation::UrlElicitation::parse(
                    request.params(),
                )?)
            } else {
                if !self.form_mode {
                    return Err("form elicitation was not negotiated".into());
                }
                None
            };
            let fields = match &url {
                Some(url) => url.fields(),
                None => grapt_form::elicitation::fields(request.params())?,
            };
            let message = request.params()["message"]
                .as_str()
                .filter(|text| text.len() <= 4096)
                .ok_or("elicitation message absent or too long")?;
            if fields.as_array().is_some_and(Vec::is_empty) {
                return Ok(json!({"action":"accept","content":{}}));
            }
            let id: FormId = format!("frm_{}", self.producer.ids.mint().as_str())
                .parse()
                .map_err(|_| "elicitation form identity")?;
            let binding = GlobalFormBinding {
                elicitation: McpElicitationId::new(self.producer.ids.mint().as_str())
                    .ok_or("elicitation identity")?,
                connection: self.connection.clone(),
                location: self.location.clone(),
                // Observational, and ABSENT rather than invented when no call
                // frames this request. It records which dispatch raised the
                // form; it does not own it. Ownership stays
                // `FormOwner::Global(elicitation)` and the form's own sessionID
                // stays the literal "global" below.
                call: dispatch.cloned(),
            };
            let mut metadata = match &url {
                Some(url) => json!({"kind":"mcp-url-elicitation","server":self.server,
                    "message":url.message(),"url":url.url(),"host":url.host(),
                    // This is the SERVER's ID, never our local owner/correlation.
                    "elicitationId":url.id().as_str()}),
                None => json!({"kind":"mcp-elicitation","server":self.server,"message":message}),
            };
            if self.startup {
                metadata["startup"] = json!(true);
            }
            let info = json!({"id":id.as_str(),"sessionID":"global","title":format!("{} is requesting input",self.server),
                "metadata":metadata,"fields":fields});
            let event = self.producer.event()?;
            {
                let mut inbox = self.producer.lock();
                FormService::check_global_capacity(inbox.store())
                    .map_err(|error| error.to_string())?;
                inbox
                    .store_mut()
                    .elect_writer(&AggregateId::Form(id.clone()))
                    .map_err(|error| error.to_string())?;
                if let Err(error) = FormService::create_global_on(
                    inbox.store_mut(),
                    &event,
                    &id,
                    &binding,
                    &info,
                    self.producer.clock.now().as_millis(),
                ) {
                    inbox
                        .store_mut()
                        .release_held_writer(&AggregateId::Form(id.clone()));
                    return Err(error.to_string());
                }
                super::global_form_events::publish(&inbox, &self.producer.hub, &id, &event);
            }
            self.pending = Some(id.clone());
            if let Some((session, hooks)) = &self.producer.hooks {
                if let Err(reason) = hooks
                    .fire(super::hooks::Event::WaitingForInput, session, None)
                    .await
                {
                    // This global form already exists for an in-flight call;
                    // a notifier cannot answer it or replay that MCP effect.
                    crate::diagnostics::report(format!("session {session}: WaitingForInput hook failed after MCP form creation: {reason}"));
                }
            }
            loop {
                self.check_recipient()?;
                let record = global_form(self.producer.lock().store(), &id)
                    .map_err(|error| error.to_string())?
                    .ok_or("live elicitation disappeared")?;
                if record.binding != binding {
                    return Err("live elicitation binding changed".into());
                }
                match record.state {
                    FormState::Answered(answer) => {
                        self.pending = None;
                        return match &url {
                            Some(url) => url.response(&answer),
                            None => Ok(json!({"action":"accept","content":answer})),
                        };
                    }
                    FormState::Cancelled => {
                        self.pending = None;
                        return Ok(json!({"action":"cancel"}));
                    }
                    FormState::Suspended(_) => return Err("elicitation lost its live owner".into()),
                    FormState::Pending => {}
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
    }
}

impl Drop for LiveForms {
    fn drop(&mut self) {
        // Best effort during a dropped future only. Normal call exits use the
        // explicit fallible settlement path; process loss is recovered at startup.
        if let Err(error) = self.suspend() {
            crate::diagnostics::report(format!("global form suspension failed: {error}"));
        }
    }
}

impl LiveForms {
    fn check_recipient(&self) -> Result<(), String> {
        if self.startup
            && !self
                .producer
                .hub
                .has_global_form_recipient(&self.location)
                .map_err(|error| error.to_string())?
        {
            return Err("startup elicitation has no live location-wide UI recipient".into());
        }
        Ok(())
    }

    pub(super) fn suspend(&mut self) -> Result<(), String> {
        let Some(id) = self.pending.as_ref() else {
            return Ok(());
        };
        self.producer.event().and_then(|event| {
            let mut inbox = self.producer.lock();
            let applied = FormService::suspend_on(
                inbox.store_mut(),
                &event,
                id,
                FormSuspensionReason::Shutdown,
                self.producer.clock.now().as_millis(),
            )
            .map_err(|error| error.to_string())?;
            if applied == grapt_types::CasOutcome::Applied {
                super::global_form_events::publish(&inbox, &self.producer.hub, id, &event);
            }
            Ok(())
        })?;
        self.pending = None;
        Ok(())
    }
}
