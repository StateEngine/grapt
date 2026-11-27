//! Bounded store recovery and snapshot pages. No store borrow crosses await.
use super::*;
use grapt_mail_transport::operation::OperationCause;
use grapt_mail_transport::{
    BoundRemoteSeatMailMessageV1, CursorRecovery, DeliveryAdmissionAck, PageLimits,
    RemoteSeatMailKind, ResumeRequest, TransportFailure,
};
use grapt_store::RemoteMailboxInput;
use grapt_types::SeatMessageKind;

impl<P: RemoteSeatMailTransportV1Port> RemoteMailboxRuntime<P> {
    pub(super) async fn acknowledge(
        &mut self,
        cancel: &watch::Receiver<Option<InterruptionSource>>,
        ack: DeliveryAdmissionAck,
    ) -> Result<(), RemoteMailboxError> {
        let binding = self.binding.as_ref().ok_or(RemoteMailboxError::NotLive)?;
        let outcome = io::operation(
            cancel,
            self.limits().max_ack_duration_ticks().get(),
            self.port.acknowledge_delivery(binding, ack.clone()),
        )
        .await?;
        let receipt = outcome.receipt();
        if receipt.ack_id() != ack.ack_id()
            || receipt.source_message() != ack.source_message()
            || receipt.projects() != ack.projects()
        {
            return Err(RemoteMailboxError::ForeignReceipt);
        }
        self.store()?
            .store_mut()
            .settle_remote_mailbox_delivery(
                &self.expected.for_origin(ack.projects().source().clone()),
                &ack,
                receipt.projection(),
            )
            .map_err(RemoteMailboxError::Store)
    }

    pub(super) async fn recover_page(
        &mut self,
        cancel: &watch::Receiver<Option<InterruptionSource>>,
        remaining_rows: u32,
        remaining_bytes: usize,
        result: &mut RemoteMailboxSlice,
    ) -> Result<(), RemoteMailboxError> {
        let limit = remaining_rows
            .min(self.limits().max_ack_debt_items_per_recovery_page().get())
            .min(64);
        let page = self
            .store()?
            .store()
            .read_remote_mailbox_for_binding(
                &self.expected,
                self.recovery_after,
                limit as usize,
                remaining_bytes,
            )
            .map_err(RemoteMailboxError::Store)?;
        for record in &page.records {
            io::check_cancel(cancel)?;
            if record.delivery_pending() || record.repair_pending() {
                self.acknowledge(cancel, record.ack().clone()).await?;
            }
            if record.actionable() {
                self.presentable_again(record.ack().local_admission(), result);
            }
        }
        // Charge even filtered rows conservatively: an empty output page does
        // not let another principal's rows bypass the scan/slice count bound.
        result.rows += limit;
        result.pages += 1;
        result.charged_bytes += page.decoded_bytes;
        match page.next_after {
            Some(after) => self.recovery_after = after,
            None => self.recovery_done = true,
        }
        Ok(())
    }

    pub(super) fn checkpoint_page(
        &mut self,
        remaining_rows: u32,
        remaining_bytes: usize,
        result: &mut RemoteMailboxSlice,
    ) -> Result<(), RemoteMailboxError> {
        let limit = remaining_rows.min(64);
        let page = self
            .store()?
            .store()
            .remote_mailbox_checkpoint_page_for_binding(
                &self.expected,
                self.checkpoint_before,
                limit as usize,
                remaining_bytes,
            )
            .map_err(RemoteMailboxError::Store)?;
        // Descending pages: retain the first/latest matching candidate while
        // validating every older prefix page before exposing it.
        if self.checkpoint_candidate.is_none() {
            self.checkpoint_candidate = page.candidate;
        }
        self.checkpoint_before = page.next_before;
        self.checkpoint_done = page.next_before.is_none();
        result.rows += limit;
        result.pages += 1;
        result.charged_bytes += page.decoded_bytes;
        Ok(())
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) async fn snapshot_page(
        &mut self,
        cancel: &watch::Receiver<Option<InterruptionSource>>,
        remaining_rows: u32,
        result: &mut RemoteMailboxSlice,
    ) -> Result<(), RemoteMailboxError> {
        let limits = self.limits();
        if self.subscription.is_none() {
            let binding = self.binding.as_ref().ok_or(RemoteMailboxError::NotLive)?;
            let checkpoint = if self.is_v2() {
                self.checkpoint_candidate.clone()
            } else {
                None
            };
            let resume = checkpoint
                .clone()
                .map_or_else(ResumeRequest::cold, ResumeRequest::from_checkpoint);
            let armed = io::operation(
                cancel,
                limits.max_connect_duration_ticks().get(),
                self.port.subscribe_and_snapshot(binding, resume),
            )
            .await?;
            if self
                .replay_origin
                .as_ref()
                .is_some_and(|origin| armed.snapshot().retained_origin() != origin)
            {
                return Err(RemoteMailboxError::RecoveryUnavailable);
            }
            if armed.snapshot().start_after() != checkpoint.as_ref() {
                return Err(RemoteMailboxError::ForeignReceipt);
            }
            self.replay_origin = None;
            self.source_after = checkpoint;
            self.subscription = Some(armed);
        }
        let binding = self.binding.as_ref().ok_or(RemoteMailboxError::NotLive)?;
        let snapshot = self
            .subscription
            .as_ref()
            .ok_or(RemoteMailboxError::NotLive)?
            .snapshot();
        let max_rows = remaining_rows.min(binding.receipt().limits().messages_per_page());
        let page_limit = PageLimits::new(
            max_rows,
            binding.receipt().limits().aggregate_body_bytes_per_page(),
        );
        let response = io::operation(
            cancel,
            limits.max_fetch_duration_ticks().get(),
            self.port.fetch_snapshot_page(
                binding,
                snapshot,
                self.source_after.as_ref(),
                page_limit,
            ),
        )
        .await;
        let page = match response {
            Err(RemoteMailboxError::Port(failure)) => {
                if let OperationCause::Transport(TransportFailure::CursorInvalid(invalid)) =
                    &failure.cause
                {
                    if !self.is_v2() {
                        if let CursorRecovery::ReplayFrom(origin) = invalid.recovery() {
                            self.replay_origin = Some(origin.clone());
                            self.subscription = None;
                            self.source_after = None;
                            result.pages += 1;
                            result.charged_bytes +=
                                limits.max_decoded_page_bytes_including_metadata().get();
                            return Ok(());
                        }
                    }
                }
                return Err(RemoteMailboxError::Port(failure));
            }
            other => other?,
        };
        if page.entry_count() > max_rows as usize || page.is_v2() != self.is_v2() {
            return Err(RemoteMailboxError::CursorDidNotAdvance);
        }
        if self.is_v2() {
            if page
                .last_cursor()
                .is_some_and(|cursor| self.source_after.as_ref() == Some(cursor))
            {
                return Err(RemoteMailboxError::CursorDidNotAdvance);
            }
        } else if page.next_after().is_some_and(|cursor| {
            self.source_after.as_ref() == Some(cursor)
                || page
                    .messages()
                    .last()
                    .is_none_or(|m| m.source_cursor() != cursor)
                    && page
                        .decode_failures()
                        .last()
                        .is_none_or(|f| f.cursor() != cursor)
        }) {
            return Err(RemoteMailboxError::CursorDidNotAdvance);
        }
        // All page coordinates are checked before admitting even its first row.
        for message in page.messages() {
            if message.source_binding() != binding.receipt().binding_id()
                || message.projects().target() != self.expected.projects.target()
                || message.recipient() != &self.expected.recipient
                || message.protocol().as_str()
                    != grapt_mail_transport::REMOTE_SEAT_MAIL_TRANSPORT_V1
            {
                return Err(RemoteMailboxError::ForeignReceipt);
            }
        }
        if self.is_v2() {
            if let Some(last) = page.last_cursor() {
                let inputs = page
                    .messages()
                    .iter()
                    .map(|message| input(&self.expected, message))
                    .collect::<Vec<_>>();
                let created = self
                    .progress_clock
                    .as_ref()
                    .ok_or(RemoteMailboxError::ForeignBinding)?
                    .now()
                    .as_millis();
                let original_debt = self.store()?.store_mut().record_remote_mail_page(
                    grapt_store::mailbox::remote::progress::RecordMailPage {
                        scope: &self.expected,
                        protocol: grapt_mail_transport::MailTransportVersion::V2,
                        previous: self.source_after.as_ref(),
                        checkpoint: last,
                        messages: &inputs,
                        gaps: page.refused_rows(),
                        decode_failures: page.decode_failures(),
                        created_ms: created,
                    },
                )?;
                // A re-report keeps its original admission, possibly below a
                // reader's mark, and may have changed quiet attention to wake.
                for (message, ack) in page.messages().iter().zip(&original_debt) {
                    if message.attention() == grapt_mail_transport::MailAttention::Wake {
                        self.presentable_again(ack.local_admission(), result);
                    }
                }
                if self.is_interleaved() {
                    self.delivery.observe();
                } else {
                    for ack in original_debt {
                        self.acknowledge(cancel, ack).await?;
                    }
                }
            }
        } else {
            for message in page.messages() {
                io::check_cancel(cancel)?;
                let incoming = input(&self.expected, message);
                let admitted = self
                    .store()?
                    .store_mut()
                    .admit_remote_mailbox(&incoming.scope, &incoming)
                    .map_err(RemoteMailboxError::Store)?;
                let admission = admitted.ack.local_admission();
                self.acknowledge(cancel, admitted.ack).await?;
                if message.attention() == grapt_mail_transport::MailAttention::Wake {
                    self.presentable_again(admission, result);
                }
            }
        }
        self.decode_failures = self
            .decode_failures
            .checked_add(
                u64::try_from(page.decode_failures().len())
                    .map_err(|_| RemoteMailboxError::SliceTooSmall)?,
            )
            .ok_or(RemoteMailboxError::SliceTooSmall)?;
        self.remember_decode_failure_positions(
            page.decode_failures()
                .iter()
                .map(|failure| failure.cursor().clone()),
        );
        result.pages += 1;
        result.rows +=
            u32::try_from(page.entry_count()).map_err(|_| RemoteMailboxError::SliceTooSmall)?;
        result.charged_bytes += limits.max_decoded_page_bytes_including_metadata().get();
        if self.is_v2() {
            if let Some(last) = page.last_cursor() {
                self.integrity_gaps = self
                    .integrity_gaps
                    .checked_add(
                        u64::try_from(page.refused_rows().len())
                            .map_err(|_| RemoteMailboxError::SliceTooSmall)?,
                    )
                    .ok_or(RemoteMailboxError::SliceTooSmall)?;
                self.checkpoint_candidate = Some(last.clone());
                self.source_after = Some(last.clone());
            }
        } else {
            self.source_after = page.next_after().cloned();
        }
        self.snapshot_done = page.next_after().is_none();
        Ok(())
    }
}

pub(super) fn input(
    expected: &RemoteMailboxScope,
    message: &BoundRemoteSeatMailMessageV1,
) -> RemoteMailboxInput {
    let kind = match message.kind() {
        RemoteSeatMailKind::TaskPointer => SeatMessageKind::TaskPointer,
        RemoteSeatMailKind::Question => SeatMessageKind::Question,
        RemoteSeatMailKind::Blocker => SeatMessageKind::Blocker,
        RemoteSeatMailKind::Done => SeatMessageKind::Done,
        RemoteSeatMailKind::Handoff => SeatMessageKind::Handoff,
        RemoteSeatMailKind::Finding => SeatMessageKind::Finding,
        RemoteSeatMailKind::RulingRequest => SeatMessageKind::RulingRequest,
        RemoteSeatMailKind::Ruling => SeatMessageKind::Ruling,
        RemoteSeatMailKind::Other(label) => SeatMessageKind::Other(label.clone()),
    };
    RemoteMailboxInput {
        scope: expected.for_origin(message.projects().source().clone()),
        protocol: message.protocol().clone(),
        source_message: message.source_message().clone(),
        source_binding: message.source_binding().clone(),
        source_cursor: message.source_cursor().clone(),
        sender: message.sender().clone(),
        kind,
        body: message.body().clone(),
        source_created: message.source_created().clone(),
        refs: message
            .refs()
            .as_slice()
            .iter()
            .map(|value| value.as_str().to_owned())
            .collect(),
        supersedes: message.supersession().supersedes().cloned(),
        superseded_by: message.supersession().superseded_by().cloned(),
        archive: message.archive(),
        attention: message.attention(),
    }
}
