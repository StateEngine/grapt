//! Question and answer shapes for user-input requests.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Modified by Grapt: bounded multi-select questions, session forms and live approvers.
use super::*;
use grapt_form::{
    catalog::{form_by_id, FormState},
    service::FormPresentation,
    FormService,
};
use grapt_types::FormId;
use serde_json::{json, Value};
use std::collections::BTreeSet;

use crate::dogfood::elicitation::Unsettled;

pub(super) const TOOL: &str = "request_user_input";
pub(super) const DESCRIPTION: &str = "Ask a person 1–4 questions and wait for explicit answers. Each needs 2–4 labelled options; free-text Other is always available. multi_select permits several answers. Returns answered, declined, or no_user_available. In headless/run sessions no user is available: proceed with a reasonable assumption or explain what is blocked; do not repeat the question tool.";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Arguments {
    questions: Vec<Question>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Question {
    id: String,
    header: String,
    question: String,
    options: Vec<Choice>,
    #[serde(default)]
    multi_select: bool,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    label: String,
    description: String,
}

impl ToolArguments for Arguments {
    const TOOL_NAME: &'static str = TOOL;
    fn provider_schema() -> String {
        json!({"type":"object","additionalProperties":false,"required":["questions"],"properties":{
            "questions":{"type":"array","minItems":1,"maxItems":4,"items":{
                "type":"object","additionalProperties":false,"required":["id","header","question","options"],"properties":{
                    "id":{"type":"string","minLength":1,"maxLength":64},
                    "header":{"type":"string","minLength":1,"maxLength":80},
                    "question":{"type":"string","minLength":1,"maxLength":2048},
                    "multi_select":{"type":"boolean"},
                    "options":{"type":"array","minItems":2,"maxItems":4,"items":{
                        "type":"object","additionalProperties":false,"required":["label","description"],"properties":{
                            "label":{"type":"string","minLength":1,"maxLength":128},
                            "description":{"type":"string","maxLength":512}
                        }
                    }}
                }
            }}
        }}).to_string()
    }
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: Arguments = decode_input(input)?;
    let bounded = |text: &str, limit| !text.trim().is_empty() && text.len() <= limit;
    let mut ids = BTreeSet::new();
    if !(1..=4).contains(&args.questions.len())
        || args.questions.iter().any(|q| {
            let mut labels = BTreeSet::new();
            !bounded(&q.id, 64)
                || !ids.insert(q.id.as_str())
                || !bounded(&q.header, 80)
                || !bounded(&q.question, 2048)
                || !(2..=4).contains(&q.options.len())
                || q.options.iter().any(|o| {
                    !bounded(&o.label, 128)
                        || o.description.len() > 512
                        || !labels.insert(o.label.as_str())
                })
        })
    {
        return Err("provide 1–4 questions with unique ids, headers and text, each with 2–4 distinct labelled options within the published bounds".into());
    }
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        target: EffectTarget::Ask(args),
        capability: CapabilityId::derive(&resolved),
        action: TOOL.into(),
        resource: "user questions".into(),
    })
}

impl Arguments {
    fn fields(&self) -> Value {
        Value::Array(self.questions.iter().map(|q| {
            let mut field = json!({
                "key":q.id,"title":q.header,"description":q.question,
                "type":if q.multi_select { "multiselect" } else { "string" },
                "required":true,"custom":true,
                "options":q.options.iter().map(|o| json!({"value":o.label,"label":o.label,"description":o.description})).collect::<Vec<_>>()
            });
            if q.multi_select {
                field["minItems"] = json!(1);
                field["maxItems"] = json!(16);
            } else {
                field["minLength"] = json!(1);
                field["maxLength"] = json!(4096);
            }
            field
        }).collect())
    }

    fn answered(&self, content: Value) -> Result<Value, String> {
        let content = content
            .as_object()
            .ok_or("question answer is not an object")?;
        if content.len() != self.questions.len() {
            return Err("question answer fields differ".into());
        }
        let mut answers = serde_json::Map::new();
        for q in &self.questions {
            let value = content.get(&q.id).ok_or("question answer absent")?;
            let values = if q.multi_select {
                value
                    .as_array()
                    .filter(|v| !v.is_empty() && v.len() <= 128)
                    .ok_or("question selections absent or exceed bound")?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or("non-text question selection")
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                vec![value
                    .as_str()
                    .ok_or("question answer is not text")?
                    .to_owned()]
            };
            answers.insert(q.id.clone(), json!({"answers":values}));
        }
        Ok(json!({"status":"answered","answers":answers}))
    }
}

fn outcome(status: &str, message: &str) -> Value {
    json!({"status":status,"message":message,"answers":{}})
}

pub(super) struct Owner {
    session: SessionId,
    feedback: Option<crate::dogfood::session_status::RetryFeedback>,
    approvals: grapt_api::ApprovalWatch,
    waiting: Arc<crate::dogfood::human_input::Waits>,
}

impl ProjectToolExecutor {
    pub(super) fn question_owner(&self) -> Owner {
        Owner {
            session: self.parent.clone(),
            feedback: self.state_feedback.clone(),
            approvals: self.approvals.clone(),
            waiting: Arc::clone(&self.decision_waits),
        }
    }
}

pub(super) async fn run(
    producer: super::super::elicitation::FormProducer,
    owner: Owner,
    args: Arguments,
    hooks: Option<&super::super::hooks::HookRuntime>,
    call: &ToolCallId,
) -> Result<Vec<u8>, String> {
    let Owner {
        session,
        feedback,
        mut approvals,
        waiting,
    } = owner;
    let session = &session;
    let encode = |v: Value| serde_json::to_vec(&v).map_err(|e| e.to_string());
    if !approvals.available().map_err(|e| e.to_string())? {
        return encode(outcome("no_user_available", "No user available. Continue with a reasonable assumption or explain the blocker; do not ask again through this tool."));
    }
    let mut live = LiveQuestion::create(producer, session, &args, call)?;
    let _wait =
        decision_wait::DecisionWait::begin(&waiting, crate::dogfood::human_input::Kind::Question)?
            .with_feedback(feedback);
    // Share the Session observer used after ordinary durable form creation.
    if let Some(hooks) = hooks {
        if let Err(error) = hooks.waiting_for_input(session, Some((TOOL, call))).await {
            crate::diagnostics::report(format!(
                "question notification failed in {session}: {error}"
            ));
        }
    }
    loop {
        // A committed answer wins against a disconnect, as permission replies do.
        let state = form_by_id(lock(&live.producer.inbox).store(), session, &live.id)
            .map_err(|e| e.to_string())?
            .ok_or("live question disappeared")?
            .state;
        match state {
            FormState::Answered(answer) => {
                live.pending = false;
                return encode(args.answered(answer)?);
            }
            FormState::Cancelled => {
                live.pending = false;
                return encode(outcome("declined", "The user declined to answer."));
            }
            FormState::Suspended(_) => {
                return Err("question lost its live waiter; no answer consumed".into())
            }
            FormState::Pending => {}
        }
        if !approvals.available().map_err(|e| e.to_string())? {
            match live.retire()? {
                Some(FormState::Answered(answer)) => return encode(args.answered(answer)?),
                Some(FormState::Cancelled) => {
                    return encode(outcome("declined", "The user declined to answer."))
                }
                _ => {}
            }
            return encode(outcome(
                "no_user_available",
                "The user disconnected before answering. No answer was consumed.",
            ));
        }
        tokio::select! {
            lost = approvals.unavailable() => { lost.map_err(|e| e.to_string())?; }
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

struct LiveQuestion {
    producer: super::super::elicitation::FormProducer,
    session: SessionId,
    id: FormId,
    pending: bool,
}
impl LiveQuestion {
    fn event(&self) -> Result<EventId, String> {
        format!("evt_{}", self.producer.ids.mint().as_str())
            .parse()
            .map_err(|_| "question event identity".into())
    }
    fn create(
        producer: super::super::elicitation::FormProducer,
        session: &SessionId,
        args: &Arguments,
        call: &ToolCallId,
    ) -> Result<Self, String> {
        let id = format!("frm_{}", producer.ids.mint().as_str())
            .parse()
            .map_err(|_| "question form identity")?;
        let mut live = Self {
            producer,
            session: session.clone(),
            id,
            pending: false,
        };
        let fields = args.fields();
        serde_json::from_value::<grapt_api::u3a::FormFieldsDto>(fields.clone())
            .map_err(|e| format!("question field encoding refused before creation: {e}"))?;
        if let Some(error) = grapt_form::answer::validate_fields(&fields) {
            return Err(error);
        }
        let metadata = json!({"kind":TOOL,"callID":call.as_str()});
        let event = live.event()?;
        let mut inbox = lock(&live.producer.inbox);
        let pending =
            grapt_form::catalog::pending_forms(inbox.store()).map_err(|e| e.to_string())?;
        if pending.len() >= grapt_form::catalog::MAX_FORM_CATALOG_ROWS {
            return Err("pending form bound exhausted".into());
        }
        FormService::create_presented_on(
            inbox.store_mut(),
            &event,
            &live.id,
            session,
            FormPresentation {
                title: "The assistant is asking",
                fields: &fields,
                metadata: Some(&metadata),
            },
            live.producer.clock.now().as_millis(),
        )
        .map_err(|e| e.to_string())?;
        super::super::input_events::publish_id(&inbox, &live.producer.hub, session, &event);
        drop(inbox);
        live.pending = true;
        Ok(live)
    }
    fn retire(&mut self) -> Result<Option<FormState>, String> {
        if self.pending {
            let event = self.event()?;
            let mut inbox = lock(&self.producer.inbox);
            let outcome = FormService::cancel_on(
                inbox.store_mut(),
                &event,
                &self.id,
                self.producer.clock.now().as_millis(),
            )
            .map_err(|e| e.to_string())?;
            if outcome == grapt_types::CasOutcome::Applied {
                super::super::input_events::publish_id(
                    &inbox,
                    &self.producer.hub,
                    &self.session,
                    &event,
                );
                self.pending = false;
                return Ok(None);
            }
            let state = form_by_id(inbox.store(), &self.session, &self.id)
                .map_err(|e| e.to_string())?
                .ok_or("question disappeared during settlement")?
                .state;
            self.pending = false;
            return Ok(Some(state));
        }
        Ok(None)
    }
}
impl Drop for LiveQuestion {
    fn drop(&mut self) {
        if let Err(error) = self.retire() {
            self.producer
                .hub
                .invalidate_session_because(&self.session, "question retirement failed");
            crate::diagnostics::report(format!("question retirement failed: {error}"));
        }
    }
}

/// Retire lost waiters through the form owner; their audit stays settled and bounded.
///
/// A form that cannot be read or retired is skipped and returned as a warning,
/// so one bad form cannot stop the server from starting. A database failure
/// still fails recovery.
pub(in crate::dogfood) fn recover(
    store: &mut grapt_store::EventStore,
    ids: &dyn IdAuthority,
    clock: &dyn ClockAuthority,
) -> Result<Vec<String>, String> {
    let recoverable =
        grapt_form::catalog::pending_forms_for_recovery(store).map_err(|e| e.to_string())?;
    let mut warnings: Vec<String> = recoverable
        .skipped
        .iter()
        .map(ToString::to_string)
        .collect();
    for row in recoverable.forms {
        if row.info["metadata"]["kind"].as_str() != Some(TOOL) {
            continue;
        }
        match retire_lost(store, ids, clock, &row) {
            Ok(()) => {}
            Err(Unsettled::Storage(reason)) => return Err(reason),
            Err(Unsettled::Item(reason)) => warnings.push(format!(
                "form {} in session {}: {reason}",
                row.id, row.session
            )),
        }
    }
    Ok(warnings)
}

fn retire_lost(
    store: &mut grapt_store::EventStore,
    ids: &dyn IdAuthority,
    clock: &dyn ClockAuthority,
    row: &grapt_form::catalog::PendingForm,
) -> Result<(), Unsettled> {
    let event: EventId = format!("evt_{}", ids.mint().as_str())
        .parse()
        .map_err(|_| Unsettled::Item("question recovery identity".into()))?;
    let aggregate = grapt_store::AggregateId::Session(row.session.clone());
    store
        .elect_writer(&aggregate)
        .map_err(|error| Unsettled::write(&error))?;
    let result = FormService::cancel_on(store, &event, &row.id, clock.now().as_millis());
    store.release_held_writer(&aggregate);
    result.map(|_| ()).map_err(|error| Unsettled::form(&error))
}
