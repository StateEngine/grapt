//! Tool identity and execution ownership. Version 1 remains local and unchanged;
//! version 2 explicitly names the executor before tool input is committed.

use grapt_types::{MessageId, SessionId, ToolCallId, ToolDefinitionId, ToolExecution};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInputIdentity {
    pub session: SessionId,
    pub message: MessageId,
    pub call: ToolCallId,
    pub tool: ToolDefinitionId,
    pub execution: ToolExecution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityError;

impl core::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("tool input identity has an invalid version, fields, identity, or executor")
    }
}
impl std::error::Error for IdentityError {}

pub fn encode(identity: &ToolInputIdentity) -> Value {
    json!({
        "sessionID": identity.session.as_str(), "assistantMessageID": identity.message.as_str(),
        "callID": identity.call.as_str(), "name": identity.tool.as_str(),
        "execution": match identity.execution { ToolExecution::Local => "local", ToolExecution::ProviderHosted => "provider_hosted" },
    })
}

/// The envelope's actual event version selects the decoder. An absent
/// executor in version 2 is corrupt data, never a default-local call.
pub fn decode(version: u32, value: &Value) -> Result<ToolInputIdentity, IdentityError> {
    let fields = value.as_object().ok_or(IdentityError)?;
    let expected = match version {
        1 => 4,
        2 => 5,
        _ => return Err(IdentityError),
    };
    if fields.len() != expected
        || fields.keys().any(|field| {
            !matches!(
                field.as_str(),
                "sessionID" | "assistantMessageID" | "callID" | "name"
            ) && !(version == 2 && field == "execution")
        })
    {
        return Err(IdentityError);
    }
    let text = |field: &str| {
        fields
            .get(field)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .ok_or(IdentityError)
    };
    let execution = match version {
        1 => ToolExecution::Local,
        2 => match text("execution")? {
            "local" => ToolExecution::Local,
            "provider_hosted" => ToolExecution::ProviderHosted,
            _ => return Err(IdentityError),
        },
        _ => return Err(IdentityError),
    };
    Ok(ToolInputIdentity {
        session: text("sessionID")?.parse().map_err(|_| IdentityError)?,
        message: text("assistantMessageID")?
            .parse()
            .map_err(|_| IdentityError)?,
        call: ToolCallId::new(text("callID")?).ok_or(IdentityError)?,
        tool: ToolDefinitionId::new(text("name")?).ok_or(IdentityError)?,
        execution,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn local() -> Value {
        json!({"sessionID":"ses_123456789abc", "assistantMessageID":"msg_123456789abc", "callID":"call-one", "name":"web_search"})
    }
    #[test]
    fn v1_is_local_and_v2_requires_an_explicit_closed_executor() -> Result<(), IdentityError> {
        assert_eq!(decode(1, &local())?.execution, ToolExecution::Local);
        assert!(decode(2, &local()).is_err());
        for execution in [ToolExecution::Local, ToolExecution::ProviderHosted] {
            let mut fact = decode(1, &local())?;
            fact.execution = execution;
            assert_eq!(decode(2, &encode(&fact))?, fact);
            assert!(decode(1, &encode(&fact)).is_err());
        }
        for bad in [
            Value::Null,
            json!(false),
            json!("function"),
            json!("hosted"),
        ] {
            let mut wire = local();
            wire["execution"] = bad;
            assert!(decode(2, &wire).is_err());
        }
        let mut unknown = encode(&decode(1, &local())?);
        unknown["extra"] = true.into();
        assert!(decode(2, &unknown).is_err());
        assert!(decode(3, &local()).is_err());
        Ok(())
    }
}
