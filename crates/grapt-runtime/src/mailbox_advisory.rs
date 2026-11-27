//! Private, body-free mailbox advisory retention.
//!
//! `SEMADV1` is an in-process runtime seam, not the R3c2 public protocol. It
//! reports only durable mailbox metadata and never participates in handling or
//! consumption. Losing this state is therefore safe: reconnect convergence is
//! a new durable mailbox read, not replay of these retained frames.

use grapt_types::{
    MailboxMessageId, SeatMessageKind, SeatRecipientId, MAX_MAILBOX_MESSAGE_ID_BYTES,
    MAX_SEAT_RECIPIENT_ID_BYTES,
};

const SEMADV1_TAG: &[u8; 7] = b"SEMADV1";
const LENGTH_PREFIX_BYTES: usize = 4;
const PHASE_DISCRIMINATOR_BYTES: usize = 1;
const DISCOVERED_DISCRIMINATOR: u8 = 1;
const HANDLED_DISCRIMINATOR: u8 = 2;
const RESCAN_REQUIRED_DISCRIMINATOR: u8 = 3;
const MAX_KIND_LABEL_BYTES: usize = 14;

/// Maximum retained stable keys in one recipient sink.
pub const MAX_RETAINED_MAILBOX_ADVISORIES: usize = 64;
/// Maximum encoded `SEMADV1` bytes retained in one recipient sink.
pub const MAX_RETAINED_MAILBOX_ADVISORY_BYTES: usize = 32_768;
/// Hostile maximum for one `Discovered` frame.
pub const MAX_DISCOVERED_MAILBOX_ADVISORY_BYTES: usize = 806;
/// Hostile maximum for one `Handled` frame.
pub const MAX_HANDLED_MAILBOX_ADVISORY_BYTES: usize = 528;
/// Hostile maximum reserved for the sticky `RescanRequired` frame.
pub const MAX_RESCAN_REQUIRED_MAILBOX_ADVISORY_BYTES: usize = 268;

/// Body-free mailbox metadata offered after a durable store fact is known.
#[derive(Debug, Clone, Copy)]
pub enum MailboxAdvisory<'a> {
    /// The exact durable actionable read returned this message.
    Discovered {
        recipient: &'a SeatRecipientId,
        message: &'a MailboxMessageId,
        sender: &'a SeatRecipientId,
        kind: &'a SeatMessageKind,
    },
    /// The handler succeeded and the local consumed marker committed.
    Handled {
        recipient: &'a SeatRecipientId,
        message: &'a MailboxMessageId,
    },
}

impl<'a> MailboxAdvisory<'a> {
    fn recipient(self) -> &'a SeatRecipientId {
        match self {
            MailboxAdvisory::Discovered { recipient, .. }
            | MailboxAdvisory::Handled { recipient, .. } => recipient,
        }
    }

    fn message(self) -> &'a MailboxMessageId {
        match self {
            MailboxAdvisory::Discovered { message, .. }
            | MailboxAdvisory::Handled { message, .. } => message,
        }
    }
}

/// A best-effort advisory destination. Offers cannot fail the durable path.
pub trait MailboxAdvisorySink {
    fn offer(&mut self, advisory: MailboxAdvisory<'_>) -> MailboxAdvisoryOffer;
}

/// Exact outcome of one best-effort offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxAdvisoryOffer {
    Retained,
    Coalesced,
    DroppedDisconnected,
    DroppedRescanRequired,
    OverflowedCount,
    OverflowedBytes,
    Fault(MailboxAdvisoryFault),
}

/// Loud, bounded diagnostics for impossible accounting or ownership states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MailboxAdvisoryFault {
    #[error("the advisory belongs to a different recipient sink")]
    RecipientMismatch,
    #[error("mailbox advisory byte accounting overflowed")]
    AccountingOverflow,
    #[error("mailbox advisory encoded bytes disagree with checked accounting")]
    EncodingInvariantViolation,
}

/// The retained phase for one stable `(recipient, message)` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxAdvisoryPhase {
    Discovered,
    Handled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MailboxAdvisoryKey {
    recipient: SeatRecipientId,
    message: MailboxMessageId,
}

#[derive(Debug, Clone)]
struct RetainedAdvisory {
    key: MailboxAdvisoryKey,
    phase: MailboxAdvisoryPhase,
    encoded: Vec<u8>,
}

/// One bounded, recipient-owned live advisory sink.
///
/// This concrete sink is also the deterministic fake used by the witness
/// suite. A transport adapter is deliberately outside R3c1.
#[derive(Debug, Clone)]
pub struct BoundedMailboxAdvisorySink {
    recipient: SeatRecipientId,
    entries: Vec<RetainedAdvisory>,
    keyed_bytes: usize,
    rescan_required: bool,
    connected: bool,
    last_fault: Option<MailboxAdvisoryFault>,
}

impl BoundedMailboxAdvisorySink {
    pub fn new(recipient: SeatRecipientId) -> Self {
        Self {
            recipient,
            entries: Vec::new(),
            keyed_bytes: 0,
            rescan_required: false,
            connected: true,
            last_fault: None,
        }
    }

    pub fn recipient(&self) -> &SeatRecipientId {
        &self.recipient
    }

    pub fn retained_entries(&self) -> usize {
        self.entries.len()
    }

    pub fn retained_encoded_bytes(&self) -> usize {
        let rescan_bytes = usize::from(self.rescan_required)
            * (SEMADV1_TAG.len()
                + PHASE_DISCRIMINATOR_BYTES
                + LENGTH_PREFIX_BYTES
                + self.recipient.as_str().len());
        self.keyed_bytes + rescan_bytes
    }

    pub fn rescan_required(&self) -> bool {
        self.rescan_required
    }

    pub fn last_fault(&self) -> Option<MailboxAdvisoryFault> {
        self.last_fault
    }

    pub fn retained_phase(&self, message: &MailboxMessageId) -> Option<MailboxAdvisoryPhase> {
        self.entries
            .iter()
            .find(|entry| entry.key.message == *message)
            .map(|entry| entry.phase)
    }

    /// Returns current private frames in insertion order and resets live state.
    pub fn take_encoded(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::with_capacity(self.entries.len() + 1);
        frames.extend(self.entries.drain(..).map(|entry| entry.encoded));
        if self.rescan_required {
            match encode_rescan_required(&self.recipient) {
                Ok(frame) => frames.push(frame),
                Err(fault) => self.last_fault = Some(fault),
            }
        }
        self.keyed_bytes = 0;
        self.rescan_required = false;
        frames
    }

    /// Models total live client-state loss. Retained advisories are not replayed.
    pub fn disconnect(&mut self) {
        self.entries.clear();
        self.keyed_bytes = 0;
        self.rescan_required = false;
        self.connected = false;
    }

    /// Re-enables offers; convergence must come from another durable read.
    pub fn reconnect(&mut self) {
        self.connected = true;
    }

    fn fault(&mut self, fault: MailboxAdvisoryFault) -> MailboxAdvisoryOffer {
        self.last_fault = Some(fault);
        self.rescan_required = true;
        MailboxAdvisoryOffer::Fault(fault)
    }

    fn set_overflow(&mut self, outcome: MailboxAdvisoryOffer) -> MailboxAdvisoryOffer {
        self.rescan_required = true;
        outcome
    }
}

impl MailboxAdvisorySink for BoundedMailboxAdvisorySink {
    fn offer(&mut self, advisory: MailboxAdvisory<'_>) -> MailboxAdvisoryOffer {
        if !self.connected {
            return MailboxAdvisoryOffer::DroppedDisconnected;
        }
        if advisory.recipient() != &self.recipient {
            return self.fault(MailboxAdvisoryFault::RecipientMismatch);
        }

        let existing = self.entries.iter().position(|entry| {
            entry.key.recipient == *advisory.recipient() && entry.key.message == *advisory.message()
        });
        if let Some(index) = existing {
            let current = self.entries[index].phase;
            let wanted = advisory_phase(advisory);
            if current == wanted || current == MailboxAdvisoryPhase::Handled {
                return MailboxAdvisoryOffer::Coalesced;
            }
            let encoded = match encode_advisory(advisory) {
                Ok(encoded) => encoded,
                Err(fault) => return self.fault(fault),
            };
            let without_old = match self
                .keyed_bytes
                .checked_sub(self.entries[index].encoded.len())
            {
                Some(bytes) => bytes,
                None => return self.fault(MailboxAdvisoryFault::EncodingInvariantViolation),
            };
            self.keyed_bytes = match without_old.checked_add(encoded.len()) {
                Some(bytes) if bytes <= MAX_RETAINED_MAILBOX_ADVISORY_BYTES => bytes,
                Some(_) => return self.fault(MailboxAdvisoryFault::EncodingInvariantViolation),
                None => return self.fault(MailboxAdvisoryFault::AccountingOverflow),
            };
            self.entries[index].phase = wanted;
            self.entries[index].encoded = encoded;
            return MailboxAdvisoryOffer::Retained;
        }

        if self.rescan_required {
            return MailboxAdvisoryOffer::DroppedRescanRequired;
        }
        if self.entries.len() >= MAX_RETAINED_MAILBOX_ADVISORIES {
            return self.set_overflow(MailboxAdvisoryOffer::OverflowedCount);
        }

        let encoded = match encode_advisory(advisory) {
            Ok(encoded) => encoded,
            Err(fault) => return self.fault(fault),
        };
        let with_entry = match self.keyed_bytes.checked_add(encoded.len()) {
            Some(bytes) => bytes,
            None => return self.fault(MailboxAdvisoryFault::AccountingOverflow),
        };
        let with_rescan_reserve =
            match with_entry.checked_add(MAX_RESCAN_REQUIRED_MAILBOX_ADVISORY_BYTES) {
                Some(bytes) => bytes,
                None => return self.fault(MailboxAdvisoryFault::AccountingOverflow),
            };
        if with_rescan_reserve > MAX_RETAINED_MAILBOX_ADVISORY_BYTES {
            return self.set_overflow(MailboxAdvisoryOffer::OverflowedBytes);
        }

        self.entries.push(RetainedAdvisory {
            key: MailboxAdvisoryKey {
                recipient: advisory.recipient().clone(),
                message: advisory.message().clone(),
            },
            phase: advisory_phase(advisory),
            encoded,
        });
        self.keyed_bytes = with_entry;
        MailboxAdvisoryOffer::Retained
    }
}

pub(crate) struct NoMailboxAdvisories;

impl MailboxAdvisorySink for NoMailboxAdvisories {
    fn offer(&mut self, _advisory: MailboxAdvisory<'_>) -> MailboxAdvisoryOffer {
        MailboxAdvisoryOffer::DroppedDisconnected
    }
}

fn advisory_phase(advisory: MailboxAdvisory<'_>) -> MailboxAdvisoryPhase {
    match advisory {
        MailboxAdvisory::Discovered { .. } => MailboxAdvisoryPhase::Discovered,
        MailboxAdvisory::Handled { .. } => MailboxAdvisoryPhase::Handled,
    }
}

/// The kind label a frame carries. A kind this build does not know is the
/// empty label: no admissible kind label is empty, so it cannot be mistaken
/// for one, and the frame keeps its exact hostile maximum. The message itself
/// keeps the verbatim label; this body-free hint is lossy by design.
fn frame_kind(kind: &SeatMessageKind) -> &str {
    match kind {
        SeatMessageKind::Other(_) => "",
        known => known.as_str(),
    }
}

fn encoded_string_len(raw: &str) -> Result<usize, MailboxAdvisoryFault> {
    LENGTH_PREFIX_BYTES
        .checked_add(raw.len())
        .ok_or(MailboxAdvisoryFault::AccountingOverflow)
}

fn encoded_len(advisory: MailboxAdvisory<'_>) -> Result<usize, MailboxAdvisoryFault> {
    let mut bytes = SEMADV1_TAG
        .len()
        .checked_add(PHASE_DISCRIMINATOR_BYTES)
        .ok_or(MailboxAdvisoryFault::AccountingOverflow)?;
    let fields: [&str; 4] = match advisory {
        MailboxAdvisory::Discovered {
            recipient,
            message,
            sender,
            kind,
        } => [
            recipient.as_str(),
            message.as_str(),
            sender.as_str(),
            frame_kind(kind),
        ],
        MailboxAdvisory::Handled { recipient, message } => {
            [recipient.as_str(), message.as_str(), "", ""]
        }
    };
    let used = if matches!(advisory, MailboxAdvisory::Discovered { .. }) {
        fields.len()
    } else {
        2
    };
    for field in &fields[..used] {
        bytes = bytes
            .checked_add(encoded_string_len(field)?)
            .ok_or(MailboxAdvisoryFault::AccountingOverflow)?;
    }
    Ok(bytes)
}

fn push_string(frame: &mut Vec<u8>, raw: &str) -> Result<(), MailboxAdvisoryFault> {
    let length = u32::try_from(raw.len()).map_err(|_| MailboxAdvisoryFault::AccountingOverflow)?;
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(raw.as_bytes());
    Ok(())
}

fn encode_advisory(advisory: MailboxAdvisory<'_>) -> Result<Vec<u8>, MailboxAdvisoryFault> {
    let accounted = encoded_len(advisory)?;
    let mut frame = Vec::with_capacity(accounted);
    frame.extend_from_slice(SEMADV1_TAG);
    match advisory {
        MailboxAdvisory::Discovered {
            recipient,
            message,
            sender,
            kind,
        } => {
            frame.push(DISCOVERED_DISCRIMINATOR);
            push_string(&mut frame, recipient.as_str())?;
            push_string(&mut frame, message.as_str())?;
            push_string(&mut frame, sender.as_str())?;
            push_string(&mut frame, frame_kind(kind))?;
        }
        MailboxAdvisory::Handled { recipient, message } => {
            frame.push(HANDLED_DISCRIMINATOR);
            push_string(&mut frame, recipient.as_str())?;
            push_string(&mut frame, message.as_str())?;
        }
    }
    if frame.len() != accounted {
        return Err(MailboxAdvisoryFault::EncodingInvariantViolation);
    }
    Ok(frame)
}

fn encoded_rescan_len(recipient: &SeatRecipientId) -> Result<usize, MailboxAdvisoryFault> {
    let recipient_bytes = encoded_string_len(recipient.as_str())?;
    SEMADV1_TAG
        .len()
        .checked_add(PHASE_DISCRIMINATOR_BYTES)
        .and_then(|bytes| bytes.checked_add(recipient_bytes))
        .ok_or(MailboxAdvisoryFault::AccountingOverflow)
}

fn encode_rescan_required(recipient: &SeatRecipientId) -> Result<Vec<u8>, MailboxAdvisoryFault> {
    let accounted = encoded_rescan_len(recipient)?;
    let mut frame = Vec::with_capacity(accounted);
    frame.extend_from_slice(SEMADV1_TAG);
    frame.push(RESCAN_REQUIRED_DISCRIMINATOR);
    push_string(&mut frame, recipient.as_str())?;
    if frame.len() != accounted {
        return Err(MailboxAdvisoryFault::EncodingInvariantViolation);
    }
    Ok(frame)
}

const _: () = {
    assert!(MAX_KIND_LABEL_BYTES == "ruling-request".len());
    assert!(
        MAX_DISCOVERED_MAILBOX_ADVISORY_BYTES
            == 7 + 1
                + 4
                + MAX_SEAT_RECIPIENT_ID_BYTES
                + 4
                + MAX_MAILBOX_MESSAGE_ID_BYTES
                + 4
                + MAX_SEAT_RECIPIENT_ID_BYTES
                + 4
                + MAX_KIND_LABEL_BYTES
    );
    assert!(
        MAX_HANDLED_MAILBOX_ADVISORY_BYTES
            == 7 + 1 + 4 + MAX_SEAT_RECIPIENT_ID_BYTES + 4 + MAX_MAILBOX_MESSAGE_ID_BYTES
    );
    assert!(MAX_RESCAN_REQUIRED_MAILBOX_ADVISORY_BYTES == 7 + 1 + 4 + MAX_SEAT_RECIPIENT_ID_BYTES);
};

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn recipient(raw: &str) -> Result<SeatRecipientId, Box<dyn std::error::Error>> {
        Ok(SeatRecipientId::try_new(raw.to_owned())?)
    }

    fn message(raw: &str) -> Result<MailboxMessageId, Box<dyn std::error::Error>> {
        Ok(MailboxMessageId::try_new(raw.to_owned())?)
    }

    #[test]
    fn semadv1_hostile_maxima_are_exact() -> TestResult {
        let owner = recipient(&"r".repeat(MAX_SEAT_RECIPIENT_ID_BYTES))?;
        let sender = recipient(&"s".repeat(MAX_SEAT_RECIPIENT_ID_BYTES))?;
        let id = message(&"m".repeat(MAX_MAILBOX_MESSAGE_ID_BYTES))?;
        let discovered = encode_advisory(MailboxAdvisory::Discovered {
            recipient: &owner,
            message: &id,
            sender: &sender,
            kind: &SeatMessageKind::RulingRequest,
        })?;
        let handled = encode_advisory(MailboxAdvisory::Handled {
            recipient: &owner,
            message: &id,
        })?;
        let rescan = encode_rescan_required(&owner)?;
        assert_eq!(discovered.len(), MAX_DISCOVERED_MAILBOX_ADVISORY_BYTES);
        assert_eq!(handled.len(), MAX_HANDLED_MAILBOX_ADVISORY_BYTES);
        assert_eq!(rescan.len(), MAX_RESCAN_REQUIRED_MAILBOX_ADVISORY_BYTES);
        assert_eq!(&discovered[..7], b"SEMADV1");
        assert_eq!(discovered[7], 1, "SEMADV1 Discovered is discriminator 1");
        assert_eq!(handled[7], 2, "SEMADV1 Handled is discriminator 2");
        assert_eq!(rescan[7], 3, "SEMADV1 RescanRequired is discriminator 3");
        Ok(())
    }

    #[test]
    fn semadv1_lengths_count_utf8_bytes_and_preserve_nuls() -> TestResult {
        let owner = recipient("seat-\0😀")?;
        let sender = recipient("sender-😀")?;
        let id = message("msg-😀")?;
        let frame = encode_advisory(MailboxAdvisory::Discovered {
            recipient: &owner,
            message: &id,
            sender: &sender,
            kind: &SeatMessageKind::TaskPointer,
        })?;
        assert_eq!(&frame[8..12], &(owner.as_str().len() as u32).to_be_bytes());
        assert!(frame.windows(4).any(|window| window == "😀".as_bytes()));
        assert!(frame.contains(&0));
        assert_eq!(
            frame.len(),
            encoded_len(MailboxAdvisory::Discovered {
                recipient: &owner,
                message: &id,
                sender: &sender,
                kind: &SeatMessageKind::TaskPointer,
            })?
        );
        Ok(())
    }

    #[test]
    fn checked_accounting_refuses_overflow() -> TestResult {
        let field = encoded_string_len("x")?;
        assert_eq!(usize::MAX.checked_add(field), None);
        Ok(())
    }
}
