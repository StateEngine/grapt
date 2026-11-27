//! Incremental SSE framing with retained event IDs for HTTP resumption.
//!
//! Follows WHATWG event-stream line rules. Data is delivered one event at a
//! time, so coalesced transport chunks cannot allocate an unbounded result list.

use std::fmt;
use std::time::Duration;

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_EVENT_ID_BYTES: usize = 1024;
const MAX_EVENT_TYPE_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseDecodeError {
    InvalidLimit,
    Oversized,
    NotUtf8,
    Truncated,
}

impl fmt::Display for SseDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidLimit => "MCP SSE frame limit must be between 1 byte and 1 MiB",
            Self::Oversized => "MCP SSE line, data, or metadata exceeds its bound",
            Self::NotUtf8 => "MCP SSE stream is not valid UTF-8",
            Self::Truncated => "MCP SSE stream ended inside an event",
        })
    }
}

impl std::error::Error for SseDecodeError {}

#[derive(Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub data: String,
    pub event_type: String,
    pub id: Option<String>,
}

/// A poisoned decoder continues returning its original error.
#[derive(Debug)]
pub struct SseDecoder {
    limit: usize,
    line: Vec<u8>,
    data: String,
    has_data: bool,
    event_type: String,
    event_id: Option<String>,
    last_event_id: Option<String>,
    retry: Option<Duration>,
    after_cr: bool,
    first_line: bool,
    failed: Option<SseDecodeError>,
}

impl SseDecoder {
    pub fn new(max_frame_bytes: usize) -> Result<Self, SseDecodeError> {
        if !(1..=MAX_FRAME_BYTES).contains(&max_frame_bytes) {
            return Err(SseDecodeError::InvalidLimit);
        }
        Ok(Self {
            limit: max_frame_bytes,
            line: Vec::new(),
            data: String::new(),
            has_data: false,
            event_type: String::new(),
            event_id: None,
            last_event_id: None,
            retry: None,
            after_cr: false,
            first_line: true,
            failed: None,
        })
    }

    pub fn last_event_id(&self) -> Option<&str> {
        self.last_event_id.as_deref()
    }

    pub fn retry_delay(&self) -> Option<Duration> {
        self.retry
    }

    /// Push one byte from any size transport chunk; drain each event before
    /// continuing. No transport chunk or queue is retained by this decoder.
    pub fn push(&mut self, byte: u8) -> Result<Option<SseEvent>, SseDecodeError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = self.push_inner(byte);
        if let Err(error) = result {
            self.failed = Some(error);
        }
        result
    }

    fn push_inner(&mut self, byte: u8) -> Result<Option<SseEvent>, SseDecodeError> {
        if self.after_cr && byte == b'\n' {
            self.after_cr = false;
            return Ok(None);
        }
        self.after_cr = byte == b'\r';
        if matches!(byte, b'\r' | b'\n') {
            return self.end_line();
        }
        // The prefix/field overhead is separately finite, even for comments
        // or unrecognized fields that never produce a data event.
        if self.line.len() >= self.limit + MAX_EVENT_TYPE_BYTES {
            return Err(SseDecodeError::Oversized);
        }
        if self.line.len() == self.line.capacity() {
            let capacity =
                (self.line.capacity().max(64) * 2).min(self.limit + MAX_EVENT_TYPE_BYTES);
            self.line.reserve_exact(capacity - self.line.len());
        }
        self.line.push(byte);
        Ok(None)
    }

    fn end_line(&mut self) -> Result<Option<SseEvent>, SseDecodeError> {
        let bytes = std::mem::take(&mut self.line);
        let text = std::str::from_utf8(&bytes).map_err(|_| SseDecodeError::NotUtf8)?;
        let text = if std::mem::replace(&mut self.first_line, false) {
            text.strip_prefix('\u{feff}').unwrap_or(text)
        } else {
            text
        };
        if text.is_empty() {
            self.last_event_id = self.event_id.clone();
            let event_type = std::mem::take(&mut self.event_type);
            if !std::mem::take(&mut self.has_data) {
                return Ok(None);
            }
            return Ok(Some(SseEvent {
                data: std::mem::take(&mut self.data),
                event_type: if event_type.is_empty() {
                    "message".to_owned()
                } else {
                    event_type
                },
                id: self.last_event_id.clone(),
            }));
        }
        if text.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = text.split_once(':').unwrap_or((text, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                let separator = usize::from(self.has_data);
                let additional = separator + value.len();
                if additional > self.limit.saturating_sub(self.data.len()) {
                    return Err(SseDecodeError::Oversized);
                }
                let needed = self.data.len() + additional;
                if needed > self.data.capacity() {
                    let capacity = needed.max(self.data.capacity().max(64) * 2).min(self.limit);
                    self.data.reserve_exact(capacity - self.data.len());
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "event" => {
                if value.len() > MAX_EVENT_TYPE_BYTES {
                    return Err(SseDecodeError::Oversized);
                }
                self.event_type = value.to_owned();
            }
            "id" if !value.contains('\0') => {
                if value.len() > MAX_EVENT_ID_BYTES {
                    return Err(SseDecodeError::Oversized);
                }
                self.event_id = Some(value.to_owned());
            }
            "retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                if let Ok(milliseconds) = value.parse::<u64>() {
                    self.retry = Some(Duration::from_millis(milliseconds));
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// EOF cannot turn an unfinished event into an acknowledged message.
    pub fn finish(&mut self) -> Result<(), SseDecodeError> {
        let result = if let Some(error) = self.failed {
            Err(error)
        } else if std::str::from_utf8(&self.line).is_err() {
            Err(SseDecodeError::NotUtf8)
        } else if !self.line.is_empty() || self.has_data || !self.event_type.is_empty() {
            Err(SseDecodeError::Truncated)
        } else {
            Ok(())
        };
        if let Err(error) = result {
            self.failed = Some(error);
        }
        result
    }
}
