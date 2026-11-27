//! The canonical mailbox record codec: the exact bytes of one typed message.
//!
//! The table row is the record's index; these bytes are the record itself.
//! Encoding is explicit and length-prefixed, and decoding is a refusal on any
//! disagreement, never a guess.

use grapt_types::{
    MailboxBody, MailboxIdentityError, MailboxMessageId, MailboxSourceCursor, ProtocolVersion,
    SeatMailboxMessage, SeatMessageKind, SeatRecipientId,
};

/// The canonical record format tag. A stored row that does not begin with
/// exactly these bytes is not a record this decoder will guess at.
pub(super) const RECORD_TAG: &[u8; 6] = b"SEMBX1";

/// Encodes one typed message into its exact canonical bytes.
///
/// Explicit and length-prefixed rather than derived: this crate owns the format,
/// nothing in it is inferred from field order at runtime, and `grapt-types`
/// stays serde-free (F1 ruling).
pub(super) fn encode_record(message: &SeatMailboxMessage) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(RECORD_TAG);
    push_field(&mut out, message.protocol().as_str().as_bytes());
    push_field(&mut out, message.id().as_str().as_bytes());
    push_field(&mut out, message.sender().as_str().as_bytes());
    push_field(&mut out, message.recipient().as_str().as_bytes());
    push_field(&mut out, message.kind().as_str().as_bytes());
    push_field(&mut out, message.body().as_str().as_bytes());
    push_field(&mut out, message.source().as_str().as_bytes());
    match message.supersedes() {
        Some(target) => {
            out.push(1);
            push_field(&mut out, target.as_str().as_bytes());
        }
        None => out.push(0),
    }
    out
}

pub(super) fn push_field(out: &mut Vec<u8>, bytes: &[u8]) {
    // A length that cannot be represented cannot be encoded; every field here
    // is already bounded by its own constructor, so the cast is total for any
    // value that exists.
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

/// A strict decoder over the canonical bytes.
///
/// Every failure is a REFUSAL with a reason, never a default value: a bad kind
/// label, a truncated field, trailing bytes, invalid UTF-8, an empty identity or
/// an oversize body all fail closed.
pub(super) struct Reader<'a> {
    pub(super) bytes: &'a [u8],
    pub(super) at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < RECORD_TAG.len() || &bytes[..RECORD_TAG.len()] != RECORD_TAG {
            return Err("record does not begin with the canonical mailbox tag".to_owned());
        }
        Ok(Reader {
            bytes,
            at: RECORD_TAG.len(),
        })
    }

    pub(super) fn field(&mut self, name: &str) -> Result<String, String> {
        let end = self
            .at
            .checked_add(4)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| format!("{name} length is truncated"))?;
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&self.bytes[self.at..end]);
        let len = u32::from_be_bytes(len_bytes) as usize;
        let stop = end
            .checked_add(len)
            .filter(|stop| *stop <= self.bytes.len())
            .ok_or_else(|| format!("{name} is truncated"))?;
        let raw = std::str::from_utf8(&self.bytes[end..stop])
            .map_err(|error| format!("{name} is not UTF-8: {error}"))?
            .to_owned();
        self.at = stop;
        Ok(raw)
    }

    pub(super) fn bytes_field(&mut self, name: &str) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(4)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| format!("{name} length is truncated"))?;
        let mut size = [0; 4];
        size.copy_from_slice(&self.bytes[self.at..end]);
        let stop = end
            .checked_add(u32::from_be_bytes(size) as usize)
            .filter(|stop| *stop <= self.bytes.len())
            .ok_or_else(|| format!("{name} is truncated"))?;
        self.at = stop;
        Ok(&self.bytes[end..stop])
    }

    pub(super) fn byte(&mut self, name: &str) -> Result<u8, String> {
        let value = *self
            .bytes
            .get(self.at)
            .ok_or_else(|| format!("{name} marker is missing"))?;
        self.at += 1;
        Ok(value)
    }

    /// Trailing bytes are a refusal: a record with something appended is not
    /// the record it claims to be.
    pub(super) fn finish(self) -> Result<(), String> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(format!(
                "{} trailing byte(s) after the record",
                self.bytes.len() - self.at
            ))
        }
    }
}

pub(super) fn decode_record(bytes: &[u8]) -> Result<SeatMailboxMessage, String> {
    decode_record_checked(bytes, super::remote::Check::Full)
}

/// A remote-tagged record decodes at the caller's check depth.
pub(super) fn decode_record_checked(
    bytes: &[u8],
    check: super::remote::Check,
) -> Result<SeatMailboxMessage, String> {
    if bytes.starts_with(super::remote::TAG) {
        return super::remote::decode_message_checked(bytes, check);
    }
    let mut reader = Reader::new(bytes)?;
    let protocol = reader.field("protocol")?;
    let id = reader.field("message id")?;
    let sender = reader.field("sender")?;
    let recipient = reader.field("recipient")?;
    let kind = reader.field("kind")?;
    let body = reader.field("body")?;
    let source = reader.field("source cursor")?;
    let supersedes = match reader.byte("supersedes")? {
        0 => None,
        1 => Some(reader.field("supersedes target")?),
        other => return Err(format!("supersedes marker {other} is not 0 or 1")),
    };
    reader.finish()?;

    let message = SeatMailboxMessage::new(
        ProtocolVersion::new(protocol).ok_or("protocol version is empty")?,
        MailboxMessageId::try_new(id).map_err(|error| identity_refusal("message id", error))?,
        SeatRecipientId::try_new(sender).map_err(|error| identity_refusal("sender", error))?,
        SeatRecipientId::try_new(recipient)
            .map_err(|error| identity_refusal("recipient", error))?,
        SeatMessageKind::parse(&kind).ok_or_else(|| format!("bad message kind {kind:?}"))?,
        MailboxBody::received(body).map_err(|error| error.to_string())?,
        match supersedes {
            Some(target) => Some(
                MailboxMessageId::try_new(target)
                    .map_err(|error| identity_refusal("supersedes target", error))?,
            ),
            None => None,
        },
        MailboxSourceCursor::new(source).ok_or("source cursor is empty")?,
    )
    .map_err(|error| error.to_string())?;
    Ok(message)
}

fn identity_refusal(field: &str, error: MailboxIdentityError) -> String {
    match error {
        MailboxIdentityError::Empty => format!("{field} is empty"),
        MailboxIdentityError::TooLarge { bytes, max } => format!(
            "{field} is {bytes} UTF-8 bytes, above the {max}-byte bound; \
             the stored row is refused, never truncated"
        ),
    }
}
