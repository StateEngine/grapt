//! Bounded HTTP/1.1 header parsing, incremental `Transfer-Encoding: chunked`
//! decoding, and LF/CRLF SSE frame boundary detection for [`crate::ChaosProxy`].

/// Maximum upstream response header bytes buffered before rejecting as oversized.
pub const MAX_RESPONSE_HEADER_BYTES: usize = 64 * 1024;

const MAX_CHUNK_SIZE_LINE_BYTES: usize = 1024;

/// Find the byte offset immediately after `\r\n\r\n`, if present.
#[must_use]
pub fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

/// Extract `(method, path)` from an HTTP/1.1 request line.
#[must_use]
pub fn parse_request_line(buf: &[u8]) -> (String, String) {
    let header_text = String::from_utf8_lossy(buf);
    let first_line = header_text.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_owned();
    let path = parts.next().unwrap_or("/").to_owned();
    (method, path)
}

/// Parse `Content-Length` from raw HTTP headers, defaulting to `0`.
#[must_use]
pub fn parse_content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers);
    for line in text.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                if let Ok(len) = value.trim().parse::<usize>() {
                    return len;
                }
            }
        }
    }
    0
}

/// Return `true` if the raw HTTP headers declare `Transfer-Encoding: chunked`.
#[must_use]
pub fn is_chunked_transfer_encoding(headers: &[u8]) -> bool {
    let text = String::from_utf8_lossy(headers);
    for line in text.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("transfer-encoding")
                && value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("chunked"))
            {
                return true;
            }
        }
    }
    false
}

/// Format a synthetic HTTP/1.1 error response with `Connection: close`.
#[must_use]
pub fn build_synthetic_http_response(
    status: u16,
    retry_after_secs: Option<u64>,
    body: &str,
) -> Vec<u8> {
    let reason = match status {
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        529 => "Site Is Overloaded",
        _ => "Chaos Injected Error",
    };
    let mut out = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(secs) = retry_after_secs {
        out.push_str(&format!("Retry-After: {secs}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    out.into_bytes()
}

/// Wrap `payload` as one HTTP/1.1 chunked-transfer frame (`<hex>\r\n<payload>\r\n`).
#[must_use]
pub fn encode_http_chunk(payload: &[u8]) -> Vec<u8> {
    let header = format!("{:x}\r\n", payload.len());
    let mut out = Vec::with_capacity(header.len() + payload.len() + 2);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\r\n");
    out
}

/// Result of feeding one byte into [`SseFrameScanner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseBoundary {
    /// No frame boundary completed at this byte.
    None,
    /// A frame delimiter completed on the current byte (include this byte).
    CompleteAtCurrent,
    /// A standalone CR blank line completed on the previous byte (exclude this byte).
    CompleteBeforeCurrent,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum LineState {
    #[default]
    Content,
    AfterContentCr,
    AtLineStart,
    AtBlankLineCr,
}

/// Incremental WHATWG SSE event boundary detector supporting LF (`\n`),
/// CRLF (`\r\n`), and standalone CR (`\r`) line endings across arbitrary chunk splits.
#[derive(Debug, Default)]
pub struct SseFrameScanner {
    state: LineState,
}

impl SseFrameScanner {
    /// Feed one logical body byte and report whether an SSE event boundary completed.
    pub fn push_byte(&mut self, byte: u8) -> SseBoundary {
        match (self.state, byte) {
            (LineState::Content, b'\n') => {
                self.state = LineState::AtLineStart;
                SseBoundary::None
            }
            (LineState::Content, b'\r') => {
                self.state = LineState::AfterContentCr;
                SseBoundary::None
            }
            (LineState::Content, _) => SseBoundary::None,
            (LineState::AfterContentCr, b'\n') => {
                self.state = LineState::AtLineStart;
                SseBoundary::None
            }
            (LineState::AfterContentCr, b'\r') => {
                self.state = LineState::AtBlankLineCr;
                SseBoundary::None
            }
            (LineState::AfterContentCr, _) => {
                self.state = LineState::Content;
                SseBoundary::None
            }
            (LineState::AtLineStart, b'\n') => {
                self.state = LineState::Content;
                SseBoundary::CompleteAtCurrent
            }
            (LineState::AtLineStart, b'\r') => {
                self.state = LineState::AtBlankLineCr;
                SseBoundary::None
            }
            (LineState::AtLineStart, _) => {
                self.state = LineState::Content;
                SseBoundary::None
            }
            (LineState::AtBlankLineCr, b'\n') => {
                self.state = LineState::Content;
                SseBoundary::CompleteAtCurrent
            }
            (LineState::AtBlankLineCr, b'\r') => {
                self.state = LineState::AtBlankLineCr;
                SseBoundary::CompleteBeforeCurrent
            }
            (LineState::AtBlankLineCr, _) => {
                self.state = LineState::Content;
                SseBoundary::CompleteBeforeCurrent
            }
        }
    }
}

/// Result of feeding a wire slice into [`ChunkedDecoder`].
#[derive(Debug, PartialEq, Eq)]
pub struct DecodedChunks {
    pub payload: Vec<u8>,
    pub finished: bool,
    pub malformed: bool,
}

#[derive(Debug)]
enum ChunkPhase {
    SizeLine(Vec<u8>),
    Data { remaining: usize },
    DataCrLf { seen_cr: bool },
    Trailers { line_len: usize, seen_cr: bool },
    Done,
}

/// Incremental HTTP/1.1 `Transfer-Encoding: chunked` body decoder.
#[derive(Debug)]
pub struct ChunkedDecoder {
    phase: ChunkPhase,
}

impl Default for ChunkedDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkedDecoder {
    /// Create a new decoder positioned at the first chunk size line.
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase: ChunkPhase::SizeLine(Vec::new()),
        }
    }

    /// Decode `wire` bytes incrementally into logical body bytes.
    pub fn decode(&mut self, wire: &[u8]) -> DecodedChunks {
        let mut payload = Vec::new();
        let mut idx = 0;
        while idx < wire.len() {
            match &mut self.phase {
                ChunkPhase::SizeLine(buf) => {
                    buf.push(wire[idx]);
                    idx += 1;
                    if buf.len() > MAX_CHUNK_SIZE_LINE_BYTES {
                        return DecodedChunks {
                            payload,
                            finished: false,
                            malformed: true,
                        };
                    }
                    if buf.ends_with(b"\r\n") {
                        let line = String::from_utf8_lossy(&buf[..buf.len() - 2]);
                        let hex = line.split(';').next().unwrap_or("").trim();
                        let Ok(size) = usize::from_str_radix(hex, 16) else {
                            return DecodedChunks {
                                payload,
                                finished: false,
                                malformed: true,
                            };
                        };
                        self.phase = if size == 0 {
                            ChunkPhase::Trailers {
                                line_len: 0,
                                seen_cr: false,
                            }
                        } else {
                            ChunkPhase::Data { remaining: size }
                        };
                    }
                }
                ChunkPhase::Data { remaining } => {
                    let take = (*remaining).min(wire.len() - idx);
                    payload.extend_from_slice(&wire[idx..idx + take]);
                    idx += take;
                    *remaining -= take;
                    if *remaining == 0 {
                        self.phase = ChunkPhase::DataCrLf { seen_cr: false };
                    }
                }
                ChunkPhase::DataCrLf { seen_cr } => {
                    let byte = wire[idx];
                    idx += 1;
                    if !*seen_cr && byte == b'\r' {
                        *seen_cr = true;
                    } else if byte == b'\n' {
                        self.phase = ChunkPhase::SizeLine(Vec::new());
                    } else {
                        return DecodedChunks {
                            payload,
                            finished: false,
                            malformed: true,
                        };
                    }
                }
                ChunkPhase::Trailers { line_len, seen_cr } => {
                    let byte = wire[idx];
                    idx += 1;
                    if byte == b'\r' {
                        *seen_cr = true;
                    } else if byte == b'\n' && *seen_cr {
                        if *line_len == 0 {
                            self.phase = ChunkPhase::Done;
                        } else {
                            *line_len = 0;
                            *seen_cr = false;
                        }
                    } else {
                        *line_len += 1;
                        *seen_cr = false;
                    }
                }
                ChunkPhase::Done => break,
            }
        }
        DecodedChunks {
            payload,
            finished: matches!(self.phase, ChunkPhase::Done),
            malformed: false,
        }
    }
}
