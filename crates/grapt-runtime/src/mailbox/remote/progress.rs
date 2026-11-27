//! Same-version restart scans local page facts before any fresh enumeration.
use super::*;
use grapt_mail_transport::MailTransportVersion;
impl<P: RemoteSeatMailTransportV1Port> RemoteMailboxRuntime<P> {
    pub(super) fn prepare_progress(&mut self) -> Result<(), RemoteMailboxError> {
        if self.progress_prepared {
            return Ok(());
        }
        if self.is_v2() {
            if self.progress_clock.is_none() {
                return Err(RemoteMailboxError::ForeignBinding);
            }
            self.store()?
                .store_mut()
                .prepare_remote_mail_progress(&self.expected, MailTransportVersion::V2)?;
        } else {
            self.store()?
                .store()
                .require_v1_mail_context(&self.expected)?;
        }
        self.progress_prepared = true;
        Ok(())
    }
    pub(super) async fn recover_progress(
        &mut self,
        cancel: &watch::Receiver<Option<InterruptionSource>>,
        remaining_rows: u32,
        remaining_bytes: usize,
        result: &mut RemoteMailboxSlice,
    ) -> Result<(), RemoteMailboxError> {
        if result.pages > 0 && remaining_rows < self.limits().settle().messages_per_page() {
            result.pages = self.limits().max_pages_per_runtime_slice().get();
            return Ok(());
        }
        let page = match self.store()?.store().remote_mail_progress_page(
            &self.expected,
            MailTransportVersion::V2,
            self.progress_after,
            remaining_rows,
            remaining_bytes,
        ) {
            Ok(page) => page,
            Err(grapt_store::WriteError::RemoteMailProgressBudget) => {
                if result.pages == 0 {
                    return Err(RemoteMailboxError::SliceTooSmall);
                }
                result.pages = self.limits().max_pages_per_runtime_slice().get();
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let Some(page) = page else {
            self.recovery_done = true;
            self.checkpoint_done = true;
            result.pages += 1;
            return Ok(());
        };
        let fact = &page.recorded.fact;
        let count = fact
            .messages()
            .len()
            .saturating_add(fact.gaps().len())
            .saturating_add(fact.decode_failures().len());
        if count > remaining_rows as usize {
            if result.pages == 0 {
                return Err(RemoteMailboxError::SliceTooSmall);
            }
            result.pages = self.limits().max_pages_per_runtime_slice().get();
            return Ok(());
        }
        if fact.previous.as_ref() != self.checkpoint_candidate.as_ref() {
            return Err(RemoteMailboxError::RecoveryUnavailable);
        }
        for record in page.recovery {
            io::check_cancel(cancel)?;
            let admission = record.ack.local_admission();
            // The verified page prefix is independent of delivery settlement.
            // All original coordinates remain in the canonical mailbox records.
            if self.is_interleaved() {
                self.delivery.remaining |= record.delivery_pending || record.repair_pending;
            } else if record.delivery_pending || record.repair_pending {
                self.acknowledge(cancel, record.ack).await?;
            }
            if record.actionable {
                self.presentable_again(admission, result);
            }
        }
        self.integrity_gaps = self
            .integrity_gaps
            .checked_add(
                u64::try_from(fact.gaps().len()).map_err(|_| RemoteMailboxError::SliceTooSmall)?,
            )
            .ok_or(RemoteMailboxError::SliceTooSmall)?;
        self.decode_failures = self
            .decode_failures
            .checked_add(
                u64::try_from(fact.decode_failures().len())
                    .map_err(|_| RemoteMailboxError::SliceTooSmall)?,
            )
            .ok_or(RemoteMailboxError::SliceTooSmall)?;
        self.remember_decode_failure_positions(
            fact.decode_failures()
                .iter()
                .map(|failure| failure.cursor().clone()),
        );
        self.checkpoint_candidate = Some(fact.checkpoint.clone());
        self.progress_after = Some(page.recorded.seq);
        if !page.has_more {
            self.recovery_done = true;
            self.checkpoint_done = true;
        }
        result.rows += u32::try_from(count).map_err(|_| RemoteMailboxError::SliceTooSmall)?;
        result.pages += 1;
        result.charged_bytes += page.decoded_bytes;
        Ok(())
    }
}
