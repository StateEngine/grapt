//! Bounded, incremental server-sent-event reassembly — provider-neutral.
//!
//! Shared with the ChatGPT/Codex route. SSE framing is a transport
//! property, not a vendor one: the OpenAI Responses route, Anthropic Messages
//! and the OpenAI-compatible chat-completions routes all deliver
//! `data:`-framed events over the same wire rules. The two defects this
//! decoder's history records — a code point split across a chunk boundary
//! and a CRLF-delimited frame — are transport defects every one of those
//! routes would otherwise have to rediscover independently.
//!
//! What each adapter still owns is what a decoded payload MEANS. This module
//! hands back `data:` payload strings and nothing more; it parses no JSON and
//! knows no event name.

use crate::fragment::AdapterState;

// ---------------------------------------------------------------------------
// SSE decoding
// ---------------------------------------------------------------------------

/// Why the decoder refused a stream. Distinguished because they are different
/// facts about the provider: one is bytes that are not text, the other is more
/// bytes than the capture bound allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SseDecodeError {
    /// A genuinely malformed UTF-8 sequence, or a partial code point still
    /// outstanding when the stream ended.
    NotUtf8,
    /// The capture bound would have been exceeded.
    Oversized,
}

/// Bounded, **incremental** server-sent-event reassembly.
///
/// # Decoding is incremental across transport chunks
///
/// An HTTP chunk boundary is not a code-point boundary, so a two-byte `é`
/// split across two chunks must not be refused as malformed provider output.
/// Decoding each chunk independently would do exactly that on an ordinary
/// transport event, and a loopback fixture set that never splits a character
/// cannot catch it.
///
/// The decoder carries the incomplete tail — **at most three bytes**, which
/// is the longest possible prefix of a UTF-8 sequence that is not yet a
/// character — into the next chunk. `error_len()` is what separates the two
/// cases: `Some(_)` is a genuinely invalid sequence and refuses immediately,
/// `None` is an incomplete tail and is carried. A tail still outstanding when
/// the stream ends is `SseDecodeError::NotUtf8` via `SseDecoder::finish`,
/// not a silent truncation.
///
/// # Both LF and CRLF event delimiters, including split delimiters
///
/// An event boundary is any two consecutive line terminators, where a
/// terminator is `\r\n`, `\n` or a lone `\r`. Searching only for `"\n\n"`
/// would buffer a standards-valid `data: ...\r\n\r\n` frame until EOF and
/// surface it as a transport failure. A delimiter split across chunks needs no
/// special case: the scan simply finds no boundary until the remaining bytes
/// arrive. The one trap that does need care is a trailing lone `\r` at the end
/// of the buffer — it may still become `\r\n` — so it is never consumed as a
/// terminator until more bytes or the end of the stream decide it.
///
/// # The bound
///
/// Applied **before** growth to the pending frame plus its UTF-8 carry.
/// Complete events release room before another bounded slice of the same read
/// is appended, so coalescing valid events cannot make a stream oversized.
/// The carry remains structurally bounded at three bytes.
#[derive(Debug)]
pub(crate) struct SseDecoder {
    buffer: String,
    /// The incomplete UTF-8 tail of the previous chunk. Never longer than three
    /// bytes, which `debug_assert` and `carry_never_exceeds_three_bytes` both
    /// state.
    pub(crate) carry: Vec<u8>,
    failed: Option<SseDecodeError>,
    /// Only the stream's first decoded character can be a byte order mark.
    at_start: bool,
}

impl SseDecoder {
    /// The longest prefix of a UTF-8 sequence that is not yet a character: a
    /// four-byte sequence missing its final byte.
    pub(crate) const MAX_CARRY: usize = 3;

    pub(crate) fn new() -> Self {
        Self {
            buffer: String::new(),
            carry: Vec::new(),
            failed: None,
            at_start: true,
        }
    }

    /// Feed one transport chunk; returns the `data:` payloads of every complete
    /// event the buffer now holds.
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Result<Vec<String>, SseDecodeError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let mut remaining = chunk;
        let mut payloads = Vec::new();
        while !remaining.is_empty() {
            // A final CR can be resolved by looking at the next byte without
            // retaining it. This matters when that delimiter exactly fills
            // the frame bound: a following non-LF starts a new valid frame.
            if self.buffer.len() + self.carry.len() == AdapterState::MAX_BYTES {
                self.extract(&mut payloads, remaining[0] != b'\n');
            }
            let room = AdapterState::MAX_BYTES - self.buffer.len() - self.carry.len();
            if room == 0 {
                return Err(self.fail(SseDecodeError::Oversized));
            }
            let take = remaining.len().min(room).min(64 * 1024);
            let (slice, rest) = remaining.split_at(take);
            self.append(slice)?;
            self.extract(&mut payloads, false);
            remaining = rest;
        }
        Ok(payloads)
    }

    /// The caller limits this slice to the remaining pending-frame capacity.
    fn append(&mut self, chunk: &[u8]) -> Result<(), SseDecodeError> {
        // Splice the carried tail in front of the new bytes. The carry is at
        // most three bytes, so this is a bounded copy, not a growing buffer.
        let combined: Vec<u8> = if self.carry.is_empty() {
            chunk.to_vec()
        } else {
            let mut combined = std::mem::take(&mut self.carry);
            combined.extend_from_slice(chunk);
            combined
        };
        self.carry.clear();

        match std::str::from_utf8(&combined) {
            Ok(text) => self.append_text(text),
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                match std::str::from_utf8(&combined[..valid_up_to]) {
                    Ok(text) => self.append_text(text),
                    // Unreachable by `from_utf8`'s contract; refusing rather
                    // than unwrapping keeps the no-panic property total.
                    Err(_) => return Err(self.fail(SseDecodeError::NotUtf8)),
                }
                if error.error_len().is_some() {
                    // A real malformed sequence, not a boundary.
                    return Err(self.fail(SseDecodeError::NotUtf8));
                }
                let tail = &combined[valid_up_to..];
                if tail.len() > Self::MAX_CARRY {
                    // Cannot happen for a genuinely incomplete sequence; treat
                    // it as malformed rather than carrying an unbounded tail.
                    return Err(self.fail(SseDecodeError::NotUtf8));
                }
                self.carry.extend_from_slice(tail);
                debug_assert!(self.carry.len() <= Self::MAX_CARRY);
            }
        }

        Ok(())
    }

    fn append_text(&mut self, mut text: &str) {
        // An incomplete UTF-8 prefix must not consume the stream-start check.
        if self.at_start && !text.is_empty() {
            self.at_start = false;
            text = text.strip_prefix('\u{feff}').unwrap_or(text);
        }
        // Geometric String growth could allocate above the pending-frame cap
        // when a sliced UTF-8 character leaves capacity just below it.
        self.buffer.reserve_exact(text.len());
        self.buffer.push_str(text);
    }

    fn extract(&mut self, payloads: &mut Vec<String>, trailing_cr_decided: bool) {
        while let Some((frame_end, next_start)) =
            Self::event_boundary(&self.buffer, trailing_cr_decided)
        {
            let frame = self.buffer[..frame_end].to_owned();
            self.buffer.drain(..next_start);
            if let Some(data) = Self::event_data(&frame) {
                payloads.push(data);
            }
        }
    }

    /// The stream ended.
    ///
    /// Two things become decidable only here:
    ///
    /// 1. a carried UTF-8 tail is now an incomplete code point at EOF, which is
    ///    a truthful refusal rather than a silently dropped byte; and
    /// 2. a **trailing lone carriage return** is no longer undecided. While
    ///    bytes could still arrive it might have become CRLF, so
    ///    terminator_at withheld it; at EOF nothing more can arrive, so a
    ///    frame ending in two carriage returns is complete and must frame
    ///    exactly once. Returning Ok(()) with no payload would substitute
    ///    absence for a valid event — precisely the failure mode this crate
    ///    exists to refuse elsewhere.
    ///
    /// Returns any payloads that only end-of-stream could resolve.
    pub(crate) fn finish(&mut self) -> Result<Vec<String>, SseDecodeError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if !self.carry.is_empty() {
            return Err(self.fail(SseDecodeError::NotUtf8));
        }
        let mut payloads = Vec::new();
        while let Some((frame_end, next_start)) = Self::event_boundary(&self.buffer, true) {
            let frame = self.buffer[..frame_end].to_owned();
            self.buffer.drain(..next_start);
            if let Some(data) = Self::event_data(&frame) {
                payloads.push(data);
            }
        }
        Ok(payloads)
    }

    fn fail(&mut self, error: SseDecodeError) -> SseDecodeError {
        self.failed = Some(error);
        self.buffer.clear();
        self.carry.clear();
        error
    }

    /// The length of the line terminator at `index`, if one starts there.
    ///
    /// `\r\n` is two bytes, `\n` and a lone `\r` are one. A lone `\r` at the
    /// very end of the buffer returns `None`: it is undecided, because the next
    /// chunk may complete it into `\r\n`.
    fn terminator_at(text: &str, index: usize, at_eof: bool) -> Option<usize> {
        let bytes = text.as_bytes();
        match bytes.get(index) {
            Some(b'\n') => Some(1),
            Some(b'\r') => match bytes.get(index + 1) {
                Some(b'\n') => Some(2),
                Some(_) => Some(1),
                // Undecided while more bytes may arrive; decided at EOF:
                // nothing more can turn this CR into a CRLF, so a frame that
                // ends with two carriage returns is complete and frames once.
                None => at_eof.then_some(1),
            },
            _ => None,
        }
    }

    /// Find the first event boundary: two consecutive line terminators.
    ///
    /// Returns `(end of the frame's content, start of the next frame)`.
    fn event_boundary(text: &str, at_eof: bool) -> Option<(usize, usize)> {
        let mut index = 0;
        while index < text.len() {
            let Some(first) = Self::terminator_at(text, index, at_eof) else {
                index += 1;
                continue;
            };
            let after_first = index + first;
            match Self::terminator_at(text, after_first, at_eof) {
                Some(second) => return Some((index, after_first + second)),
                None => {
                    index = after_first;
                }
            }
        }
        None
    }

    /// Concatenate a frame's `data:` fields, per the SSE field rules.
    fn event_data(frame: &str) -> Option<String> {
        let mut data = String::new();
        for line in frame.split(['\n', '\r']) {
            if let Some(rest) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            }
        }
        if data.is_empty() || data == "[DONE]" {
            None
        } else {
            Some(data)
        }
    }
}

#[cfg(test)]
mod tests;
