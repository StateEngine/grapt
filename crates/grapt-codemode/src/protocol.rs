use std::collections::BTreeSet;
use std::io::{Read, Write};

use grapt_types::{ToolDefinitionId, ToolSchemaVersion};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::{CATALOG_BYTES, CATALOG_TOOLS, FRAME_BYTES, SOURCE_BYTES};

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("CodeMode protocol I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("CodeMode protocol JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("CodeMode {0} exceeds its bound or is malformed")]
    Invalid(&'static str),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogTool {
    #[serde(with = "tool_name")]
    pub name: ToolDefinitionId,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteRequest {
    pub code: String,
    #[serde(with = "catalog_version")]
    pub catalog: ToolSchemaVersion,
    pub tools: Vec<CatalogTool>,
}

impl ExecuteRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.code.len() > SOURCE_BYTES {
            return Err(ProtocolError::Invalid("source"));
        }
        if self.catalog.as_str().len() > 256 || self.tools.len() > CATALOG_TOOLS {
            return Err(ProtocolError::Invalid("catalog"));
        }
        let mut names = BTreeSet::new();
        for tool in &self.tools {
            if tool.name.as_str().len() > 256
                || tool.description.len() > 4096
                || !tool.input_schema.is_object()
                || tool
                    .output_schema
                    .as_ref()
                    .is_some_and(|schema| !schema.is_object())
                || !names.insert(&tool.name)
            {
                return Err(ProtocolError::Invalid("catalog tool"));
            }
        }
        bounded_json(&self.tools, CATALOG_BYTES)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCall {
    pub ordinal: u16,
    #[serde(with = "catalog_version")]
    pub catalog: ToolSchemaVersion,
    #[serde(with = "tool_name")]
    pub tool: ToolDefinitionId,
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestFailure {
    ApprovalUnavailable,
    InvalidSource,
    GuestError,
    TypeError,
    ReferenceError,
    RangeError,
    Interrupted,
    JobLimit,
    OutputLimit,
    SerializationError,
    UnresolvedPromise,
    HostProtocol,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum GuestResult {
    Completed {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_value"
        )]
        value: Option<Value>,
    },
    Failed {
        reason: GuestFailure,
        /// What the guest threw, bounded by [`crate::DETAIL_BYTES`], so the model can
        /// tell an exception from a nested refusal (SC-TOOL-01a).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// Captured from an actual host rejection, never inferred from guest text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nested_failure: Option<HostFailure>,
    },
}

fn present_value<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(decoder).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerMessage {
    Call { call: HostCall },
    Finished { result: GuestResult },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostResult {
    Completed {
        value: Value,
    },
    Failed {
        reason: HostFailure,
        /// The nested tool's own failure text, bounded by [`crate::DETAIL_BYTES`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostFailure {
    ApprovalUnavailable,
    InvalidArguments,
    StaleCatalog,
    AuthorityUnavailable,
    EffectFailed,
    Cancelled,
    ResultUnknown,
    Capacity,
    ProducerVersionRefused,
    ProducerReconnected,
    ProducerReconnectFailed,
    ProducerReadRetryRefused,
    ProducerWriteRetryRefused,
    ConnectionReplaced,
    McpUnavailable,
    InvalidPlacement,
    McpProtocol,
    McpToolRefused,
    IdentityArguments,
    MessageKind,
    AuthorityReference,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostReply {
    pub ordinal: u16,
    pub result: HostResult,
}

struct LimitedBytes {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("CodeMode JSON bound exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn bounded_json<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> Result<Vec<u8>, ProtocolError> {
    let mut output = LimitedBytes {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, value)?;
    Ok(output.bytes)
}

pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), ProtocolError> {
    let bytes = bounded_json(value, FRAME_BYTES)?;
    let length = u32::try_from(bytes.len()).map_err(|_| ProtocolError::Invalid("frame"))?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, ProtocolError> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > FRAME_BYTES {
        return Err(ProtocolError::Invalid("frame"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

mod tool_name {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &ToolDefinitionId,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.as_str())
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        decoder: D,
    ) -> Result<ToolDefinitionId, D::Error> {
        let value = String::deserialize(decoder)?;
        if value.len() > 256 {
            return Err(serde::de::Error::custom("tool name exceeds bound"));
        }
        ToolDefinitionId::new(value).ok_or_else(|| serde::de::Error::custom("empty tool name"))
    }
}

mod catalog_version {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &ToolSchemaVersion,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.as_str())
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        decoder: D,
    ) -> Result<ToolSchemaVersion, D::Error> {
        let value = String::deserialize(decoder)?;
        if value.len() > 256 {
            return Err(serde::de::Error::custom("catalog identity exceeds bound"));
        }
        ToolSchemaVersion::new(value)
            .ok_or_else(|| serde::de::Error::custom("empty catalog identity"))
    }
}
