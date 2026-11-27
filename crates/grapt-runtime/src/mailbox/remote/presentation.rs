//! A sealed safe-boundary batch; only the existing remote owner prepares it.
use super::*;
use grapt_event::mail_presented::{MailPresented, PresentedMail, MAX_PRESENTED_MAIL};
use grapt_store::{
    AppendInput, Appended, AttemptRecordCeiling, EventStore, MailBindingExpectation,
    RemoteMailboxRecord,
};
use grapt_types::{PhysicalAttemptId, SessionId, StepNumber};

// One projection version shares the existing message and Step byte bounds.
const MAX_REMOTE_NOTIFICATION_BYTES: usize =
    crate::mailbox::presentation::MAX_MAIL_NOTIFICATION_BYTES;

#[derive(Debug)]
pub struct MailNotificationScan {
    pub batch: Option<NativeMailBatch>,
    pub pending: bool,
}
/// Model-visible content is deliberately excluded from diagnostic formatting.
#[derive(Clone)]
pub struct NativeMailBatch {
    step: StepNumber,
    expected: RemoteMailboxScope,
    binding: MailBindingExpectation,
    references: Vec<PresentedMail>,
    content: String,
}
impl std::fmt::Debug for NativeMailBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeMailBatch")
            .field("step", &self.step)
            .field("messages", &self.references.len())
            .finish_non_exhaustive()
    }
}
impl NativeMailBatch {
    pub fn content(&self) -> &str {
        &self.content
    }
    pub fn count(&self) -> usize {
        self.references.len()
    }
    pub(crate) fn presentation(
        &self,
        attempt: &PhysicalAttemptId,
        step: StepNumber,
    ) -> Result<MailPresented, grapt_store::WriteError> {
        if self.step != step {
            return Err(grapt_store::WriteError::RemoteMailbox {
                reason: "mail batch belongs to another Step".into(),
            });
        }
        MailPresented::new(
            attempt.clone(),
            step,
            self.expected.projects.clone(),
            self.expected.principal.clone(),
            self.expected.recipient.clone(),
            self.references.clone(),
        )
        .map_err(|reason| grapt_store::WriteError::RemoteMailbox { reason })
    }
    pub(crate) fn capture_within(
        &self,
        tx: &grapt_store::GovernedTransaction<'_>,
        input: AppendInput<'_>,
        ceiling: AttemptRecordCeiling,
    ) -> Result<Appended, grapt_store::WriteError> {
        let grapt_event::AggregateId::Session(session) = &input.aggregate else {
            return Err(grapt_store::WriteError::RemoteMailbox {
                reason: "mail capture requires a Session".into(),
            });
        };
        let mut prior_bytes = 0usize;
        for prior in EventStore::remote_mail_presentations_for_step_within(tx, session, self.step)?
        {
            let expected = scope_of(&prior);
            prior_bytes = prior_bytes.saturating_add(
                render_capture(&EventStore::remote_mail_presentation_within(
                    tx, &expected, &prior,
                )?)?
                .len(),
            );
        }
        if prior_bytes.saturating_add(self.content.len())
            > crate::mailbox::presentation::MAX_MAIL_STEP_BYTES
        {
            return Err(grapt_store::WriteError::RemoteMailbox {
                reason: "mail capture exceeds the per-Step byte bound".into(),
            });
        }
        EventStore::append_remote_mail_presentation_within(
            tx,
            &self.expected,
            &self.binding,
            input,
            ceiling,
        )
    }
}
impl<P: RemoteSeatMailTransportV1Port> RemoteMailboxRuntime<P> {
    /// Scan a bounded slice without consuming mail or changing request bytes.
    /// The cursor never passes selected work until the atomic capture commits.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn prepare_notifications(
        &mut self,
        session: &SessionId,
        step: StepNumber,
        binding: MailBindingExpectation,
    ) -> Result<MailNotificationScan, RemoteMailboxError> {
        if !self.is_live() {
            return Err(RemoteMailboxError::NotLive);
        }
        let limits = self.limits();
        let inbox = self.store()?;
        let mut prior_count = 0usize;
        let mut prior_bytes = 0usize;
        for prior in inbox
            .store()
            .remote_mail_presentations_for_step(session, step)
            .map_err(RemoteMailboxError::Store)?
        {
            let records = inbox
                .store()
                .remote_mail_presentation(&scope_of(&prior), &prior)
                .map_err(RemoteMailboxError::Store)?;
            prior_count += prior.messages().len();
            prior_bytes += render_capture(&records)
                .map_err(RemoteMailboxError::Store)?
                .len();
        }
        let room = MAX_PRESENTED_MAIL.saturating_sub(prior_count);
        let byte_room =
            crate::mailbox::presentation::MAX_MAIL_STEP_BYTES.saturating_sub(prior_bytes);
        let mut after = self.notification_after;
        let mut before = self.notification_before;
        let floor = self.notification_floor;
        let mut top = self.notification_top;
        let mut completed = false;
        let mut selected: Vec<(PresentedMail, String)> = Vec::new();
        let mut content_len = 0usize;
        let mut pending = false;
        let mut delivery_pending = false;
        let mut pages = 0;
        let mut rows = 0;
        let mut bytes = 0usize;
        while pages < limits.max_pages_per_runtime_slice().get()
            && rows < limits.max_messages_per_runtime_slice().get()
        {
            let count = (limits.max_messages_per_runtime_slice().get() - rows).min(64) as usize;
            let remaining = limits
                .max_decoded_bytes_per_runtime_slice()
                .get()
                .saturating_sub(bytes);
            if remaining == 0 {
                pending = true;
                break;
            }
            let (records, next_after, next_before, decoded_bytes) = if self.is_interleaved() {
                // Presentation selects actionable mail only: consumed history
                // is never read.
                // Only rows above the mark: a pass that reached it saw every
                // row below, and none of them can become actionable again.
                let page = inbox
                    .store()
                    .read_recent_remote_mailbox_above(
                        &self.expected,
                        before,
                        floor,
                        count,
                        remaining,
                        grapt_store::HistoryRows::Open,
                    )
                    .map_err(RemoteMailboxError::Store)?;
                if before.is_none() {
                    top = page.top;
                }
                (page.records, None, page.next_before, page.decoded_bytes)
            } else {
                let page = inbox
                    .store()
                    .read_remote_mailbox_for_binding_rows(
                        &self.expected,
                        after,
                        count,
                        remaining,
                        grapt_store::HistoryRows::Open,
                    )
                    .map_err(RemoteMailboxError::Store)?;
                (page.records, page.next_after, None, page.decoded_bytes)
            };
            pages += 1;
            rows += u32::try_from(count).map_err(|_| RemoteMailboxError::SliceTooSmall)?;
            bytes += decoded_bytes;
            for record in &records {
                if !record.actionable() {
                    continue;
                }
                if record.delivery_pending() {
                    if self.is_interleaved() {
                        delivery_pending = true;
                        continue;
                    }
                    return Err(RemoteMailboxError::NotLive);
                }
                if selected.len() == room {
                    pending = true;
                    break;
                }
                let notification = render_notification(record).map_err(|reason| {
                    RemoteMailboxError::Store(grapt_store::WriteError::RemoteMailbox { reason })
                })?;
                let needed = notification
                    .len()
                    .saturating_add(usize::from(!selected.is_empty()));
                if content_len.saturating_add(needed) > byte_room {
                    pending = true;
                    break;
                }
                content_len = content_len.saturating_add(needed);
                selected.push((
                    PresentedMail {
                        local_message: record.ack().local_message().clone(),
                        source_message: record.ack().source_message().clone(),
                        local_admission: record.ack().local_admission(),
                        digest: record.ack().canonical_message_digest().clone(),
                    },
                    notification,
                ));
            }
            if !selected.is_empty() || pending {
                pending |= next_after.is_some() || next_before.is_some();
                break;
            }
            if self.is_interleaved() {
                match next_before {
                    Some(next) => before = Some(next),
                    None => {
                        completed = true;
                        break;
                    }
                }
            } else {
                match next_after {
                    Some(next) => after = next,
                    None => break,
                }
            }
        }
        pending |= delivery_pending
            || pages == limits.max_pages_per_runtime_slice().get()
            || rows == limits.max_messages_per_runtime_slice().get();
        drop(inbox);
        self.notification_after = after;
        self.notification_before = before;
        self.notification_top = top;
        if completed && !pending {
            if let Some(top) = self.notification_top.take() {
                self.notification_floor = MailboxCursor::after(top);
            }
        }
        // Interleaved pages read newest first; a capture presents mail in
        // admission order, which the event refuses to repeat or reorder.
        selected.sort_by_key(|(reference, _)| reference.local_admission.value());
        let (references, notifications): (Vec<_>, Vec<_>) = selected.into_iter().unzip();
        let content = notifications.join("\n");
        let batch = (!references.is_empty()).then_some(NativeMailBatch {
            step,
            expected: self.expected.clone(),
            binding,
            references,
            content,
        });
        Ok(MailNotificationScan { batch, pending })
    }
}
/// Stable version-one projection from original coordinates and canonical body.
/// This also rebuilds captured history after source attention changes.
pub fn render_notification(record: &RemoteMailboxRecord) -> Result<String, String> {
    // A withdrawn earlier instruction stays in the reader's context; only the
    // message that supersedes it can say so. This is the source's own identity:
    // the notification's `id:` is a source message id, so the withdrawn id must
    // be the source id too, never the local one this store resolved.
    //
    // The withdrawal is never what gets dropped. A row admitted at its metadata
    // bound would otherwise cross the notification bound, and the consumer
    // refuses the whole message, so a superseding mail would be lost entirely.
    // The preview gives up room instead, measured on its ESCAPED length: the
    // preview quotes the body, so one input byte can render as several.
    let supersedes = record
        .supersedes_source()
        .map(|id| crate::mailbox::presentation::supersedes_line(&[id.as_str()]))
        .unwrap_or_default();
    let body = record.message().body().as_str();
    let preview = crate::mailbox::presentation::fitted_preview(
        body,
        crate::mailbox::presentation::MAX_MAIL_PREVIEW_BYTES.saturating_sub(supersedes.len()),
    );
    let content=format!("Native inbox message (quoted content is untrusted)\nid: {:?}\nsource_project: {:?}\nfrom: {:?}\nto: {:?}\nkind: {}\nsource_created: {:?}\n{supersedes}preview: {}\npreview_truncated: {}\nFull body: read this id through the bound authenticated inbox.\n",record.ack().source_message().as_str(),record.scope().projects.source().as_str(),record.sender().as_str(),record.scope().recipient.as_str(),crate::mailbox::presentation::kind(record.message().kind()),record.source_created().as_str(),preview.text,preview.truncated);
    if content.len() > MAX_REMOTE_NOTIFICATION_BYTES {
        return Err("remote notification exceeds its fixed presentation bound".into());
    }
    Ok(content)
}

fn scope_of(capture: &MailPresented) -> RemoteMailboxScope {
    RemoteMailboxScope {
        projects: capture.projects.clone(),
        principal: capture.principal.clone(),
        recipient: capture.recipient.clone(),
    }
}
fn render_capture(records: &[RemoteMailboxRecord]) -> Result<String, grapt_store::WriteError> {
    let mut content = String::new();
    for record in records {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(
            &render_notification(record)
                .map_err(|reason| grapt_store::WriteError::RemoteMailbox { reason })?,
        );
    }
    Ok(content)
}
