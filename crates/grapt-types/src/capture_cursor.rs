//! Bounded, versioned, interest-bound durable capture cursor. Law: the cursor is
//! opaque and sealed to the exact interest it advances; it derives from accepted
//! aggregate-scoped event identity and monotonic-with-gaps sequences, and it
//! advances only over committed durable truth. Campaign: EVENT-STREAM-REPLAY-CURSOR-01
//! J02 (charter `docs/campaigns/EVENT-STREAM-REPLAY-CURSOR-01.md`).
//!
//! Split from `event.rs` under the 1,000-line god-file guard: the cursor codec and
//! its law tests live here; the durable event identities stay in `event`.

use std::cmp::Ordering;
use std::fmt;

use crate::event::{EventFencing, EventSeq, WriterEpoch};
use crate::session::SessionId;

/// The caller's interest in captured events: every durable aggregate, or one
/// session-list location. The interest is sealed into the cursor token, so a
/// page read under one interest cannot be resumed under another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureInterest {
    /// Every durable session aggregate, whatever its location.
    All,
    /// Sessions whose projection row lives in `directory`, optionally narrowed
    /// to one `workspace`; an absent workspace matches every workspace in the
    /// directory.
    Location {
        directory: String,
        workspace: Option<String>,
    },
}

impl CaptureInterest {
    /// Directory component bound for a [`CaptureInterest::Location`], in bytes.
    pub const MAX_CAPTURE_INTEREST_DIRECTORY_BYTES: usize = 1024;
    /// Workspace component bound for a [`CaptureInterest::Location`], in bytes.
    pub const MAX_CAPTURE_INTEREST_WORKSPACE_BYTES: usize = 256;

    /// Builds a location interest, refusing an empty or over-bound component
    /// before any token is minted from it.
    pub fn location(
        directory: String,
        workspace: Option<String>,
    ) -> Result<Self, CaptureInterestError> {
        if directory.is_empty() {
            return Err(CaptureInterestError::DirectoryEmpty);
        }
        if directory.len() > Self::MAX_CAPTURE_INTEREST_DIRECTORY_BYTES {
            return Err(CaptureInterestError::DirectoryTooLong);
        }
        if let Some(workspace) = &workspace {
            if workspace.is_empty() {
                return Err(CaptureInterestError::WorkspaceEmpty);
            }
            if workspace.len() > Self::MAX_CAPTURE_INTEREST_WORKSPACE_BYTES {
                return Err(CaptureInterestError::WorkspaceTooLong);
            }
        }
        Ok(Self::Location {
            directory,
            workspace,
        })
    }
}

/// A refused [`CaptureInterest::location`] component, named by the rule it
/// broke, so the caller can tell an empty value from an over-bound one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureInterestError {
    /// The directory component is the empty string.
    DirectoryEmpty,
    /// The directory component exceeds
    /// [`CaptureInterest::MAX_CAPTURE_INTEREST_DIRECTORY_BYTES`].
    DirectoryTooLong,
    /// The workspace component is the empty string.
    WorkspaceEmpty,
    /// The workspace component exceeds
    /// [`CaptureInterest::MAX_CAPTURE_INTEREST_WORKSPACE_BYTES`].
    WorkspaceTooLong,
}

impl fmt::Display for CaptureInterestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::DirectoryEmpty => "capture interest directory is empty",
            Self::DirectoryTooLong => "capture interest directory exceeds the byte bound",
            Self::WorkspaceEmpty => "capture interest workspace is empty",
            Self::WorkspaceTooLong => "capture interest workspace exceeds the byte bound",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CaptureInterestError {}

/// One aggregate's resume position: the last sequence it already received,
/// under the writer fencing it held when it was last served.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CaptureCoordinate {
    /// The session aggregate the position belongs to.
    pub aggregate: SessionId,
    /// The last sequence served to the reader for this aggregate.
    pub seq: EventSeq,
    /// The writer fencing held when the position was last served.
    pub fencing: EventFencing,
}

impl PartialOrd for CaptureCoordinate {
    /// Compares by `aggregate` only: a well-formed cursor holds at most one
    /// coordinate per aggregate, so no tie-break is ever consulted.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CaptureCoordinate {
    /// Orders by `aggregate` only: a well-formed cursor holds at most one
    /// coordinate per aggregate, so no tie-break is ever consulted.
    fn cmp(&self, other: &Self) -> Ordering {
        self.aggregate.cmp(&other.aggregate)
    }
}

/// Maximum coordinates in one cursor: the resume state must stay a small,
/// checkable document, never a growing ledger.
pub const MAX_CAPTURE_CURSOR_COORDINATES: usize = 1024;
/// Maximum encoded cursor token, in bytes: past this the token stops being a
/// query parameter and starts being a payload, so it is refused.
pub const MAX_CAPTURE_CURSOR_TOKEN_BYTES: usize = 64 * 1024;

/// The reader's durable resume state: an interest sealed to one location and
/// one coordinate per served aggregate. The encoded token is canonical — a
/// single strict grammar, exact key order, no whitespace — so a token either
/// round-trips to itself or is refused as non-canonical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureCursor {
    interest: CaptureInterest,
    coordinates: Vec<CaptureCoordinate>,
}

impl CaptureCursor {
    /// The resume state before anything has been served: the interest alone.
    pub fn start(interest: CaptureInterest) -> Self {
        Self {
            interest,
            coordinates: Vec::new(),
        }
    }

    /// Builds a cursor over named coordinates, refusing the shapes a token
    /// could not honestly carry: too many, unsorted, or a repeated aggregate.
    pub fn new(
        interest: CaptureInterest,
        coordinates: Vec<CaptureCoordinate>,
    ) -> Result<Self, CaptureCursorError> {
        if coordinates.len() > MAX_CAPTURE_CURSOR_COORDINATES {
            return Err(CaptureCursorError::Malformed {
                reason: "too many coordinates",
            });
        }
        for pair in coordinates.windows(2) {
            if pair[0].aggregate >= pair[1].aggregate {
                return Err(CaptureCursorError::Malformed {
                    reason: "coordinates are not strictly increasing by aggregate",
                });
            }
        }
        Ok(Self {
            interest,
            coordinates,
        })
    }

    /// The interest sealed into this cursor.
    pub fn interest(&self) -> &CaptureInterest {
        &self.interest
    }

    /// The coordinates in aggregate order.
    pub fn coordinates(&self) -> &[CaptureCoordinate] {
        &self.coordinates
    }

    /// The canonical token: [`Self::decode`] of [`Self::encode`] is the
    /// identity on every well-formed cursor, so tokens are comparable.
    pub fn encode(&self) -> String {
        token::encode(self)
    }

    /// Decodes a token sealed to `interest`. A token that does not round-trip
    /// to itself, or that names a different interest, is refused with the
    /// named reason — a replay reader must never guess which stream it is on.
    pub fn decode(
        token_text: &str,
        interest: &CaptureInterest,
    ) -> Result<Self, CaptureCursorError> {
        if token_text.len() > MAX_CAPTURE_CURSOR_TOKEN_BYTES {
            return Err(CaptureCursorError::Malformed {
                reason: "token exceeds the byte bound",
            });
        }
        let payload = token::base64url_decode(token_text).ok_or_else(malformed)?;
        let document = token::parse_document(&payload).ok_or_else(malformed)?;
        if document.version != 1 {
            return Err(CaptureCursorError::WrongVersion {
                found: document.version,
            });
        }
        let mut coordinates = Vec::with_capacity(document.coordinates.len());
        for entry in document.coordinates {
            let aggregate = entry.aggregate.parse::<SessionId>().map_err(|_| {
                CaptureCursorError::Malformed {
                    reason: "coordinate aggregate is not a session identity",
                }
            })?;
            let fencing = if entry.epoch == 0 {
                EventFencing::LegacyUnfenced
            } else {
                let epoch = u32::try_from(entry.epoch)
                    .ok()
                    .and_then(WriterEpoch::new)
                    .ok_or(CaptureCursorError::Malformed {
                        reason: "coordinate epoch is not a valid writer epoch",
                    })?;
                EventFencing::Fenced { epoch }
            };
            coordinates.push(CaptureCoordinate {
                aggregate,
                seq: EventSeq::new(entry.seq),
                fencing,
            });
        }
        let cursor = Self::new(document.interest, coordinates)?;
        if cursor.interest != *interest {
            return Err(CaptureCursorError::WrongLocation);
        }
        if token::encode(&cursor) != token_text {
            return Err(CaptureCursorError::Malformed {
                reason: "non-canonical encoding",
            });
        }
        Ok(cursor)
    }
}

/// A refused cursor: the token or the resume state broke a named rule, and
/// the refusal names which one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureCursorError {
    /// The token or the coordinate list broke the grammar, with the named reason.
    Malformed { reason: &'static str },
    /// The token was well-formed but carries a schema version this reader does
    /// not read.
    WrongVersion { found: u64 },
    /// The token is sealed to a different interest than the one the read was
    /// made under.
    WrongLocation,
    /// A coordinate names a sequence past its aggregate's durable head.
    FutureCoordinate {
        aggregate: SessionId,
        seq: EventSeq,
        head: Option<EventSeq>,
    },
    /// A coordinate's aggregate no longer matches the read's interest.
    Stale { aggregate: SessionId },
}

impl fmt::Display for CaptureCursorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { reason } => {
                write!(formatter, "capture cursor is malformed: {reason}")
            }
            Self::WrongVersion { found } => {
                write!(formatter, "capture cursor version {found} is not version 1")
            }
            Self::WrongLocation => {
                formatter.write_str("capture cursor is bound to a different location")
            }
            Self::FutureCoordinate {
                aggregate,
                seq,
                head,
            } => match head {
                Some(head) => write!(
                    formatter,
                    "coordinate for {aggregate} at {} is past the durable head {}",
                    seq.value(),
                    head.value()
                ),
                None => write!(
                    formatter,
                    "coordinate for {aggregate} at {} names an aggregate with no durable head",
                    seq.value()
                ),
            },
            Self::Stale { aggregate } => write!(
                formatter,
                "coordinate for {aggregate} no longer matches the read location"
            ),
        }
    }
}

impl std::error::Error for CaptureCursorError {}

fn malformed() -> CaptureCursorError {
    CaptureCursorError::Malformed {
        reason: "token is not canonical",
    }
}

/// The canonical token grammar. A token is base64url (no padding) of a strict
/// JSON document: exact key order, no whitespace, canonical escapes. The
/// parser and the encoder are written against each other, so
/// `decode(encode(c))` is the identity on every well-formed cursor —
/// anything else is non-canonical and is refused.
mod token {
    use super::{CaptureCursor, CaptureInterest, EventFencing};

    /// The decoded token document, exactly as the canonical grammar carries it.
    pub(super) struct Document {
        pub(super) version: u64,
        pub(super) interest: CaptureInterest,
        pub(super) coordinates: Vec<Coordinate>,
    }

    /// One coordinate entry before it is typed into the cursor.
    pub(super) struct Coordinate {
        pub(super) aggregate: String,
        pub(super) seq: u64,
        pub(super) epoch: u64,
    }

    const ALPHABET: [u8; 64] = *b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    const HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";

    pub(super) fn base64url_decode(text: &str) -> Option<Vec<u8>> {
        let mut value: u32 = 0;
        let mut bits = 0usize;
        let mut out = Vec::new();
        for byte in text.bytes() {
            let index = ALPHABET.iter().position(|&entry| entry == byte)?;
            value = (value << 6) | index as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((value >> bits) as u8);
                // Keep only the unemitted bits: without the decay, value
                // outgrows a u32 after seven characters and the tail check
                // below would read wrapped bits.
                value &= (1u32 << bits) - 1;
            }
        }
        // Canonical base64url leaves no live bits in the trailing partial
        // group: a non-zero tail is padding this encoder never writes.
        if bits > 0 && value & ((1 << bits) - 1) != 0 {
            return None;
        }
        Some(out)
    }

    pub(super) fn parse_document(payload: &[u8]) -> Option<Document> {
        let mut parser = Parser {
            bytes: payload,
            pos: 0,
        };
        parser.expect(b'{')?;
        parser.member_key("version")?;
        let version = parser.uint()?;
        parser.expect(b',')?;
        parser.member_key("interest")?;
        let interest = parser.interest()?;
        parser.expect(b',')?;
        parser.member_key("coordinates")?;
        let coordinates = parser.coordinates()?;
        parser.expect(b'}')?;
        parser.end()?;
        Some(Document {
            version,
            interest,
            coordinates,
        })
    }

    pub(super) fn encode(cursor: &CaptureCursor) -> String {
        base64url_encode(encode_document(cursor).as_bytes())
    }

    fn encode_document(cursor: &CaptureCursor) -> String {
        let mut out = String::with_capacity(64);
        out.push_str("{\"version\":1,\"interest\":");
        encode_interest(cursor.interest(), &mut out);
        out.push_str(",\"coordinates\":[");
        for (index, coordinate) in cursor.coordinates().iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push('{');
            out.push_str("\"aggregate\":");
            encode_string(coordinate.aggregate.as_str(), &mut out);
            out.push_str(",\"seq\":");
            out.push_str(&coordinate.seq.value().to_string());
            out.push_str(",\"epoch\":");
            match coordinate.fencing {
                EventFencing::LegacyUnfenced => out.push('0'),
                EventFencing::Fenced { epoch } => out.push_str(&epoch.value().to_string()),
            }
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    fn encode_interest(interest: &CaptureInterest, out: &mut String) {
        match interest {
            CaptureInterest::All => out.push_str("\"all\""),
            CaptureInterest::Location {
                directory,
                workspace,
            } => {
                out.push_str("{\"directory\":");
                encode_string(directory, out);
                out.push_str(",\"workspace\":");
                match workspace {
                    Some(workspace) => encode_string(workspace, out),
                    None => out.push_str("null"),
                }
                out.push('}');
            }
        }
    }

    fn encode_string(text: &str, out: &mut String) {
        out.push('"');
        for character in text.chars() {
            match character {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                character if (character as u32) < 0x20 => {
                    let value = character as u32;
                    out.push_str("\\u00");
                    out.push(HEX_DIGITS[(value >> 4) as usize] as char);
                    out.push(HEX_DIGITS[(value & 0x0F) as usize] as char);
                }
                character => out.push(character),
            }
        }
        out.push('"');
    }

    fn base64url_encode(payload: &[u8]) -> String {
        let mut out = String::with_capacity(payload.len().div_ceil(3) * 4);
        let mut index = 0usize;
        while index + 3 <= payload.len() {
            let triple = (payload[index] as u32) << 16
                | (payload[index + 1] as u32) << 8
                | payload[index + 2] as u32;
            out.push(ALPHABET[(triple >> 18) as usize] as char);
            out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
            index += 3;
        }
        match payload.len() - index {
            1 => {
                let pair = (payload[index] as u32) << 16;
                out.push(ALPHABET[(pair >> 18) as usize] as char);
                out.push(ALPHABET[((pair >> 12) & 0x3F) as usize] as char);
            }
            2 => {
                let pair = (payload[index] as u32) << 16 | (payload[index + 1] as u32) << 8;
                out.push(ALPHABET[(pair >> 18) as usize] as char);
                out.push(ALPHABET[((pair >> 12) & 0x3F) as usize] as char);
                out.push(ALPHABET[((pair >> 6) & 0x3F) as usize] as char);
            }
            _ => {}
        }
        out
    }

    /// One strict pass over the canonical JSON document: exact member order,
    /// no whitespace, no duplicates, canonical escapes.
    struct Parser<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl<'a> Parser<'a> {
        fn peek(&self) -> Option<u8> {
            self.bytes.get(self.pos).copied()
        }

        fn advance(&mut self) -> Option<u8> {
            let byte = self.peek()?;
            self.pos += 1;
            Some(byte)
        }

        fn expect(&mut self, byte: u8) -> Option<()> {
            if self.peek()? == byte {
                self.pos += 1;
                Some(())
            } else {
                None
            }
        }

        fn end(&self) -> Option<()> {
            (self.pos == self.bytes.len()).then_some(())
        }

        fn literal(&mut self, text: &str) -> Option<()> {
            for expected in text.bytes() {
                if self.advance()? != expected {
                    return None;
                }
            }
            Some(())
        }

        /// Reads a `"quoted"` value; member keys and string values alike.
        fn string(&mut self) -> Option<String> {
            self.expect(b'"')?;
            let mut output = Vec::new();
            loop {
                let byte = self.advance()?;
                match byte {
                    b'"' => return String::from_utf8(output).ok(),
                    b'\\' => {
                        let escape = self.advance()?;
                        match escape {
                            b'"' => output.push(b'"'),
                            b'\\' => output.push(b'\\'),
                            b'/' => output.push(b'/'),
                            b'b' => output.push(0x08),
                            b'f' => output.push(0x0C),
                            b'n' => output.push(b'\n'),
                            b'r' => output.push(b'\r'),
                            b't' => output.push(b'\t'),
                            b'u' => {
                                let high = self.hex4()? as u32;
                                let code = if (0xD800..=0xDBFF).contains(&high) {
                                    self.expect(b'\\')?;
                                    self.expect(b'u')?;
                                    let low = self.hex4()? as u32;
                                    if !(0xDC00..=0xDFFF).contains(&low) {
                                        return None;
                                    }
                                    0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                                } else if (0xDC00..=0xDFFF).contains(&high) {
                                    // A low surrogate without its high half.
                                    return None;
                                } else {
                                    high
                                };
                                let mut buffer = [0u8; 4];
                                let scalar = char::from_u32(code)?;
                                output
                                    .extend_from_slice(scalar.encode_utf8(&mut buffer).as_bytes());
                            }
                            _ => return None,
                        }
                    }
                    0x00..=0x1F => return None,
                    _ => output.push(byte),
                }
            }
        }

        fn hex4(&mut self) -> Option<u16> {
            let mut value: u16 = 0;
            for _ in 0..4 {
                let digit = match self.advance()? {
                    byte @ b'0'..=b'9' => byte - b'0',
                    byte @ b'a'..=b'f' => byte - b'a' + 10,
                    byte @ b'A'..=b'F' => byte - b'A' + 10,
                    _ => return None,
                };
                value = value
                    .checked_mul(16)
                    .and_then(|scaled| scaled.checked_add(digit as u16))?;
            }
            Some(value)
        }

        /// `0` or a digit run without a leading zero; more than 20 digits or
        /// a value past `u64::MAX` is not canonical.
        fn uint(&mut self) -> Option<u64> {
            match self.peek()? {
                b'0' => {
                    self.pos += 1;
                    Some(0)
                }
                first @ b'1'..=b'9' => {
                    self.pos += 1;
                    let mut value: u128 = (first - b'0') as u128;
                    let mut digits = 1usize;
                    while digits < 20 {
                        match self.peek()? {
                            digit @ b'0'..=b'9' => {
                                self.pos += 1;
                                digits += 1;
                                value =
                                    value.checked_mul(10)?.checked_add((digit - b'0') as u128)?;
                                if value > u64::MAX as u128 {
                                    return None;
                                }
                            }
                            _ => return u64::try_from(value).ok(),
                        }
                    }
                    let over_long = self.peek().is_some_and(|next: u8| next.is_ascii_digit());
                    if over_long {
                        return None;
                    }
                    u64::try_from(value).ok()
                }
                _ => None,
            }
        }

        fn member_key(&mut self, expected: &str) -> Option<()> {
            let key = self.string()?;
            if key != expected {
                return None;
            }
            self.expect(b':')
        }

        fn interest(&mut self) -> Option<CaptureInterest> {
            match self.peek()? {
                b'"' => (self.string()? == "all").then_some(CaptureInterest::All),
                b'{' => self.location(),
                _ => None,
            }
        }

        fn location(&mut self) -> Option<CaptureInterest> {
            self.expect(b'{')?;
            self.member_key("directory")?;
            let directory = self.string()?;
            self.expect(b',')?;
            self.member_key("workspace")?;
            let workspace = match self.peek()? {
                b'n' => {
                    self.literal("null")?;
                    None
                }
                b'"' => Some(self.string()?),
                _ => None,
            };
            self.expect(b'}')?;
            Some(CaptureInterest::Location {
                directory,
                workspace,
            })
        }

        fn coordinates(&mut self) -> Option<Vec<Coordinate>> {
            self.expect(b'[')?;
            let mut coordinates = Vec::new();
            if self.peek()? != b']' {
                loop {
                    coordinates.push(self.coordinate()?);
                    match self.advance()? {
                        b',' => {}
                        b']' => break,
                        _ => return None,
                    }
                }
            } else {
                self.pos += 1;
            }
            Some(coordinates)
        }

        fn coordinate(&mut self) -> Option<Coordinate> {
            self.expect(b'{')?;
            self.member_key("aggregate")?;
            let aggregate = self.string()?;
            self.expect(b',')?;
            self.member_key("seq")?;
            let seq = self.uint()?;
            self.expect(b',')?;
            self.member_key("epoch")?;
            let epoch = self.uint()?;
            self.expect(b'}')?;
            Some(Coordinate {
                aggregate,
                seq,
                epoch,
            })
        }
    }
}

#[cfg(test)]
mod capture_cursor_tests {
    use super::*;

    fn b64url(text: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        let mut index = 0;
        while index + 3 <= text.len() {
            let triple =
                (text[index] as u32) << 16 | (text[index + 1] as u32) << 8 | text[index + 2] as u32;
            out.push(ALPHABET[(triple >> 18) as usize] as char);
            out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
            index += 3;
        }
        match text.len() - index {
            1 => {
                let pair = (text[index] as u32) << 16;
                out.push(ALPHABET[(pair >> 18) as usize] as char);
                out.push(ALPHABET[((pair >> 12) & 0x3F) as usize] as char);
            }
            2 => {
                let pair = (text[index] as u32) << 16 | (text[index + 1] as u32) << 8;
                out.push(ALPHABET[(pair >> 18) as usize] as char);
                out.push(ALPHABET[((pair >> 12) & 0x3F) as usize] as char);
                out.push(ALPHABET[((pair >> 6) & 0x3F) as usize] as char);
            }
            _ => {}
        }
        out
    }

    fn malformed() -> CaptureCursorError {
        CaptureCursorError::Malformed {
            reason: "token is not canonical",
        }
    }

    fn session_id(name: &str) -> SessionId {
        name.parse::<SessionId>()
            .unwrap_or_else(|error| panic!("test identity {name} must parse: {error}"))
    }

    fn coordinate(aggregate: &str, seq: u64) -> CaptureCoordinate {
        CaptureCoordinate {
            aggregate: session_id(aggregate),
            seq: EventSeq::new(seq),
            fencing: EventFencing::Fenced {
                epoch: WriterEpoch::FIRST,
            },
        }
    }

    #[test]
    fn round_trip_preserves_all_interest_shapes() {
        let all = CaptureCursor::start(CaptureInterest::All);
        assert_eq!(
            CaptureCursor::decode(&all.encode(), &CaptureInterest::All)
                .unwrap_or_else(|error| panic!("the all token must decode: {error}")),
            all
        );

        let location = CaptureInterest::Location {
            directory: "/work/one".to_owned(),
            workspace: None,
        };
        let cursor = CaptureCursor::start(location.clone());
        assert_eq!(
            CaptureCursor::decode(&cursor.encode(), &location)
                .unwrap_or_else(|error| panic!("the location token must decode: {error}")),
            cursor
        );

        let workspace = CaptureInterest::Location {
            directory: "/work/two".to_owned(),
            workspace: Some("ws_x".to_owned()),
        };
        let seeded = CaptureCursor::new(
            workspace.clone(),
            vec![
                coordinate("ses_a", 3),
                CaptureCoordinate {
                    aggregate: session_id("ses_b"),
                    seq: EventSeq::new(7),
                    fencing: EventFencing::LegacyUnfenced,
                },
            ],
        )
        .unwrap_or_else(|error| panic!("the workspace cursor must build: {error}"));
        assert_eq!(
            CaptureCursor::decode(&seeded.encode(), &workspace)
                .unwrap_or_else(|error| panic!("the workspace token must decode: {error}")),
            seeded
        );
    }

    #[test]
    fn non_canonical_tokens_are_refused() {
        let space_after_version = b64url(br#"{"version": 1,"interest":"all","coordinates":[]}"#);
        assert_eq!(
            CaptureCursor::decode(&space_after_version, &CaptureInterest::All),
            Err(malformed())
        );

        let reordered = b64url(br#"{"interest":"all","version":1,"coordinates":[]}"#);
        assert_eq!(
            CaptureCursor::decode(&reordered, &CaptureInterest::All),
            Err(malformed())
        );

        let doubled = b64url(br#"{"version":1,"version":1,"interest":"all","coordinates":[]}"#);
        assert_eq!(
            CaptureCursor::decode(&doubled, &CaptureInterest::All),
            Err(malformed())
        );

        let reordered_coordinate = b64url(
            br#"{"version":1,"interest":"all","coordinates":[{"seq":0,"aggregate":"ses_a","epoch":0}]}"#,
        );
        assert_eq!(
            CaptureCursor::decode(&reordered_coordinate, &CaptureInterest::All),
            Err(malformed())
        );

        // A legal escape the canonical encoder never writes: it parses but cannot round-trip.
        let escaped = b64url(
            br#"{"version":1,"interest":{"directory":"\/work/A","workspace":null},"coordinates":[]}"#,
        );
        let interest = CaptureInterest::Location {
            directory: "/work/A".to_owned(),
            workspace: None,
        };
        assert_eq!(
            CaptureCursor::decode(&escaped, &interest),
            Err(CaptureCursorError::Malformed {
                reason: "non-canonical encoding",
            })
        );
    }

    #[test]
    fn unencodable_tokens_are_refused() {
        let valid = CaptureCursor::start(CaptureInterest::All).encode();
        assert_eq!(
            CaptureCursor::decode("!!!", &CaptureInterest::All),
            Err(malformed())
        );
        assert_eq!(
            CaptureCursor::decode(&format!("{valid}="), &CaptureInterest::All),
            Err(malformed())
        );
    }

    #[test]
    fn a_wrong_version_is_distinct_from_malformed() {
        let token = b64url(br#"{"version":2,"interest":"all","coordinates":[]}"#);
        assert_eq!(
            CaptureCursor::decode(&token, &CaptureInterest::All),
            Err(CaptureCursorError::WrongVersion { found: 2 })
        );
    }

    #[test]
    fn a_cursor_is_refused_under_a_different_interest() {
        let location_a = CaptureInterest::Location {
            directory: "/work/one".to_owned(),
            workspace: None,
        };
        let location_b = CaptureInterest::Location {
            directory: "/work/two".to_owned(),
            workspace: None,
        };
        let token = CaptureCursor::start(location_a.clone()).encode();
        assert_eq!(
            CaptureCursor::decode(&token, &location_b),
            Err(CaptureCursorError::WrongLocation)
        );
        assert_eq!(
            CaptureCursor::decode(&token, &CaptureInterest::All),
            Err(CaptureCursorError::WrongLocation)
        );
        let all_token = CaptureCursor::start(CaptureInterest::All).encode();
        assert_eq!(
            CaptureCursor::decode(&all_token, &location_a),
            Err(CaptureCursorError::WrongLocation)
        );
    }

    #[test]
    fn the_cursor_grammar_refuses_unsorted_and_over_bound_coordinates() {
        let unsorted = vec![coordinate("ses_b", 0), coordinate("ses_a", 1)];
        assert_eq!(
            CaptureCursor::new(CaptureInterest::All, unsorted),
            Err(CaptureCursorError::Malformed {
                reason: "coordinates are not strictly increasing by aggregate",
            })
        );

        let duplicate = vec![coordinate("ses_a", 0), coordinate("ses_a", 1)];
        assert_eq!(
            CaptureCursor::new(CaptureInterest::All, duplicate),
            Err(CaptureCursorError::Malformed {
                reason: "coordinates are not strictly increasing by aggregate",
            })
        );

        let over_bound: Vec<CaptureCoordinate> = (0..1025)
            .map(|index| coordinate(&format!("ses_{index:04}"), 0))
            .collect();
        assert_eq!(
            CaptureCursor::new(CaptureInterest::All, over_bound),
            Err(CaptureCursorError::Malformed {
                reason: "too many coordinates",
            })
        );
    }

    #[test]
    fn malformed_coordinate_entries_are_refused_named() {
        let tokens: [&[u8]; 4] = [
            br#"{"version":1,"interest":"all","coordinates":[{"aggregate":"ses_b","seq":0,"epoch":0},{"aggregate":"ses_a","seq":0,"epoch":0}]}"#,
            br#"{"version":1,"interest":"all","coordinates":[{"aggregate":"ses_a","seq":0,"epoch":0},{"aggregate":"ses_a","seq":1,"epoch":0}]}"#,
            br#"{"version":1,"interest":"all","coordinates":[{"aggregate":"campaign_a","seq":0,"epoch":0}]}"#,
            br#"{"version":1,"interest":"all","coordinates":[{"aggregate":"ses_a","seq":0,"epoch":4294967296}]}"#,
        ];
        for raw in tokens {
            let token = b64url(raw);
            assert!(
                matches!(
                    CaptureCursor::decode(&token, &CaptureInterest::All),
                    Err(CaptureCursorError::Malformed { .. })
                ),
                "the token must be refused as malformed: {raw:?}"
            );
        }
    }

    #[test]
    fn an_over_bound_token_is_refused_before_any_parse() {
        let oversized = CaptureCursor::start(CaptureInterest::All).encode()
            + &"A".repeat(MAX_CAPTURE_CURSOR_TOKEN_BYTES);
        assert_eq!(
            CaptureCursor::decode(&oversized, &CaptureInterest::All),
            Err(CaptureCursorError::Malformed {
                reason: "token exceeds the byte bound",
            })
        );
    }

    #[test]
    fn interest_components_are_bounded() {
        assert_eq!(
            CaptureInterest::location(String::new(), None),
            Err(CaptureInterestError::DirectoryEmpty)
        );
        assert_eq!(
            CaptureInterest::location("x".repeat(1025), None),
            Err(CaptureInterestError::DirectoryTooLong)
        );
        assert!(
            CaptureInterest::location("x".repeat(1024), None).is_ok(),
            "the bound itself must be admitted"
        );
        assert_eq!(
            CaptureInterest::location("d".to_owned(), Some(String::new())),
            Err(CaptureInterestError::WorkspaceEmpty)
        );
        assert_eq!(
            CaptureInterest::location("d".to_owned(), Some("x".repeat(257))),
            Err(CaptureInterestError::WorkspaceTooLong)
        );
        assert!(
            CaptureInterest::location("d".to_owned(), Some("x".repeat(256))).is_ok(),
            "the bound itself must be admitted"
        );
    }

    #[test]
    fn a_windows_directory_round_trips_through_the_token() {
        // HOLD B1: the encoder escapes backslashes, so a Windows directory
        // round-trips; lookalikes — quote, doubled backslash, backslash
        // before an escape letter, non-ASCII — stay distinct.
        let interests = [
            ("C:\\workspace\\repo", None),
            (r#"A\"B\\C\"D\t\u0041€é"#, Some("w\"s \\u0041€é")),
        ]
        .map(|(directory, workspace)| CaptureInterest::Location {
            directory: directory.to_owned(),
            workspace: workspace.map(str::to_owned),
        });
        for interest in interests {
            let cursor = CaptureCursor::start(interest.clone());
            assert_eq!(
                CaptureCursor::decode(&cursor.encode(), &interest)
                    .unwrap_or_else(|error| panic!("the token must decode: {error}")),
                cursor
            );
        }
    }
}
