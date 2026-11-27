//! Server requests nested in a client exchange. Correlation spaces are directional.
use crate::{McpBounds, McpProtocolError};
use serde_json::{json, Value};

/// Validated server-owned correlation, preserved without numeric coercion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRequest {
    id: Value,
    method: String,
    params: Value,
}

impl ServerRequest {
    pub fn id(&self) -> &Value {
        &self.id
    }
    pub fn method(&self) -> &str {
        &self.method
    }
    pub fn params(&self) -> &Value {
        &self.params
    }

    pub fn result(&self, bounds: McpBounds, result: Value) -> Result<Vec<u8>, McpProtocolError> {
        encode(
            bounds,
            json!({"jsonrpc":"2.0", "id":self.id, "result":result}),
        )
    }

    pub fn error(
        &self,
        bounds: McpBounds,
        code: i64,
        message: &str,
    ) -> Result<Vec<u8>, McpProtocolError> {
        encode(
            bounds,
            json!({"jsonrpc":"2.0", "id":self.id, "error":{"code":code,"message":message}}),
        )
    }
}

/// `None` leaves reply validation to the existing client-id decoder.
pub fn server_request(
    bounds: McpBounds,
    frame: &[u8],
) -> Result<Option<ServerRequest>, McpProtocolError> {
    if frame.len() > bounds.max_frame_bytes() {
        return Err(McpProtocolError::FrameTooLarge {
            limit: bounds.max_frame_bytes(),
            offered: frame.len(),
        });
    }
    let value: Value = serde_json::from_slice(frame)
        .map_err(|error| McpProtocolError::Malformed(error.to_string()))?;
    let object = value.as_object().ok_or(McpProtocolError::NotAnObject)?;
    let Some(method) = object.get("method") else {
        return Ok(None);
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpProtocolError::WrongVersion("server request".into()));
    }
    if object.contains_key("result") || object.contains_key("error") {
        return Err(McpProtocolError::Malformed(
            "server request also carries a reply".into(),
        ));
    }
    let method = method
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .ok_or_else(|| McpProtocolError::Malformed("server request method".into()))?;
    let id = object
        .get("id")
        .filter(|id| match id {
            Value::String(s) => !s.is_empty() && s.len() <= 512,
            Value::Number(n) => n.is_i64() || n.is_u64(),
            _ => false,
        })
        .ok_or(McpProtocolError::UnusableId)?;
    let params = match object.get("params") {
        None => json!({}),
        Some(value) if value.is_object() => value.clone(),
        Some(_) => {
            return Err(McpProtocolError::Malformed(
                "server request parameters must be an object".into(),
            ))
        }
    };
    Ok(Some(ServerRequest {
        id: id.clone(),
        method: method.into(),
        params,
    }))
}

/// Validated notification. Unknown notification methods have no reply.
pub fn notification(bounds: McpBounds, frame: &[u8]) -> Result<Option<Value>, McpProtocolError> {
    if frame.len() > bounds.max_frame_bytes() {
        return Err(McpProtocolError::FrameTooLarge {
            limit: bounds.max_frame_bytes(),
            offered: frame.len(),
        });
    }
    let value: Value = serde_json::from_slice(frame)
        .map_err(|error| McpProtocolError::Malformed(error.to_string()))?;
    let object = value.as_object().ok_or(McpProtocolError::NotAnObject)?;
    if object.contains_key("id") || !object.contains_key("method") {
        return Ok(None);
    }
    if value["jsonrpc"] != "2.0"
        || value["method"].as_str().is_none()
        || object.contains_key("result")
        || object.contains_key("error")
    {
        return Err(McpProtocolError::Malformed(
            "invalid server notification".into(),
        ));
    }
    Ok(Some(value))
}

fn encode(bounds: McpBounds, value: Value) -> Result<Vec<u8>, McpProtocolError> {
    // Write through a capped sink: refused output cannot allocate beyond the frame bound.
    struct Frame {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Frame {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("MCP response exceeds frame bound"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut frame = Frame {
        bytes: Vec::new(),
        limit: bounds.max_frame_bytes(),
    };
    serde_json::to_writer(&mut frame, &value)
        .map_err(|error| McpProtocolError::Malformed(error.to_string()))?;
    Ok(frame.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_id_space_preserves_string_and_signed_integer_ids(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let bounds = McpBounds::conservative()?;
        for id in [json!("1"), json!(1), json!(-1)] {
            let frame = json!({"jsonrpc":"2.0","id":id,"method":"elicitation/create","params":{}});
            let request =
                server_request(bounds, &serde_json::to_vec(&frame)?)?.ok_or("request absent")?;
            let response: Value =
                serde_json::from_slice(&request.result(bounds, json!({"action":"cancel"}))?)?;
            assert_eq!(response["id"], id);
        }
        Ok(())
    }
    #[test]
    fn malformed_requests_cannot_be_treated_as_tool_replies(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let bounds = McpBounds::conservative()?;
        for frame in [
            json!({"jsonrpc":"2.0","id":null,"method":"elicitation/create"}),
            json!({"jsonrpc":"2.0","id":1,"method":"elicitation/create","result":{}}),
            json!({"jsonrpc":"2.0","id":1.5,"method":"elicitation/create"}),
        ] {
            assert!(server_request(bounds, &serde_json::to_vec(&frame)?).is_err());
        }
        assert!(server_request(bounds, br#"{"jsonrpc":"2.0","id":1,"result":{}}"#)?.is_none());
        Ok(())
    }
}
