//! Bounded advisory progress for an active tool and committed nested tools.
use super::{BoundedString, BoundedVec, WireId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolProgressStatus {
    Running,
    Completed,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolProgressEntry {
    #[serde(deserialize_with = "nonempty_tool")]
    tool: BoundedString<256>,
    status: ToolProgressStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "progress_message"
    )]
    message: Option<BoundedString<1024>>,
}
fn nonempty_tool<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BoundedString<256>, D::Error> {
    let tool = BoundedString::<256>::deserialize(deserializer)?;
    if tool.as_str().is_empty() {
        return Err(serde::de::Error::custom("progress tool name is empty"));
    }
    Ok(tool)
}
fn progress_message<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<BoundedString<1024>>, D::Error> {
    let message = Option::<BoundedString<1024>>::deserialize(deserializer)?;
    if message
        .as_ref()
        .is_some_and(|text| text.as_str().chars().any(char::is_control))
    {
        return Err(serde::de::Error::custom(
            "progress message contains control characters",
        ));
    }
    Ok(message)
}
impl ToolProgressEntry {
    pub fn new(tool: &str, status: ToolProgressStatus) -> Result<Self, &'static str> {
        if tool.is_empty() {
            return Err("progress tool name is empty");
        }
        Ok(Self {
            tool: BoundedString::try_new(tool)?,
            status,
            message: None,
        })
    }
    pub fn with_message(mut self, message: &str) -> Result<Self, &'static str> {
        if message.chars().any(char::is_control) {
            return Err("progress message contains control characters");
        }
        self.message = Some(BoundedString::try_new(message)?);
        Ok(self)
    }
    pub fn message(&self) -> Option<&str> {
        self.message.as_ref().map(BoundedString::as_str)
    }
    pub fn tool(&self) -> &str {
        self.tool.as_str()
    }
    pub fn status(&self) -> ToolProgressStatus {
        self.status
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolProgressMetadata {
    #[serde(rename = "toolCalls")]
    calls: BoundedVec<ToolProgressEntry, 64>,
}
impl ToolProgressMetadata {
    pub fn new(calls: Vec<ToolProgressEntry>) -> Result<Self, &'static str> {
        Ok(Self {
            calls: BoundedVec::try_new(calls)?,
        })
    }
    pub fn calls(&self) -> &[ToolProgressEntry] {
        self.calls.as_slice()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolProgressData {
    #[serde(rename = "sessionID")]
    session: WireId<grapt_types::SessionId>,
    #[serde(rename = "assistantMessageID")]
    message: WireId<grapt_types::MessageId>,
    #[serde(rename = "callID")]
    call: WireId<grapt_types::ToolCallId>,
    metadata: ToolProgressMetadata,
    #[serde(
        default,
        rename = "sourceCallID",
        skip_serializing_if = "Option::is_none"
    )]
    source_call: Option<WireId<grapt_types::ToolCallId>>,
}
impl ToolProgressData {
    pub fn new(
        session: WireId<grapt_types::SessionId>,
        message: WireId<grapt_types::MessageId>,
        call: WireId<grapt_types::ToolCallId>,
        metadata: ToolProgressMetadata,
    ) -> Self {
        Self {
            session,
            message,
            call,
            metadata,
            source_call: None,
        }
    }
    pub fn with_source_call(mut self, call: WireId<grapt_types::ToolCallId>) -> Self {
        self.source_call = Some(call);
        self
    }
    pub fn source_call(&self) -> Option<&grapt_types::ToolCallId> {
        self.source_call.as_ref().map(WireId::as_inner)
    }
    pub fn session(&self) -> &grapt_types::SessionId {
        self.session.as_inner()
    }
    pub fn message(&self) -> &grapt_types::MessageId {
        self.message.as_inner()
    }
    pub fn call(&self) -> &grapt_types::ToolCallId {
        self.call.as_inner()
    }
    pub fn metadata(&self) -> &ToolProgressMetadata {
        &self.metadata
    }
}

// Committed parent lifecycle projected for the retained frontend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolInputStartedData {
    #[serde(rename = "sessionID")]
    session: WireId<grapt_types::SessionId>,
    #[serde(rename = "assistantMessageID")]
    message: WireId<grapt_types::MessageId>,
    #[serde(rename = "callID")]
    call: WireId<grapt_types::ToolCallId>,
    name: BoundedString<256>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolCalledData {
    #[serde(rename = "sessionID")]
    session: WireId<grapt_types::SessionId>,
    #[serde(rename = "assistantMessageID")]
    message: WireId<grapt_types::MessageId>,
    #[serde(rename = "callID")]
    call: WireId<grapt_types::ToolCallId>,
    input: super::ToolInputObject,
    executed: bool,
}

impl ToolCalledData {
    pub fn new(
        session: WireId<grapt_types::SessionId>,
        message: WireId<grapt_types::MessageId>,
        call: WireId<grapt_types::ToolCallId>,
        input: &str,
    ) -> Result<Self, String> {
        let limits = grapt_schema::DecodeLimits::new(256 * 1024, 32).map_err(|e| e.to_string())?;
        let input: super::ToolInputObject =
            grapt_schema::decode_json(input.as_bytes(), limits).map_err(|e| e.to_string())?;
        Ok(Self {
            session,
            message,
            call,
            input,
            executed: false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSuccessData {
    #[serde(rename = "sessionID")]
    session: WireId<grapt_types::SessionId>,
    #[serde(rename = "assistantMessageID")]
    message: WireId<grapt_types::MessageId>,
    #[serde(rename = "callID")]
    call: WireId<grapt_types::ToolCallId>,
    #[serde(deserialize_with = "nonempty_content")]
    content: BoundedVec<super::ToolContentDto, 33>,
    metadata: super::OpenObject,
    executed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolFailedData {
    #[serde(rename = "sessionID")]
    session: WireId<grapt_types::SessionId>,
    #[serde(rename = "assistantMessageID")]
    message: WireId<grapt_types::MessageId>,
    #[serde(rename = "callID")]
    call: WireId<grapt_types::ToolCallId>,
    error: super::SessionStructuredErrorDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<BoundedVec<super::ToolContentDto, 33>>,
    metadata: super::OpenObject,
    executed: bool,
}

/// Decode the shared public result without applying metadata limits to its
/// unused input. The terminal wire carries the input already published at call.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultProjection {
    status: ToolResultStatus,
    #[serde(rename = "input")]
    _input: serde::de::IgnoredAny,
    content: Option<BoundedVec<super::ToolContentDto, 33>>,
    metadata: super::OpenObject,
    error: Option<super::SessionStructuredErrorDto>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ToolResultStatus {
    Completed,
    Error,
}

impl ToolSuccessData {
    pub fn from_history(
        session: WireId<grapt_types::SessionId>,
        message: WireId<grapt_types::MessageId>,
        call: WireId<grapt_types::ToolCallId>,
        state: ToolResultProjection,
    ) -> Result<Self, &'static str> {
        let content = state.content.ok_or("tool success content is absent")?;
        if state.status != ToolResultStatus::Completed
            || state.error.is_some()
            || content.as_slice().is_empty()
        {
            return Err("tool success projection is invalid");
        }
        Ok(Self {
            session,
            message,
            call,
            content,
            metadata: state.metadata,
            executed: false,
        })
    }
}

impl ToolFailedData {
    pub fn from_history(
        session: WireId<grapt_types::SessionId>,
        message: WireId<grapt_types::MessageId>,
        call: WireId<grapt_types::ToolCallId>,
        state: ToolResultProjection,
    ) -> Result<Self, &'static str> {
        if state.status != ToolResultStatus::Error {
            return Err("tool failure projection is invalid");
        }
        Ok(Self {
            session,
            message,
            call,
            error: state.error.ok_or("tool failure error is absent")?,
            content: state.content,
            metadata: state.metadata,
            executed: false,
        })
    }
}

fn nonempty_content<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BoundedVec<super::ToolContentDto, 33>, D::Error> {
    let content = BoundedVec::<super::ToolContentDto, 33>::deserialize(deserializer)?;
    if content.as_slice().is_empty() {
        return Err(serde::de::Error::custom("tool success content is empty"));
    }
    Ok(content)
}
