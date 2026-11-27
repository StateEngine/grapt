//! Reply correlation precedes payload handling; private error data survives decoding.
use crate::{elicitation::UrlRequirements, McpBounds, McpProtocolError, RequestId};
use serde_json::Value;

/// Bounded by the incoming frame; never formatted into diagnostics or model results.
#[derive(Clone, PartialEq, Eq)]
pub struct ServerErrorData(Value);

impl ServerErrorData {
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

impl std::fmt::Debug for ServerErrorData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerErrorData([private])")
    }
}

/// Decode a reply only after validating its exact client-owned correlation ID.
pub fn decode_reply(
    bounds: McpBounds,
    expected: RequestId,
    frame: &[u8],
) -> Result<Value, McpProtocolError> {
    if frame.len() > bounds.max_frame_bytes() {
        return Err(McpProtocolError::FrameTooLarge {
            limit: bounds.max_frame_bytes(),
            offered: frame.len(),
        });
    }
    let value: Value = serde_json::from_slice(frame)
        .map_err(|error| McpProtocolError::Malformed(error.to_string()))?;
    let object = value.as_object().ok_or(McpProtocolError::NotAnObject)?;
    match object.get("jsonrpc").and_then(Value::as_str) {
        Some("2.0") => {}
        other => {
            return Err(McpProtocolError::WrongVersion(
                other.unwrap_or_default().to_owned(),
            ))
        }
    }
    let replied = object
        .get("id")
        .and_then(Value::as_u64)
        .ok_or(McpProtocolError::UnusableId)?;
    if replied != expected.value() {
        return Err(McpProtocolError::CorrelationMismatch {
            expected: expected.value(),
            replied,
        });
    }
    match (object.get("result"), object.get("error")) {
        (Some(_), Some(_)) => Err(McpProtocolError::BothResultAndError),
        (None, None) => Err(McpProtocolError::NeitherResultNorError),
        (None, Some(error)) => {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            if code == -32042 {
                // Never expose this error's message or URL material through Display.
                let data = error.get("data").ok_or_else(|| {
                    McpProtocolError::Malformed("URL-required error lacks data".into())
                })?;
                return Err(McpProtocolError::UrlElicitationRequired(
                    UrlRequirements::parse(data).map_err(McpProtocolError::Malformed)?,
                ));
            }
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the server gave no message")
                .to_owned();
            Err(McpProtocolError::ServerError {
                code,
                message,
                data: error.get("data").cloned().map(ServerErrorData),
            })
        }
        (Some(result), None) => Ok(result.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{initialize_request_with_elicitation, RequestSequence};
    use serde_json::json;

    #[test]
    fn initialization_advertises_only_composed_modes_and_url_required_data_is_private(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let bounds = McpBounds::conservative()?;
        let mut sequence = RequestSequence::new();
        for enabled in [false, true] {
            let request =
                initialize_request_with_elicitation(&mut sequence, bounds, "test", "1", enabled)?;
            let frame: Value = serde_json::from_slice(request.frame())?;
            assert_eq!(frame["params"]["protocolVersion"], "2025-11-25");
            assert_eq!(
                frame["params"]["capabilities"].get("elicitation").is_some(),
                enabled
            );
            if enabled {
                assert!(frame["params"]["capabilities"]["elicitation"]["url"].is_object());
            }
            let reply = json!({"jsonrpc":"2.0","id":request.id().value(),"error":{"code":-32042,"message":"secret URL","data":{"elicitations":[{"mode":"url","message":"Connect","url":"https://example.test/?secret=token","elicitationId":"external-9"}]}}});
            let error = decode_reply(bounds, request.id(), &serde_json::to_vec(&reply)?)
                .err()
                .ok_or("missing error")?;
            assert!(!error.to_string().contains("secret"));
            assert!(!format!("{error:?}").contains("secret"));
            let McpProtocolError::UrlElicitationRequired(requirements) = error else {
                return Err("wrong error".into());
            };
            assert_eq!(requirements.requests()[0].id().as_str(), "external-9");
            let reply = json!({"jsonrpc":"2.0","id":request.id().value(),"error":{"code":-32000,"message":"refused","data":{"private":"token"}}});
            let Err(McpProtocolError::ServerError {
                data: Some(data), ..
            }) = decode_reply(bounds, request.id(), &serde_json::to_vec(&reply)?)
            else {
                return Err("error data lost".into());
            };
            assert_eq!(data.as_value()["private"], "token");
        }
        Ok(())
    }
}
