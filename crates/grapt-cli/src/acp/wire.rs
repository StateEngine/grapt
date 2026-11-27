//! Strict JSON-RPC envelopes and bounded reusable newline framing.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::CliLimits;

const READ_CHUNK_BYTES: usize = 8_192;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RpcId {
    Signed(i64),
    Unsigned(u64),
    String(String),
}

impl RpcId {
    pub fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Number(value) => value
                .as_i64()
                .map(Self::Signed)
                .or_else(|| value.as_u64().map(Self::Unsigned)),
            Value::String(value) if !value.is_empty() => Some(Self::String(value)),
            _ => None,
        }
    }

    pub fn as_value(&self) -> Value {
        match self {
            Self::Signed(value) => Value::from(*value),
            Self::Unsigned(value) => Value::from(*value),
            Self::String(value) => Value::String(value.clone()),
        }
    }
}

impl Serialize for RpcId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_value().serialize(serializer)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcFault {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcFault {
    pub fn parse_error() -> Self {
        Self::new(-32700, "Parse error")
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(-32600, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::with_data(-32601, "Method not found", json!({"method": method}))
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }

    pub fn invalid_params_with_data(message: impl Into<String>, data: Value) -> Self {
        Self::with_data(-32602, message, data)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(-32603, message)
    }

    pub fn server_busy(maximum: usize) -> Self {
        Self::with_data(
            -32000,
            "ACP concurrent request bound reached",
            json!({"maximum": maximum}),
        )
    }

    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn with_data(code: i64, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }

    pub fn value(&self) -> Value {
        let mut value = json!({"code": self.code, "message": self.message});
        if let Some(data) = &self.data {
            value["data"] = data.clone();
        }
        value
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Request {
        id: RpcId,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: RpcId,
        result: Result<Value, RpcFault>,
    },
}

impl Inbound {
    pub fn decode(frame: &[u8]) -> Result<Self, (Option<RpcId>, RpcFault)> {
        let value: Value =
            serde_json::from_slice(frame).map_err(|_| (None, RpcFault::parse_error()))?;
        let object = value.as_object().ok_or_else(|| {
            (
                None,
                RpcFault::invalid_request("JSON-RPC frame must be an object"),
            )
        })?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err((
                request_id(object),
                RpcFault::invalid_request("jsonrpc must equal 2.0"),
            ));
        }
        if let Some(method) = object.get("method") {
            let method = method
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    (
                        request_id(object),
                        RpcFault::invalid_request("method must be a non-empty string"),
                    )
                })?;
            reject_unknown(object, &["jsonrpc", "id", "method", "params"])
                .map_err(|fault| (request_id(object), fault))?;
            let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
            if !params.is_object() && !params.is_array() {
                return Err((
                    request_id(object),
                    RpcFault::invalid_params("params must be an object or array"),
                ));
            }
            return match object.get("id") {
                Some(value) => RpcId::from_value(value.clone())
                    .map(|id| Self::Request {
                        id,
                        method: method.to_owned(),
                        params,
                    })
                    .ok_or_else(|| {
                        (
                            None,
                            RpcFault::invalid_request("request id must be a string or number"),
                        )
                    }),
                None => Ok(Self::Notification {
                    method: method.to_owned(),
                    params,
                }),
            };
        }

        reject_unknown(object, &["jsonrpc", "id", "result", "error"])
            .map_err(|fault| (request_id(object), fault))?;
        let id = object
            .get("id")
            .cloned()
            .and_then(RpcId::from_value)
            .ok_or_else(|| {
                (
                    None,
                    RpcFault::invalid_request("response id must be a string or number"),
                )
            })?;
        match (object.get("result"), object.get("error")) {
            (Some(result), None) => Ok(Self::Response {
                id,
                result: Ok(result.clone()),
            }),
            (None, Some(error)) => Ok(Self::Response {
                id,
                result: Err(decode_fault(error).map_err(|fault| (None, fault))?),
            }),
            _ => Err((
                Some(id),
                RpcFault::invalid_request("response must contain exactly one of result or error"),
            )),
        }
    }

    pub fn success(id: &RpcId, result: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "result": result})
    }

    pub fn failure(id: Option<&RpcId>, fault: &RpcFault) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id.map_or(Value::Null, RpcId::as_value),
            "error": fault.value()
        })
    }
}

fn request_id(object: &serde_json::Map<String, Value>) -> Option<RpcId> {
    object.get("id").cloned().and_then(RpcId::from_value)
}

fn reject_unknown(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> Result<(), RpcFault> {
    let unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(RpcFault::invalid_request(format!(
            "unknown JSON-RPC field: {}",
            unknown.join(", ")
        )))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFault {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

fn decode_fault(value: &Value) -> Result<RpcFault, RpcFault> {
    let fault: WireFault = serde_json::from_value(value.clone())
        .map_err(|_| RpcFault::invalid_request("response error is malformed"))?;
    Ok(RpcFault {
        code: fault.code,
        message: fault.message,
        data: fault.data,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameRead {
    Frame(Vec<u8>),
    TooLarge { maximum: usize },
    Eof,
}

#[derive(Debug)]
pub struct FrameReader<R> {
    input: R,
    maximum: usize,
    frame: Vec<u8>,
    chunk: [u8; READ_CHUNK_BYTES],
    start: usize,
    end: usize,
    discarding: bool,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(input: R, limits: CliLimits) -> Self {
        let maximum = limits.values().stdin_record_bytes;
        Self {
            input,
            maximum,
            frame: Vec::with_capacity(maximum.min(READ_CHUNK_BYTES)),
            chunk: [0; READ_CHUNK_BYTES],
            start: 0,
            end: 0,
            discarding: false,
        }
    }

    pub async fn next(&mut self) -> std::io::Result<FrameRead> {
        loop {
            if self.start == self.end {
                self.end = self.input.read(&mut self.chunk).await?;
                self.start = 0;
                if self.end == 0 {
                    if self.discarding {
                        self.discarding = false;
                        return Ok(FrameRead::TooLarge {
                            maximum: self.maximum,
                        });
                    }
                    if self.frame.is_empty() {
                        return Ok(FrameRead::Eof);
                    }
                    return Ok(FrameRead::Frame(core::mem::take(&mut self.frame)));
                }
            }

            let byte = self.chunk[self.start];
            self.start = self.start.saturating_add(1);
            if byte == b'\n' {
                if self.discarding {
                    self.discarding = false;
                    self.frame.clear();
                    return Ok(FrameRead::TooLarge {
                        maximum: self.maximum,
                    });
                }
                if self.frame.last() == Some(&b'\r') {
                    self.frame.pop();
                }
                if self.frame.is_empty() {
                    continue;
                }
                return Ok(FrameRead::Frame(core::mem::take(&mut self.frame)));
            }
            if self.discarding {
                continue;
            }
            if self.frame.len() >= self.maximum {
                self.frame.clear();
                self.discarding = true;
                continue;
            }
            self.frame.push(byte);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn json_rpc_ids_are_integral_and_request_fields_are_closed() {
        let Ok(request) =
            Inbound::decode(br#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{}}"#)
        else {
            panic!("valid JSON-RPC request was refused");
        };
        assert!(matches!(
            request,
            Inbound::Request {
                id: RpcId::Signed(7),
                ..
            }
        ));
        let fractional =
            Inbound::decode(br#"{"jsonrpc":"2.0","id":1.5,"method":"initialize","params":{}}"#);
        assert!(matches!(
            fractional,
            Err((None, RpcFault { code: -32600, .. }))
        ));
        let unknown = Inbound::decode(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{},"extra":true}"#,
        );
        assert!(matches!(
            unknown,
            Err((Some(RpcId::Signed(1)), RpcFault { code: -32600, .. }))
        ));
    }

    #[tokio::test]
    async fn frames_survive_splits_crlf_and_multiple_records_without_eof() -> TestResult {
        let (mut writer, reader) = tokio::io::duplex(512);
        let task = tokio::spawn(async move {
            writer.write_all(b"{\"a\":").await?;
            writer.write_all(b"1}\r\n{\"b\":2}\n").await?;
            Ok::<(), std::io::Error>(())
        });
        let mut frames = FrameReader::new(reader, CliLimits::production());
        assert_eq!(
            frames.next().await?,
            FrameRead::Frame(br#"{"a":1}"#.to_vec())
        );
        assert_eq!(
            frames.next().await?,
            FrameRead::Frame(br#"{"b":2}"#.to_vec())
        );
        assert_eq!(frames.next().await?, FrameRead::Eof);
        task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn oversized_frame_is_discarded_and_the_connection_recovers() -> TestResult {
        let limits = CliLimits::production();
        let maximum = limits.values().stdin_record_bytes;
        let mut bytes = vec![b'x'; maximum.saturating_add(1)];
        bytes.extend_from_slice(b"\n{}\n");
        let mut frames = FrameReader::new(bytes.as_slice(), limits);
        assert_eq!(frames.next().await?, FrameRead::TooLarge { maximum });
        assert_eq!(frames.next().await?, FrameRead::Frame(b"{}".to_vec()));
        Ok(())
    }
}
