//! Strict tagged record encoding. Unknown and trailing data refuse.
use super::*;
use sha2::{Digest, Sha256};

fn hexadecimal(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}
fn scope_bytes(scope: &RemoteMailboxScope) -> Vec<u8> {
    let mut bytes = Vec::new();
    for field in [
        scope.projects.source().as_str(),
        scope.projects.target().as_str(),
        scope.principal.as_str(),
        scope.recipient.as_str(),
    ] {
        push_field(&mut bytes, field.as_bytes());
    }
    bytes
}
pub(super) fn local_id(
    scope: &RemoteMailboxScope,
    source: &RemoteMailboxMessageId,
) -> Result<MailboxMessageId, WriteError> {
    let mut bytes = b"secode-remote-mail-identity-v1".to_vec();
    bytes.extend_from_slice(&scope_bytes(scope));
    push_field(&mut bytes, source.as_str().as_bytes());
    MailboxMessageId::try_new(format!(
        "remote-mail:{}",
        hexadecimal(&Sha256::digest(bytes))
    ))
    .map_err(|error| refusal(error.to_string()))
}
pub(super) fn sender_address(
    scope: &RemoteMailboxScope,
    sender: &SourcePrincipalId,
) -> Result<SeatRecipientId, WriteError> {
    let mut bytes = b"secode-remote-sender-address-v1".to_vec();
    bytes.extend_from_slice(&scope_bytes(scope));
    push_field(&mut bytes, sender.as_str().as_bytes());
    SeatRecipientId::try_new(format!(
        "remote-sender:{}",
        hexadecimal(&Sha256::digest(bytes))
    ))
    .map_err(|error| refusal(error.to_string()))
}
fn optional(out: &mut Vec<u8>, value: Option<&RemoteMailboxMessageId>) {
    match value {
        None => out.push(0),
        Some(value) => {
            out.push(1);
            push_field(out, value.as_str().as_bytes());
        }
    }
}
pub(super) fn immutable_bytes(message: &SeatMailboxMessage, metadata: &Metadata) -> Vec<u8> {
    let mut bytes = encode_record(message);
    bytes.extend_from_slice(&scope_bytes(&metadata.scope));
    push_field(
        &mut bytes,
        metadata.ack.source_message().as_str().as_bytes(),
    );
    push_field(&mut bytes, metadata.source_created.as_str().as_bytes());
    push_field(&mut bytes, metadata.sender.as_str().as_bytes());
    let count = u32::try_from(metadata.refs.len()).unwrap_or(u32::MAX);
    bytes.extend_from_slice(&count.to_be_bytes());
    for value in &metadata.refs {
        push_field(&mut bytes, value.as_bytes());
    }
    optional(&mut bytes, metadata.supersedes.as_ref());
    bytes
}
pub(super) fn digest(
    message: &SeatMailboxMessage,
    metadata: &Metadata,
) -> Result<MailboxMessageDigest, WriteError> {
    MailboxMessageDigest::try_new(hexadecimal(&Sha256::digest(immutable_bytes(
        message, metadata,
    ))))
    .map_err(|error| refusal(error.to_string()))
}
pub(super) fn encode(message: &SeatMailboxMessage, metadata: &Metadata) -> Vec<u8> {
    let mut bytes = TAG.to_vec();
    push_field(&mut bytes, &encode_record(message));
    bytes.extend_from_slice(&scope_bytes(&metadata.scope));
    push_field(&mut bytes, metadata.sender.as_str().as_bytes());
    for field in [
        metadata.ack.ack_id().as_str(),
        metadata.ack.source_binding().as_str(),
        metadata.ack.source_message().as_str(),
        metadata.ack.source_cursor().as_str(),
        metadata.ack.canonical_message_digest().as_str(),
        metadata.source_created.as_str(),
    ] {
        push_field(&mut bytes, field.as_bytes());
    }
    bytes.extend_from_slice(&metadata.ack.local_admission().value().to_be_bytes());
    let count = u32::try_from(metadata.refs.len()).unwrap_or(u32::MAX);
    bytes.extend_from_slice(&count.to_be_bytes());
    for reference in &metadata.refs {
        push_field(&mut bytes, reference.as_bytes());
    }
    optional(&mut bytes, metadata.supersedes.as_ref());
    optional(&mut bytes, metadata.superseded_by.as_ref());
    push_field(
        &mut bytes,
        match metadata.archive {
            SourceArchiveDisposition::Active => b"active",
            SourceArchiveDisposition::Archived(reason) => reason.as_str().as_bytes(),
        },
    );
    bytes.push(match metadata.attention {
        MailAttention::Wake => 1,
        MailAttention::Quiet => 0,
    });
    bytes.push(u8::from(metadata.primary_witnessed));
    bytes.push(match metadata.projection {
        None => 0,
        Some(SecondaryProjectionState::Posted) => 1,
        Some(SecondaryProjectionState::AlreadyPresent) => 2,
        Some(SecondaryProjectionState::Unarmed) => 3,
        Some(SecondaryProjectionState::Refused) => 4,
        Some(SecondaryProjectionState::Unreadable) => 5,
    });
    bytes
}
fn edge(reader: &mut Reader<'_>) -> Result<Option<RemoteMailboxMessageId>, String> {
    match reader.byte("edge")? {
        0 => Ok(None),
        1 => RemoteMailboxMessageId::try_new(reader.field("edge")?)
            .map(Some)
            .map_err(|e| e.to_string()),
        _ => Err("unknown remote edge marker".into()),
    }
}
/// How much of a stored record a read re-derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::mailbox) enum Check {
    /// Every structural check plus the hashed identities and immutable digest.
    Full,
    /// Every structural check without re-hashing. Only a scan that selects
    /// candidates may use it: whatever it selects is re-read with `Full`
    /// before it is consumed or served. A pending count may too: it only
    /// rings a bell, and a tampered row it counts is refused when served.
    Scan,
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn decode_checked(
    bytes: &[u8],
    check: Check,
) -> Result<(SeatMailboxMessage, Metadata), String> {
    if bytes.len() > MAX_REMOTE_MAIL_RECORD_BYTES || !bytes.starts_with(TAG) {
        return Err("unsupported or oversized remote mailbox record".into());
    }
    let mut reader = Reader {
        bytes,
        at: TAG.len(),
    };
    let local_bytes = reader.bytes_field("local message")?;
    if !local_bytes.starts_with(crate::mailbox::record::RECORD_TAG) {
        return Err("remote body must use the canonical legacy tag".into());
    }
    let message = decode_record(local_bytes)?;
    if message.protocol().as_str() != "remote-seat-mail-transport/v1" {
        return Err("unsupported stored remote protocol".into());
    }
    let scope = RemoteMailboxScope {
        projects: ProjectPair::new(
            SourceProjectId::try_new(reader.field("source project")?).map_err(|e| e.to_string())?,
            TargetProjectId::try_new(reader.field("target project")?).map_err(|e| e.to_string())?,
        ),
        principal: SourcePrincipalId::try_new(reader.field("principal")?)
            .map_err(|e| e.to_string())?,
        recipient: SeatRecipientId::try_new(reader.field("recipient")?)
            .map_err(|e| e.to_string())?,
    };
    let sender =
        SourcePrincipalId::try_new(reader.field("source sender")?).map_err(|e| e.to_string())?;
    let ack_id =
        DeliveryAdmissionAckId::try_new(reader.field("ack id")?).map_err(|e| e.to_string())?;
    let source_binding =
        SourceBindingId::try_new(reader.field("binding")?).map_err(|e| e.to_string())?;
    let source_message = RemoteMailboxMessageId::try_new(reader.field("source message")?)
        .map_err(|e| e.to_string())?;
    let source_cursor = ExclusiveSourceCursor::try_new(reader.field("source cursor")?)
        .map_err(|e| e.to_string())?;
    let canonical_message_digest =
        MailboxMessageDigest::try_new(reader.field("digest")?).map_err(|e| e.to_string())?;
    let source_created =
        SourceTimestamp::try_new(reader.field("created")?).map_err(|e| e.to_string())?;
    let mut seq = [0; 8];
    for byte in &mut seq {
        *byte = reader.byte("admission sequence")?;
    }
    let local_admission =
        MailboxAdmissionSeq::new(u64::from_be_bytes(seq)).ok_or("zero remote admission")?;
    let mut count = [0; 4];
    for byte in &mut count {
        *byte = reader.byte("reference count")?;
    }
    let count = u32::from_be_bytes(count) as usize;
    if count > bytes.len().saturating_sub(reader.at) / 5 {
        return Err("remote reference count exceeds its record".into());
    }
    let mut refs = Vec::new();
    for _ in 0..count {
        let value = reader.field("reference")?;
        if value.is_empty() {
            return Err("empty remote reference".into());
        }
        refs.push(value);
    }
    let supersedes = edge(&mut reader)?;
    let superseded_by = edge(&mut reader)?;
    let archive = match reader.field("archive")?.as_str() {
        "active" => SourceArchiveDisposition::Active,
        value => SourceArchiveDisposition::Archived(
            SourceArchiveReason::parse(value).ok_or("unknown archive reason")?,
        ),
    };
    let attention = match reader.byte("attention")? {
        0 => MailAttention::Quiet,
        1 => MailAttention::Wake,
        _ => return Err("unknown remote attention".into()),
    };
    let primary_witnessed = match reader.byte("primary delivery")? {
        0 => false,
        1 => true,
        _ => return Err("unknown primary delivery marker".into()),
    };
    let projection = match reader.byte("projection")? {
        0 => None,
        1 => Some(SecondaryProjectionState::Posted),
        2 => Some(SecondaryProjectionState::AlreadyPresent),
        3 => Some(SecondaryProjectionState::Unarmed),
        4 => Some(SecondaryProjectionState::Refused),
        5 => Some(SecondaryProjectionState::Unreadable),
        _ => return Err("unknown remote projection".into()),
    };
    reader.finish()?;
    let ack = DeliveryAdmissionAck::new(DeliveryAdmissionAckParts {
        ack_id,
        source_binding,
        projects: scope.projects.clone(),
        source_message,
        source_cursor,
        recipient: scope.recipient.clone(),
        local_message: message.id().clone(),
        local_admission,
        canonical_message_digest,
        protocol: message.protocol().clone(),
    });
    let metadata = Metadata {
        scope,
        ack,
        source_created,
        sender,
        refs,
        supersedes,
        superseded_by,
        archive,
        attention,
        primary_witnessed,
        projection,
    };
    let full = check == Check::Full;
    if metadata.scope.recipient != *message.recipient()
        || message.source().as_str() != metadata.ack.source_cursor().as_str()
        || (full && hashed_addresses_disagree(&message, &metadata)?)
    {
        return Err("remote scope or original cursor disagrees with local record".into());
    }
    if full
        && (local_id(&metadata.scope, metadata.ack.source_message()).map_err(|e| e.to_string())?
            != *message.id()
            || digest(&message, &metadata).map_err(|e| e.to_string())?
                != *metadata.ack.canonical_message_digest())
    {
        return Err("remote identity or immutable digest disagrees with its record".into());
    }
    if metadata.ack.ack_id().as_str() != format!("delivery:{}", message.id().as_str()) {
        return Err("remote ACK identity disagrees with local admission".into());
    }
    if !metadata.primary_witnessed && metadata.projection.is_some() {
        return Err("secondary projection lacks primary delivery proof".into());
    }
    validate_edges(&metadata)?;
    Ok((message, metadata))
}

/// The hashed sender address and superseded local identity, re-derived and
/// compared with the record.
fn hashed_addresses_disagree(
    message: &SeatMailboxMessage,
    metadata: &Metadata,
) -> Result<bool, String> {
    Ok(
        sender_address(&metadata.scope, &metadata.sender).map_err(|e| e.to_string())?
            != *message.sender()
            || message.supersedes().cloned()
                != metadata
                    .supersedes
                    .as_ref()
                    .map(|source| local_id(&metadata.scope, source))
                    .transpose()
                    .map_err(|e| e.to_string())?,
    )
}
pub(super) fn validate_edges(metadata: &Metadata) -> Result<(), String> {
    let id = metadata.ack.source_message();
    if metadata.supersedes.as_ref() == Some(id)
        || metadata.superseded_by.as_ref() == Some(id)
        || metadata.supersedes.is_some() && metadata.supersedes == metadata.superseded_by
    {
        return Err("invalid remote supersession edge".into());
    }
    Ok(())
}
pub(super) fn decode_message_checked(
    bytes: &[u8],
    check: Check,
) -> Result<SeatMailboxMessage, String> {
    decode_checked(bytes, check).map(|(message, _)| message)
}
