//! Atomic handoff from the existing mailbox to canonical Session history.
use super::{refusal, require, require_bound, RemoteMailboxRecord, RemoteMailboxScope};
use crate::governed::GovernedTransaction;
use crate::{AppendInput, Appended, AttemptRecordCeiling, EventStore, WriteError};
use grapt_event::{mail_presented::MailPresented, AggregateId};
use grapt_types::{InstructionKey, MailboxMessageId, SessionId};

/// The nonsecret binding snapshot used to prepare the unpolled request.
/// Its value stays private and is never included in a diagnostic or event.
#[derive(Clone)]
pub struct MailBindingExpectation {
    key: InstructionKey,
    value: String,
}
impl std::fmt::Debug for MailBindingExpectation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailBindingExpectation")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}
impl MailBindingExpectation {
    /// Nonsecret identity coordinates for an admitted input's restart check.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(self.value.as_bytes()))
    }

    pub fn new(key: InstructionKey, value: String) -> Result<Self, WriteError> {
        if key.as_str().len() > 256 || value.is_empty() || value.len() > 65_536 {
            return Err(refusal("mail binding snapshot exceeds instruction bounds"));
        }
        Ok(Self { key, value })
    }
    pub(super) fn validate(
        &self,
        tx: &GovernedTransaction<'_>,
        session: &SessionId,
    ) -> Result<(), WriteError> {
        use rusqlite::OptionalExtension;
        // Compare in SQL: a corrupt oversized value is never allocated here.
        let matches: Option<bool> = tx.query_row(
            "SELECT removed=0 AND value=?3 FROM instruction_entry WHERE session_id=?1 AND key=?2",
            rusqlite::params![session.as_str(), self.key.as_str(), &self.value],
            |row| row.get(0),
        ).optional()?;
        if matches != Some(true) {
            return Err(refusal(
                "mail identity binding changed before conversation capture",
            ));
        }
        Ok(())
    }
}
pub(super) fn verify_reference(
    record: &RemoteMailboxRecord,
    reference: &grapt_event::mail_presented::PresentedMail,
) -> Result<(), WriteError> {
    if record.ack().source_message() != &reference.source_message
        || record.ack().local_admission() != reference.local_admission
        || record.ack().canonical_message_digest() != &reference.digest
    {
        return Err(refusal(
            "mail capture reference differs from original admission",
        ));
    }
    Ok(())
}
impl EventStore {
    /// Exact bounded read, including consumed history. Missing is a refusal.
    pub fn remote_mailbox_message(
        &self,
        expected: &RemoteMailboxScope,
        id: &MailboxMessageId,
    ) -> Result<RemoteMailboxRecord, WriteError> {
        require(self.connection(), expected, id)
    }
    /// Rebuild the original presentation from one canonical body per message.
    /// Current source attention/archive do not rewrite a captured conversation.
    pub fn remote_mail_presentation(
        &self,
        expected: &RemoteMailboxScope,
        presentation: &MailPresented,
    ) -> Result<Vec<RemoteMailboxRecord>, WriteError> {
        replay(self.connection(), expected, presentation)
    }
    pub fn remote_mail_presentation_within(
        tx: &GovernedTransaction<'_>,
        expected: &RemoteMailboxScope,
        presentation: &MailPresented,
    ) -> Result<Vec<RemoteMailboxRecord>, WriteError> {
        replay(tx, expected, presentation)
    }
    pub fn remote_mail_presentations_for_step(
        &self,
        session: &SessionId,
        step: grapt_types::StepNumber,
    ) -> Result<Vec<MailPresented>, WriteError> {
        presented_facts(self.connection(), session, step)
    }
    pub fn remote_mail_presentations_for_step_within(
        tx: &GovernedTransaction<'_>,
        session: &SessionId,
        step: grapt_types::StepNumber,
    ) -> Result<Vec<MailPresented>, WriteError> {
        presented_facts(tx, session, step)
    }
    /// The caller's existing reservation/permit transaction owns the commit.
    /// No async work, schema change, new mailbox or provider effect occurs here.
    pub fn append_remote_mail_presentation_within(
        tx: &GovernedTransaction<'_>,
        expected: &RemoteMailboxScope,
        binding: &MailBindingExpectation,
        input: AppendInput<'_>,
        ceiling: AttemptRecordCeiling,
    ) -> Result<Appended, WriteError> {
        if input.kind_name != "session.mail.presented" || input.version.value() != 1 {
            return Err(refusal("mail capture requires session.mail.presented v1"));
        }
        let AggregateId::Session(session) = &input.aggregate else {
            return Err(refusal("mail capture requires a Session aggregate"));
        };
        let presentation = MailPresented::decode(input.data).map_err(refusal)?;
        validate_scope(expected, &presentation)?;
        binding.validate(tx, session)?;
        let receipt = crate::attempt_receipt::read_one(tx, &presentation.attempt, ceiling)?
            .ok_or_else(|| refusal("mail capture has no reserved PhysicalAttempt"))?;
        if receipt.receipt().session() != session
            || receipt.receipt().step() != presentation.step
            || receipt.writer_epoch() != input.writer.epoch()
            || receipt.receipt().request_sent_at().is_none()
        {
            return Err(refusal(
                "mail capture does not match its permitted Session attempt",
            ));
        }
        let count = presented_count(tx, session, presentation.step)?;
        if count.saturating_add(presentation.messages().len())
            > grapt_event::mail_presented::MAX_PRESENTED_MAIL
        {
            return Err(refusal(
                "mail capture exceeds the per-Step notification bound",
            ));
        }
        // Check all rows before making the first change. A consumed row may
        // replay its existing fact, but cannot enter a second capture event.
        for reference in presentation.messages() {
            let record = require_bound(tx, expected, &reference.local_message)?;
            verify_reference(&record, reference)?;
            if !record.actionable() || record.delivery_pending() {
                return Err(refusal(
                    "mail capture requires settled, unconsumed actionable mail",
                ));
            }
        }
        for reference in presentation.messages() {
            let record = require_bound(tx, expected, &reference.local_message)?;
            Self::consume_remote_mailbox_within(tx, record.scope(), &reference.local_message)?;
        }
        Self::append_captured_mail_within(tx, input)
    }
}
fn validate_scope(
    expected: &RemoteMailboxScope,
    presentation: &MailPresented,
) -> Result<(), WriteError> {
    if presentation.projects != expected.projects
        || presentation.principal != expected.principal
        || presentation.recipient != expected.recipient
    {
        return Err(refusal(
            "mail capture scope differs from authenticated conversation binding",
        ));
    }
    Ok(())
}

fn replay(
    conn: &rusqlite::Connection,
    expected: &RemoteMailboxScope,
    presentation: &MailPresented,
) -> Result<Vec<RemoteMailboxRecord>, WriteError> {
    validate_scope(expected, presentation)?;
    let mut records = Vec::with_capacity(presentation.messages().len());
    for reference in presentation.messages() {
        let record = require_bound(conn, expected, &reference.local_message)?;
        verify_reference(&record, reference)?;
        if !record.consumed() {
            return Err(refusal("mail presentation lacks committed consumption"));
        }
        records.push(record);
    }
    Ok(records)
}
fn presented_count(
    conn: &rusqlite::Connection,
    session: &SessionId,
    step: grapt_types::StepNumber,
) -> Result<usize, WriteError> {
    Ok(presented_facts(conn, session, step)?
        .iter()
        .map(|fact| fact.messages().len())
        .sum())
}
fn presented_facts(
    conn: &rusqlite::Connection,
    session: &SessionId,
    step: grapt_types::StepNumber,
) -> Result<Vec<MailPresented>, WriteError> {
    let mut statement = conn.prepare(
        "SELECT CASE WHEN length(CAST(data AS BLOB))<=49152 THEN data ELSE NULL END,
            CASE WHEN length(type)<=256 THEN type ELSE NULL END
         FROM event WHERE aggregate_family='session' AND aggregate_id=?1
            AND type LIKE 'session.mail.presented.%'
            AND CASE WHEN json_valid(data)=0 THEN 1
                WHEN json_type(data,'$.step') IS NOT 'integer' THEN 1
                ELSE json_extract(data,'$.step')=?2 END
         ORDER BY seq LIMIT 9",
    )?;
    let mut rows = statement.query(rusqlite::params![session.as_str(), step.value()])?;
    let mut count = 0usize;
    let mut facts = Vec::new();
    while let Some(row) = rows.next()? {
        let kind: Option<String> = row.get(1)?;
        if kind.as_deref() != Some("session.mail.presented.1") {
            return Err(refusal("unsupported prior mail capture version"));
        }
        let raw: Option<String> = row.get(0)?;
        let value: serde_json::Value =
            serde_json::from_str(&raw.ok_or_else(|| refusal("oversized prior mail capture"))?)
                .map_err(|_| refusal("invalid prior mail capture JSON"))?;
        let prior = MailPresented::decode(&value).map_err(refusal)?;
        count = count.saturating_add(prior.messages().len());
        if count > grapt_event::mail_presented::MAX_PRESENTED_MAIL {
            return Err(refusal("prior mail captures exceed the per-Step bound"));
        }
        facts.push(prior);
    }
    Ok(facts)
}
