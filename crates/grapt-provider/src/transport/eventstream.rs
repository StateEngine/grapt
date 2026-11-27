//! Bounded AWS EventStream framing for native Converse.
//! License and attribution notices are retained in THIRD_PARTY_NOTICES.md.
use serde_json::{json, Value};

#[derive(Debug, Default)]
pub(crate) struct EventStream {
    buffer: Vec<u8>,
}
impl EventStream {
    pub(crate) fn feed(&mut self, mut bytes: &[u8]) -> Result<Vec<String>, &'static str> {
        let mut events = Vec::new();
        while !bytes.is_empty() {
            // Buffer only one frame, validating its prelude before its body grows.
            let target = if self.buffer.len() < 12 {
                12
            } else {
                self.frame_length()?
            };
            let take = bytes.len().min(target - self.buffer.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buffer.len() >= 12 {
                let length = self.frame_length()?;
                if self.buffer.len() == length {
                    events.push(decode(&self.buffer)?);
                    self.buffer.clear();
                }
            }
        }
        Ok(events)
    }
    fn frame_length(&self) -> Result<usize, &'static str> {
        let length = number(&self.buffer[..4])? as usize;
        let headers = number(&self.buffer[4..8])? as usize;
        if !(16..=crate::AdapterState::MAX_BYTES).contains(&length)
            || headers > length - 16
            || headers > 16 * 1024
        {
            return Err("Bedrock event frame exceeds its byte bound");
        }
        if number(&self.buffer[8..12])? != crc32(&self.buffer[..8]) {
            return Err("Bedrock event prelude checksum failed");
        }
        Ok(length)
    }
    pub(crate) fn finish(&self) -> Result<Vec<String>, &'static str> {
        if self.buffer.is_empty() {
            Ok(Vec::new())
        } else {
            Err("Bedrock event stream ended mid-frame")
        }
    }
}
fn number(bytes: &[u8]) -> Result<u32, &'static str> {
    Ok(u32::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| "invalid Bedrock event integer")?,
    ))
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}
fn decode(frame: &[u8]) -> Result<String, &'static str> {
    let end = frame.len() - 4;
    if number(&frame[end..])? != crc32(&frame[..end]) {
        return Err("Bedrock event message checksum failed");
    }
    let headers_end = 12 + number(&frame[4..8])? as usize;
    let mut headers = &frame[12..headers_end];
    let (mut kind, mut event) = (None, None);
    while !headers.is_empty() {
        let name_len = usize::from(headers[0]);
        let name = std::str::from_utf8(
            headers
                .get(1..1 + name_len)
                .ok_or("truncated Bedrock event header")?,
        )
        .map_err(|_| "invalid Bedrock event header name")?;
        headers = headers
            .get(1 + name_len..)
            .ok_or("truncated Bedrock event header")?;
        let tag = *headers
            .first()
            .ok_or("truncated Bedrock event header type")?;
        headers = &headers[1..];
        let size = match tag {
            0 | 1 => 0,
            2 => 1,
            3 => 2,
            4 => 4,
            5 | 8 => 8,
            9 => 16,
            6 | 7 => {
                let length = u16::from_be_bytes(
                    headers
                        .get(..2)
                        .ok_or("truncated Bedrock event header length")?
                        .try_into()
                        .map_err(|_| "invalid Bedrock event header length")?,
                );
                headers = &headers[2..];
                usize::from(length)
            }
            _ => return Err("unknown Bedrock event header type"),
        };
        let value = headers
            .get(..size)
            .ok_or("truncated Bedrock event header value")?;
        if matches!(name, ":message-type" | ":event-type" | ":exception-type") {
            if tag != 7 {
                return Err("Bedrock event routing header must be text");
            }
            let value =
                std::str::from_utf8(value).map_err(|_| "invalid Bedrock event routing header")?;
            if name == ":message-type" {
                if kind.replace(value).is_some() {
                    return Err("duplicate Bedrock message type");
                }
            } else if event.replace(value).is_some() {
                return Err("duplicate Bedrock event type");
            }
        }
        headers = &headers[size..];
    }
    let body: Value = serde_json::from_slice(&frame[headers_end..end])
        .map_err(|_| "invalid Bedrock event payload")?;
    if !body.is_object() {
        return Err("Bedrock event payload must be an object");
    }
    match kind {
        Some("event") => Ok(json!({event.ok_or("missing Bedrock event type")?:body}).to_string()),
        Some("exception" | "error") => {
            let mut payload = serde_json::Map::new();
            payload.insert("type".to_owned(), json!(event.unwrap_or("unknown")));
            if let Some(message) = body
                .get("message")
                .or_else(|| body.get("Message"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty())
            {
                payload.insert("message".to_owned(), json!(message));
            }
            Ok(json!({"exception": payload}).to_string())
        }
        _ => Err("unknown Bedrock event message type"),
    }
}
