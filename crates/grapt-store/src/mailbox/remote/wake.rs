//! A frozen local-mail claim, committed with its canonical Session input.
use super::presentation::{verify_reference, MailBindingExpectation};
use super::*;
use grapt_event::mail_presented::PresentedMail;
use grapt_types::SessionId;

/// One bounded scan. Overflow stays in the mailbox, never in a second queue.
#[derive(Debug)]
pub struct RemoteMailWakeScan {
    pub claim: Option<RemoteMailWakeClaim>,
    pub after: MailboxCursor,
    pub pending: bool,
}

/// A newest-first scan; skipped delivery debt remains in its original record.
#[derive(Debug)]
pub struct RemoteMailRecentWakeScan {
    pub claim: Option<RemoteMailWakeClaim>,
    pub before: Option<MailboxAdmissionSeq>,
    pub pending: bool,
    /// The newest row the scan read; see [`RemoteMailboxRecentPage::top`].
    pub top: Option<MailboxAdmissionSeq>,
}

/// Only the store can prepare a claim from verified, settled local records.
/// The input owner must consume it inside its admission transaction.
pub struct RemoteMailWakeClaim {
    session: SessionId,
    scope: RemoteMailboxScope,
    binding: MailBindingExpectation,
    references: Vec<PresentedMail>,
}

impl std::fmt::Debug for RemoteMailWakeClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteMailWakeClaim")
            .field("messages", &self.references.len())
            .finish_non_exhaustive()
    }
}

impl RemoteMailWakeClaim {
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn source_messages(&self) -> impl Iterator<Item = &RemoteMailboxMessageId> {
        self.references
            .iter()
            .map(|reference| &reference.source_message)
    }

    fn try_push(&mut self, record: &RemoteMailboxRecord) -> Result<bool, WriteError> {
        self.references.push(PresentedMail {
            local_message: record.ack().local_message().clone(),
            source_message: record.ack().source_message().clone(),
            local_admission: record.ack().local_admission(),
            digest: record.ack().canonical_message_digest().clone(),
        });
        if serde_json::to_vec(&self.metadata())
            .map_err(|_| refusal("native wake metadata encoding"))?
            .len()
            > 64 * 1024
        {
            let _deferred = self.references.pop();
            if self.references.is_empty() {
                return Err(refusal("one native wake reference exceeds the input bound"));
            }
            return Ok(false);
        }
        Ok(true)
    }

    /// Nonsecret provenance frozen with the input, including each exact digest.
    pub fn metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "native_mail_wake": 1,
            "binding": self.binding.fingerprint(),
            "recipient": self.scope.recipient.as_str(),
            "messages": self.references.iter().map(|reference| serde_json::json!({
                "local_message": reference.local_message.as_str(),
                "source_message": reference.source_message.as_str(),
                "local_admission": reference.local_admission.value(),
                "digest": reference.digest.as_str(),
            })).collect::<Vec<_>>(),
        })
    }

    /// A competing capture, rebind, or supersession refuses the entire claim.
    /// The caller's transaction rolls back its input admission as well.
    pub fn consume_within(&self, tx: &GovernedTransaction<'_>) -> Result<(), WriteError> {
        self.binding.validate(tx, &self.session)?;
        for reference in &self.references {
            let record = super::require_bound(tx, &self.scope, &reference.local_message)?;
            verify_reference(&record, reference)?;
            if !record.actionable() || record.delivery_pending() {
                return Err(refusal(
                    "native wake requires settled, unconsumed actionable mail",
                ));
            }
        }
        for reference in &self.references {
            let record = super::require_bound(tx, &self.scope, &reference.local_message)?;
            if EventStore::consume_remote_mailbox_within(
                tx,
                record.scope(),
                &reference.local_message,
            )? != crate::MailboxConsumed::Consumed
            {
                return Err(refusal("native wake claim was already consumed"));
            }
        }
        Ok(())
    }
}

impl EventStore {
    /// Select at most one existing mailbox page. The service retains `after`
    /// while skipping quiet/consumed prefixes; a source change starts a rescan.
    pub fn prepare_remote_mail_wake(
        &self,
        session: &SessionId,
        scope: &RemoteMailboxScope,
        binding: MailBindingExpectation,
        after: MailboxCursor,
    ) -> Result<RemoteMailWakeScan, WriteError> {
        let page = self.read_remote_mailbox_for_binding(scope, after, 64, 4_194_304)?;
        let mut claim = RemoteMailWakeClaim {
            session: session.clone(),
            scope: scope.clone(),
            binding,
            references: Vec::new(),
        };
        let mut scanned_after = after;
        let mut overflow = false;
        for record in &page.records {
            if !record.actionable() {
                scanned_after = MailboxCursor::after(record.ack().local_admission());
                continue;
            }
            if record.delivery_pending() {
                return Err(refusal("native wake delivery acknowledgement is pending"));
            }
            if !claim.try_push(record)? {
                overflow = true;
                break;
            }
            scanned_after = MailboxCursor::after(record.ack().local_admission());
        }
        Ok(RemoteMailWakeScan {
            claim: (!claim.references.is_empty()).then_some(claim),
            after: if overflow {
                scanned_after
            } else {
                page.next_after.unwrap_or(scanned_after)
            },
            pending: overflow || page.next_after.is_some(),
        })
    }
    /// Current settled work can be admitted while unrelated original ACK debt remains.
    /// Atomic claim consumption still verifies every selected message's receipt.
    pub fn prepare_recent_remote_mail_wake(
        &self,
        session: &SessionId,
        scope: &RemoteMailboxScope,
        binding: MailBindingExpectation,
        before: Option<MailboxAdmissionSeq>,
        floor: MailboxCursor,
    ) -> Result<RemoteMailRecentWakeScan, WriteError> {
        let page = self.read_recent_remote_mailbox_above(
            scope,
            before,
            floor,
            64,
            4_194_304,
            HistoryRows::OpenScan,
        )?;
        let mut claim = RemoteMailWakeClaim {
            session: session.clone(),
            scope: scope.clone(),
            binding,
            references: Vec::new(),
        };
        let mut scanned_before = before;
        let mut overflow = false;
        for record in &page.records {
            if record.actionable() && !record.delivery_pending() && !claim.try_push(record)? {
                overflow = true;
                break;
            }
            scanned_before = Some(record.ack().local_admission());
        }
        Ok(RemoteMailRecentWakeScan {
            claim: (!claim.references.is_empty()).then_some(claim),
            before: if overflow {
                scanned_before
            } else {
                page.next_before.or(scanned_before)
            },
            pending: overflow || page.next_before.is_some(),
            top: page.top,
        })
    }
}
