//! Non-user agent mail carried by the existing inbox admission envelope.

use grapt_types::{MessageId, SessionId, ToolCallId};
use serde_json::{json, Value};

pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_SOURCE_BYTES: usize = MAX_TEXT_BYTES * 6 + 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMessage {
    pub session: SessionId,
    pub input: MessageId,
    pub sender: SessionId,
    pub call: ToolCallId,
    pub text: String,
}

impl AgentMessage {
    pub fn encode(&self) -> Value {
        json!({"sessionID":self.session.as_str(), "inputID":self.input.as_str(),
            "input":{"type":"agent-message", "rustAuthorized":true,
                "data":{"senderSessionID":self.sender.as_str(),
                    "callID":self.call.as_str(), "text":self.text}}})
    }

    pub fn decode(data: &Value) -> Result<Self, String> {
        let closed = |value: &Value, fields: &[&str]| {
            value.as_object().is_some_and(|object| {
                object.len() == fields.len()
                    && object.keys().all(|key| fields.contains(&key.as_str()))
            })
        };
        let input = data.get("input").ok_or("agent message has no input")?;
        let body = input.get("data").ok_or("agent message has no data")?;
        if !closed(data, &["sessionID", "inputID", "input"])
            || !closed(input, &["type", "rustAuthorized", "data"])
            || !closed(body, &["senderSessionID", "callID", "text"])
            || input.get("type").and_then(Value::as_str) != Some("agent-message")
            || input.get("rustAuthorized").and_then(Value::as_bool) != Some(true)
        {
            return Err("agent message has invalid fields or authorization marker".into());
        }
        let text = |value: &Value, field: &str| {
            value
                .get(field)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("agent message has no {field}"))
        };
        let identity = |value: &Value, field: &str| {
            let raw = text(value, field)?;
            if raw.len() > 512 {
                return Err(format!("agent message {field} exceeds 512 bytes"));
            }
            Ok(raw)
        };
        let message = Self {
            session: identity(data, "sessionID")?
                .parse()
                .map_err(|error| format!("agent message session: {error}"))?,
            input: identity(data, "inputID")?
                .parse()
                .map_err(|error| format!("agent message input: {error}"))?,
            sender: identity(body, "senderSessionID")?
                .parse()
                .map_err(|error| format!("agent message sender: {error}"))?,
            call: ToolCallId::new(identity(body, "callID")?)
                .ok_or("agent message call is empty")?,
            text: text(body, "text")?,
        };
        if message.text.trim().is_empty() || message.text.len() > MAX_TEXT_BYTES {
            return Err("agent message must contain text and fit within 64 KiB".into());
        }
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_mail_has_a_closed_non_user_envelope() -> Result<(), String> {
        let data = json!({"sessionID":"ses_target", "inputID":"msg_mail",
            "input":{"type":"agent-message", "rustAuthorized":true,
                "data":{"senderSessionID":"ses_sender", "callID":"call_mail", "text":"quoted mail"}}});
        assert_eq!(AgentMessage::decode(&data)?.encode(), data);
        for field in ["senderSessionID", "callID", "text"] {
            let mut bad = data.clone();
            bad["input"]["data"][field] = Value::Null;
            assert!(AgentMessage::decode(&bad).is_err());
        }
        let mut unsigned = data.clone();
        unsigned["input"]["rustAuthorized"] = json!(false);
        assert!(AgentMessage::decode(&unsigned).is_err());
        let mut unknown = data.clone();
        unknown["input"]["data"]["role"] = json!("user");
        assert!(AgentMessage::decode(&unknown).is_err());
        for text in [" ".to_owned(), "x".repeat(MAX_TEXT_BYTES + 1)] {
            let mut bad = data.clone();
            bad["input"]["data"]["text"] = json!(text);
            assert!(AgentMessage::decode(&bad).is_err());
        }
        Ok(())
    }
}
