//! One caller-owned, bounded page of durable seat mail.
//!
//! This is a library seam, not a seat host or transport. The caller decides
//! when a safe boundary exists and supplies a typed handler. Seat mail does not
//! enter or reuse the closed Session `InboxInputClass`: the handler owns its
//! meaning, and this module owns only the durable handoff order.
//!
//! One invocation reads exactly one existing R3a page. The store therefore
//! retains the inherited hard bounds of 64 messages and 262,144 decoded body
//! bytes. This drain adds no page loop, aggregate/time budget, TTL, age-based
//! stale policy, subscription, reconnect, notification, or process-liveness
//! claim.
//!
//! [`presentation`] separately projects action-filtered mail at the caller's
//! safe boundary without changing this durable drain or its acknowledgements.

pub mod presentation;
pub mod remote;

use grapt_store::{EventStore, MailboxPageEntry, WriteError, MAX_MAILBOX_PAGE_MESSAGES};
use grapt_types::{MailboxCursor, MailboxMessageId, SeatMailboxMessage, SeatRecipientId};

use crate::mailbox_advisory::{MailboxAdvisory, MailboxAdvisorySink, NoMailboxAdvisories};

/// The caller boundary for one actionable seat-mail message.
///
/// Returning `Ok(())` means the caller accepted the message and permits the
/// runtime to mark its durable row consumed. Returning `Err` leaves that row
/// actionable. Callers should make externally visible effects idempotent: a
/// process loss after handler success but before the consumed marker is an
/// explicit ambiguity this library cannot settle on the caller's behalf.
pub trait MailboxMessageHandler {
    type Error;

    fn handle(&mut self, message: &SeatMailboxMessage) -> Result<(), Self::Error>;
}

impl<F, E> MailboxMessageHandler for F
where
    F: FnMut(&SeatMailboxMessage) -> Result<(), E>,
{
    type Error = E;

    fn handle(&mut self, message: &SeatMailboxMessage) -> Result<(), Self::Error> {
        self(message)
    }
}

/// The completed result of exactly one bounded mailbox page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxDrainPage {
    consumed_messages: usize,
    unreadable_rows: usize,
    body_bytes: usize,
    next_after: Option<MailboxCursor>,
}

impl MailboxDrainPage {
    /// Rows whose handlers succeeded and whose consumed markers committed.
    pub fn consumed_messages(self) -> usize {
        self.consumed_messages
    }

    /// Unreadable rows named in the page and consumed as their own named
    /// absence; no handler saw them.
    pub fn unreadable_rows(self) -> usize {
        self.unreadable_rows
    }

    /// Decoded body bytes in the page returned by the store.
    pub fn body_bytes(self) -> usize {
        self.body_bytes
    }

    /// The store's exclusive ascending cursor, or `None` when mail is exhausted.
    pub fn next_after(self) -> Option<MailboxCursor> {
        self.next_after
    }
}

/// Why one page did not complete.
#[derive(Debug, thiserror::Error)]
pub enum MailboxDrainError<E> {
    /// The bounded actionable page could not be read or decoded.
    #[error("the actionable mailbox page could not be read")]
    Read(#[source] WriteError),
    /// The caller refused one message; that message remains actionable.
    #[error(
        "the caller refused mailbox message {message:?} after {consumed_before_failure} consumed row(s)"
    )]
    Handler {
        message: MailboxMessageId,
        consumed_before_failure: usize,
        #[source]
        source: E,
    },
    /// The caller succeeded, but the durable consumed marker did not commit.
    #[error(
        "mailbox message {message:?} was handled but not marked consumed after {consumed_before_failure} earlier row(s)"
    )]
    Consume {
        message: MailboxMessageId,
        consumed_before_failure: usize,
        #[source]
        source: WriteError,
    },
    /// The unreadable row was named in the page, but its consumed marker did
    /// not commit; it remains actionable and is named again on the retry.
    #[error(
        "the unreadable mailbox row {message} was not marked consumed after {consumed_before_failure} consumed row(s)"
    )]
    ConsumeUnreadable {
        message: String,
        consumed_before_failure: usize,
        #[source]
        source: WriteError,
    },
}

/// Handles exactly one existing R3a mailbox page in ascending admission order.
///
/// The `after` cursor is exclusive. On success, `next_after` is the exact
/// cursor returned by the store. On any error the caller should retry from its
/// original cursor: earlier rows whose consumed markers committed are excluded
/// durably, while the failed row remains actionable.
pub fn drain_mailbox_page<H>(
    store: &mut EventStore,
    recipient: &SeatRecipientId,
    after: MailboxCursor,
    handler: &mut H,
) -> Result<MailboxDrainPage, MailboxDrainError<H::Error>>
where
    H: MailboxMessageHandler,
{
    drain_mailbox_page_with_advisories(store, recipient, after, handler, &mut NoMailboxAdvisories)
}

/// Handles one durable page and offers body-free, best-effort live advisories.
///
/// `Discovered` follows the exact durable actionable read. `Handled` follows
/// both handler success and the committed consumed marker. Every sink outcome
/// is deliberately ignored so advisory loss cannot change durable behavior.
pub fn drain_mailbox_page_with_advisories<H, S>(
    store: &mut EventStore,
    recipient: &SeatRecipientId,
    after: MailboxCursor,
    handler: &mut H,
    sink: &mut S,
) -> Result<MailboxDrainPage, MailboxDrainError<H::Error>>
where
    H: MailboxMessageHandler,
    S: MailboxAdvisorySink,
{
    let page = store
        .read_actionable_mailbox(recipient, after, MAX_MAILBOX_PAGE_MESSAGES)
        .map_err(MailboxDrainError::Read)?;
    let mut consumed_messages = 0usize;
    let mut unreadable_rows = 0usize;

    for entry in &page.entries {
        match entry {
            MailboxPageEntry::Message(message) => {
                let _ = sink.offer(MailboxAdvisory::Discovered {
                    recipient: message.recipient(),
                    message: message.id(),
                    sender: message.sender(),
                    kind: message.kind(),
                });
                handler
                    .handle(message)
                    .map_err(|source| MailboxDrainError::Handler {
                        message: message.id().clone(),
                        consumed_before_failure: consumed_messages,
                        source,
                    })?;
                store
                    .consume_mailbox_message(recipient, message.id())
                    .map_err(|source| MailboxDrainError::Consume {
                        message: message.id().clone(),
                        consumed_before_failure: consumed_messages,
                        source,
                    })?;
                let _ = sink.offer(MailboxAdvisory::Handled {
                    recipient: message.recipient(),
                    message: message.id(),
                });
                consumed_messages = consumed_messages.saturating_add(1);
            }
            MailboxPageEntry::Unreadable(row) => {
                // The row is named in the page and then consumed as its own
                // named absence: one bad row costs that one position and the
                // mailbox moves on, so it can never hold the reader at its
                // cursor again.
                store
                    .consume_unreadable_mailbox_message(recipient, &row.message)
                    .map_err(|source| MailboxDrainError::ConsumeUnreadable {
                        message: row.message.clone(),
                        consumed_before_failure: consumed_messages,
                        source,
                    })?;
                unreadable_rows = unreadable_rows.saturating_add(1);
            }
        }
    }

    Ok(MailboxDrainPage {
        consumed_messages,
        unreadable_rows,
        body_bytes: page.body_bytes,
        next_after: page.next_after,
    })
}
