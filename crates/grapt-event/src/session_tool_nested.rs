//! A nested call is owned by its parent and is not provider/model content.
//! Its input artifact commits before this admission fact; effects follow it.

use crate::session_tool_terminal::{
    decode_artifact, encode_artifact, invalid, object, reject_unknown, required, required_str,
    PayloadError,
};
use grapt_types::{
    CaptureDisposition, PhysicalAttemptId, ToolCallId, ToolDefinitionId, ToolSchemaVersion,
    ToolTerminalArtifact,
};
use serde_json::Value;

const AT: &str = "session.tool.nested.started.1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedToolStartedFact {
    attempt: PhysicalAttemptId,
    parent: ToolCallId,
    call: ToolCallId,
    tool: ToolDefinitionId,
    catalog: ToolSchemaVersion,
    input: ToolTerminalArtifact,
}

impl NestedToolStartedFact {
    pub fn new(
        attempt: PhysicalAttemptId,
        parent: ToolCallId,
        call: ToolCallId,
        tool: ToolDefinitionId,
        catalog: ToolSchemaVersion,
        input: ToolTerminalArtifact,
    ) -> Result<Self, PayloadError> {
        if parent == call {
            return Err(invalid(AT, "call", "a child distinct from its parent"));
        }
        if tool.as_str().len() > 256 || catalog.as_str().len() > 256 {
            return Err(invalid(AT, "tool/catalog", "bounded definition identities"));
        }
        if input.disposition() != CaptureDisposition::Complete {
            return Err(invalid(AT, "input", "a complete captured input"));
        }
        Ok(Self {
            attempt,
            parent,
            call,
            tool,
            catalog,
            input,
        })
    }
    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }
    pub fn parent(&self) -> &ToolCallId {
        &self.parent
    }
    pub fn call(&self) -> &ToolCallId {
        &self.call
    }
    pub fn tool(&self) -> &ToolDefinitionId {
        &self.tool
    }
    pub fn catalog(&self) -> &ToolSchemaVersion {
        &self.catalog
    }
    pub fn input(&self) -> &ToolTerminalArtifact {
        &self.input
    }
}

pub fn encode(fact: &NestedToolStartedFact) -> Value {
    serde_json::json!({"attempt":fact.attempt.as_str(), "parent":fact.parent.as_str(),
        "call":fact.call.as_str(), "tool":fact.tool.as_str(), "catalog":fact.catalog.as_str(),
        "input":encode_artifact(&fact.input)})
}

pub fn decode(value: &Value) -> Result<NestedToolStartedFact, PayloadError> {
    let root = object(value, AT)?;
    reject_unknown(
        root,
        AT,
        &["attempt", "parent", "call", "tool", "catalog", "input"],
    )?;
    let attempt = PhysicalAttemptId::new(required_str(root, AT, "attempt")?)
        .ok_or_else(|| invalid(AT, "attempt", "a physical attempt"))?;
    let parent = ToolCallId::new(required_str(root, AT, "parent")?)
        .ok_or_else(|| invalid(AT, "parent", "a parent call"))?;
    let call = ToolCallId::new(required_str(root, AT, "call")?)
        .ok_or_else(|| invalid(AT, "call", "a child call"))?;
    let tool = ToolDefinitionId::new(required_str(root, AT, "tool")?)
        .ok_or_else(|| invalid(AT, "tool", "a tool definition"))?;
    let catalog = ToolSchemaVersion::new(required_str(root, AT, "catalog")?)
        .ok_or_else(|| invalid(AT, "catalog", "a catalog identity"))?;
    let input = decode_artifact(required(root, AT, "input")?, AT)?;
    NestedToolStartedFact::new(attempt, parent, call, tool, catalog, input)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> Value {
        serde_json::json!({"attempt":"attempt-1","parent":"parent-1","call":"child-1",
            "tool":"server_tool","catalog":"schema-1","input":{"id":"input-1","length":2,
            "digest":"sha256-input","disposition":"complete"}})
    }
    #[test]
    fn nested_identity_and_input_reference_round_trip() {
        let value = valid();
        let fact = decode(&value).unwrap_or_else(|error| panic!("valid nested fact: {error}"));
        assert_eq!(encode(&fact), value);
        assert_ne!(fact.parent(), fact.call());
    }
    #[test]
    fn missing_null_unknown_self_parent_and_truncated_input_are_refused() {
        for field in ["attempt", "parent", "call", "tool", "catalog", "input"] {
            let mut value = valid();
            value
                .as_object_mut()
                .unwrap_or_else(|| panic!("fixture object"))
                .remove(field);
            assert!(decode(&value).is_err(), "missing {field}");
            value[field] = Value::Null;
            assert!(decode(&value).is_err(), "null {field}");
        }
        let mut value = valid();
        value["call"] = value["parent"].clone();
        assert!(decode(&value).is_err());
        let mut value = valid();
        value["extra"] = Value::Bool(true);
        assert!(decode(&value).is_err());
        let mut value = valid();
        value["input"]["disposition"] = Value::from("truncated");
        value["input"]["captured"] = Value::from(2);
        assert!(decode(&value).is_err());
    }
}
