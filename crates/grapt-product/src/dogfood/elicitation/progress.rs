// Copyright 2025 OpenAI. Licensed under Apache-2.0.
//! One active MCP call publishes throttled, display-only tool progress.
use super::FormProducer;
use crate::dogfood::mcp::channel::ProgressObserver;
use grapt_api::u3a::{
    DogfoodEvent, FiniteF64, LiveEvent, ToolProgressData, ToolProgressEntry, ToolProgressMetadata,
    ToolProgressStatus, WireId,
};
use grapt_types::{ClockAuthority, EventId, MessageId, SessionId, ToolCallId};
use serde_json::Value;
use std::sync::Mutex;

#[derive(Default)]
struct Projection {
    value: Option<f64>,
    shown: Option<i64>,
}

pub(super) struct LiveProgress {
    producer: FormProducer,
    token: String,
    session: SessionId,
    message: MessageId,
    call: ToolCallId,
    source_call: ToolCallId,
    tool: String,
    projection: Mutex<Projection>,
}

impl LiveProgress {
    pub(super) fn new(
        producer: FormProducer,
        token: String,
        session: SessionId,
        message: MessageId,
        call: ToolCallId,
        source_call: ToolCallId,
        tool: String,
    ) -> Self {
        Self {
            producer,
            token,
            session,
            message,
            call,
            source_call,
            tool,
            projection: Mutex::default(),
        }
    }

    fn accept(&self, params: &Value) -> Option<String> {
        if params["progressToken"].as_str() != Some(self.token.as_str()) {
            return None;
        }
        let value = params["progress"]
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)?;
        let total = match params.get("total") {
            Some(total) => Some(
                total
                    .as_f64()
                    .filter(|total| total.is_finite() && *total > 0.0)?,
            ),
            None => None,
        };
        let now = self.producer.clock.now().as_millis();
        {
            let mut held = self.projection.lock().ok()?;
            if held.value.is_some_and(|old| value <= old) {
                return None;
            }
            held.value = Some(value);
            if held
                .shown
                .is_some_and(|shown| now.saturating_sub(shown) < 500)
            {
                return None;
            }
            held.shown = Some(now);
        }
        let message = params
            .get("message")
            .and_then(Value::as_str)
            .map(|message| {
                message
                    .chars()
                    .filter(|character| !character.is_control())
                    .scan(0usize, |bytes, character| {
                        *bytes += character.len_utf8();
                        (*bytes <= 1024).then_some(character)
                    })
                    .collect::<String>()
            })
            .filter(|message| !message.trim().is_empty());
        Some(message.unwrap_or_else(|| {
            total
                .map(|total| format!("{:.0}%", (value / total).min(1.0) * 100.0))
                .unwrap_or_else(|| format!("Progress {value}"))
        }))
    }

    fn event(&self, message: &str, status: ToolProgressStatus) -> Option<DogfoodEvent> {
        let entry = ToolProgressEntry::new(&self.tool, status)
            .ok()?
            .with_message(message)
            .ok()?;
        let data = ToolProgressData::new(
            WireId::parse(self.session.as_str().into())?,
            WireId::parse(self.message.as_str().into())?,
            WireId::parse(self.call.as_str().into())?,
            ToolProgressMetadata::new(vec![entry]).ok()?,
        )
        .with_source_call(WireId::parse(self.source_call.as_str().into())?);
        Some(DogfoodEvent::ToolProgress(LiveEvent::new(
            WireId::<EventId>::parse(self.producer.event().ok()?.as_str().into())?,
            FiniteF64::try_new(self.producer.clock.now().as_millis() as f64).ok()?,
            data,
        )))
    }
}

impl ProgressObserver for LiveProgress {
    fn token(&self) -> &str {
        &self.token
    }
    fn notify(&self, params: &Value) {
        if let Some(event) = self
            .accept(params)
            .and_then(|message| self.event(&message, ToolProgressStatus::Running))
        {
            // Advisory delivery cannot change the tool's result or its ledger.
            let _ = self.producer.hub.publish(&event);
        }
    }
}

impl Drop for LiveProgress {
    fn drop(&mut self) {
        if self
            .projection
            .get_mut()
            .is_ok_and(|projection| projection.shown.is_some())
        {
            if let Some(event) = self.event("", ToolProgressStatus::Completed) {
                let _ = self.producer.hub.publish(&event);
            }
        }
    }
}
