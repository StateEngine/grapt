//! Original durable delivery debt shares the owner's serial operation slot.
use super::*;
use grapt_mail_transport::DeliveryAdmissionAck;
use grapt_store::HistoryRows;

/// Where a delivery candidate was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Walk {
    /// Newest-first over unconsumed rows.
    Recent,
    /// Unacknowledged delivery, found by SQL.
    Debt,
    /// The oldest-first walk for repair debt, above its high-water mark.
    History,
    /// A row whose repair was still due after its acknowledgement.
    Retry,
}

/// One page of delivery candidates and where it came from.
struct DeliveryPage {
    records: Vec<grapt_store::RemoteMailboxRecord>,
    next_after: Option<MailboxCursor>,
    next_before: Option<MailboxAdmissionSeq>,
    bytes: usize,
    walk: Walk,
    top: Option<MailboxAdmissionSeq>,
}

#[derive(Debug, Clone)]
struct PendingDelivery {
    ack: DeliveryAdmissionAck,
    walk: Walk,
    actionable: bool,
    restart_recent: bool,
}

#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub(super) struct DeliveryProgress {
    recent_before: Option<MailboxAdmissionSeq>,
    recent_done: bool,
    /// The recent walk reads only rows above this mark.
    recent_floor: MailboxCursor,
    /// The newest row of the recent pass in progress.
    recent_top: Option<MailboxAdmissionSeq>,
    history_after: MailboxCursor,
    history_done: bool,
    recent_turn: bool,
    repair_seen: bool,
    /// Rows whose repair was still due after their acknowledgement, retried
    /// once per observation without walking history again.
    repair_due: std::collections::BTreeMap<MailboxAdmissionSeq, DeliveryAdmissionAck>,
    /// This observation's retries.
    repair_now: std::collections::BTreeMap<MailboxAdmissionSeq, DeliveryAdmissionAck>,
    pending: Option<PendingDelivery>,
    pub(super) remaining: bool,
}
impl Default for DeliveryProgress {
    fn default() -> Self {
        Self {
            recent_before: None,
            recent_done: false,
            recent_floor: MailboxCursor::START,
            recent_top: None,
            history_after: MailboxCursor::START,
            history_done: false,
            recent_turn: true,
            repair_seen: false,
            repair_due: std::collections::BTreeMap::new(),
            repair_now: std::collections::BTreeMap::new(),
            pending: None,
            remaining: true,
        }
    }
}
impl DeliveryProgress {
    pub(super) fn observe(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.restart_recent = true;
        }
        self.recent_before = None;
        self.recent_done = false;
        // The recent walk stops at its mark: a row it passed with nothing to
        // deliver is never undelivered again, and later debt is found by SQL.
        self.recent_top = None;
        // The history walk resumes from its high-water mark: rows below it were
        // walked once this runtime started, and new rows can only arrive above.
        if self.history_done {
            self.repair_seen = false;
        }
        let due = std::mem::take(&mut self.repair_due);
        self.repair_now.extend(due);
        self.history_done = false;
        self.remaining = true;
    }
    /// The next of this observation's repair retries, on a history turn. It
    /// stays queued until it is saved as pending, so a cancel never loses it.
    fn next_retry(&self, recent: bool) -> Option<PendingDelivery> {
        if recent || self.pending.is_some() {
            return None;
        }
        self.repair_now
            .values()
            .next()
            .cloned()
            .map(|ack| PendingDelivery {
                ack,
                walk: Walk::Retry,
                actionable: false,
                restart_recent: false,
            })
    }
    /// A new binding or a closed transport: every walk starts over.
    pub(super) fn forget_marks(&mut self) {
        self.observe();
        self.recent_floor = MailboxCursor::START;
    }
    /// Move the selected walk past a settled row.
    fn settled(&mut self, selected: &PendingDelivery, admission: MailboxAdmissionSeq) {
        match selected.walk {
            Walk::Recent => {
                self.recent_before = if selected.restart_recent {
                    None
                } else {
                    Some(admission)
                };
            }
            Walk::History => self.history_after = MailboxCursor::after(admission),
            // The repair walk keeps its own position.
            Walk::Debt | Walk::Retry => {}
        }
        self.pending = None;
    }
    fn work_pending(&self) -> bool {
        self.pending.is_some()
            || !self.recent_done
            || !self.history_done
            || !self.repair_now.is_empty()
    }
    /// Save the operation's exact coordinates. A retry leaves the queue by its
    /// own identity here, never by position: re-saving it removes nothing else.
    fn save_pending(&mut self, selected: &PendingDelivery) {
        self.pending = Some(selected.clone());
        if selected.walk == Walk::Retry {
            self.repair_now.remove(&selected.ack.local_admission());
        }
    }
}

impl<P: RemoteSeatMailTransportV1Port> RemoteMailboxRuntime<P> {
    /// Finite background work is distinct from unresolved secondary repair debt.
    pub fn delivery_work_pending(&self) -> bool {
        self.is_interleaved() && self.delivery.work_pending()
    }
    pub fn delivery_debt_pending(&self) -> bool {
        self.is_interleaved() && self.delivery.remaining
    }

    /// One page of delivery candidates: open rows newest-first, else delivery
    /// debt by SQL, else the repair walk above its high-water mark.
    fn delivery_page(
        &self,
        recent: bool,
        limit: usize,
        max_bytes: usize,
    ) -> Result<DeliveryPage, RemoteMailboxError> {
        let inbox = self.store()?;
        if recent {
            // A recent candidate must be actionable, so consumed rows never matter.
            let page = inbox.store().read_recent_remote_mailbox_above(
                &self.expected,
                self.delivery.recent_before,
                self.delivery.recent_floor,
                limit,
                max_bytes,
                HistoryRows::OpenScan,
            )?;
            return Ok(DeliveryPage {
                records: page.records,
                next_after: None,
                next_before: page.next_before,
                bytes: page.decoded_bytes,
                walk: Walk::Recent,
                top: page.top,
            });
        }
        // Unacknowledged delivery is found by SQL; only repair debt, which
        // lives in the record, needs the walk.
        let debt = inbox.store().read_remote_mailbox_for_binding_rows(
            &self.expected,
            MailboxCursor::START,
            limit,
            max_bytes,
            HistoryRows::Debt,
        )?;
        if debt.records.iter().any(|record| record.delivery_pending()) {
            return Ok(DeliveryPage {
                records: debt.records,
                next_after: None,
                next_before: None,
                bytes: debt.decoded_bytes,
                walk: Walk::Debt,
                top: None,
            });
        }
        let page = inbox.store().read_remote_mailbox_for_binding_rows(
            &self.expected,
            self.delivery.history_after,
            limit,
            max_bytes,
            HistoryRows::Scan,
        )?;
        Ok(DeliveryPage {
            records: page.records,
            next_after: page.next_after,
            next_before: None,
            bytes: debt.decoded_bytes + page.decoded_bytes,
            walk: Walk::History,
            top: None,
        })
    }

    /// How an older row becomes presentable again: its delivery was witnessed,
    /// a recovery settled it, or the source re-reported it as wake mail in
    /// place. Readers' marks drop just below it, so their next pass sees it.
    pub(super) fn presentable_again(
        &mut self,
        admission: MailboxAdmissionSeq,
        result: &mut RemoteMailboxSlice,
    ) {
        self.notification_before = None;
        self.notification_floor = self.notification_floor.min(below(admission));
        result.rescan_notifications = true;
        result.delivered_from = Some(
            result
                .delivered_from
                .map_or(admission, |from| from.min(admission)),
        );
    }

    pub(super) async fn settle_delivery_slice(
        &mut self,
        cancel: &watch::Receiver<Option<InterruptionSource>>,
        result: &mut RemoteMailboxSlice,
    ) -> Result<(), RemoteMailboxError> {
        if !self.delivery.work_pending() {
            return Ok(());
        }
        let recent = self.delivery.pending.as_ref().map_or(
            !self.delivery.recent_done && (self.delivery.recent_turn || self.delivery.history_done),
            |pending| pending.walk == Walk::Recent,
        );
        let limit = self.limits().max_messages_per_runtime_slice().get().min(64);
        let max_bytes = self.limits().max_decoded_bytes_per_runtime_slice().get();
        let DeliveryPage {
            records,
            next_after,
            next_before,
            bytes,
            walk,
            top,
        } = self.delivery_page(recent, limit as usize, max_bytes)?;
        if recent && self.delivery.recent_before.is_none() {
            self.delivery.recent_top = top;
        }
        result.pages += 1;
        result.rows += limit;
        result.charged_bytes += bytes;
        let retry = self.delivery.next_retry(recent);
        let selected = self.delivery.pending.clone().or(retry).or_else(|| {
            records
                .iter()
                .find(|record| {
                    (!recent || record.actionable())
                        && (record.delivery_pending() || record.repair_pending())
                })
                .map(|record| PendingDelivery {
                    ack: record.ack().clone(),
                    walk,
                    actionable: record.actionable(),
                    restart_recent: false,
                })
        });
        if let Some(selected) = selected {
            io::check_cancel(cancel)?;
            self.delivery.save_pending(&selected);
            // A walk candidate was not hash-verified: acknowledge only the
            // coordinates of the row re-read in full.
            let ack = self
                .store()?
                .store()
                .remote_mailbox_message(
                    &self
                        .expected
                        .for_origin(selected.ack.projects().source().clone()),
                    selected.ack.local_message(),
                )?
                .ack()
                .clone();
            let admission = ack.local_admission();
            // An unfinished operation retains its exact original coordinates
            // and selection across observation, cancellation and reconnect.
            self.acknowledge(cancel, ack.clone()).await?;
            let repair = self
                .store()?
                .store()
                .remote_mailbox_message(
                    &self.expected.for_origin(ack.projects().source().clone()),
                    ack.local_message(),
                )?
                .repair_pending();
            self.delivery.repair_seen |= repair;
            if repair {
                self.delivery.repair_due.insert(admission, ack.clone());
            } else {
                self.delivery.repair_due.remove(&admission);
                self.delivery.repair_now.remove(&admission);
            }
            self.delivery.settled(&selected, admission);
            if selected.actionable {
                self.presentable_again(admission, result);
            }
        } else if recent {
            self.delivery.recent_before = next_before;
            self.delivery.recent_done = next_before.is_none();
            if self.delivery.recent_done {
                if let Some(top) = self.delivery.recent_top.take() {
                    self.delivery.recent_floor = MailboxCursor::after(top);
                }
            }
        } else {
            if let Some(after) = next_after {
                self.delivery.history_after = after;
            }
            self.delivery.history_done = next_after.is_none();
            if self.delivery.history_done {
                self.delivery.remaining = self.delivery.repair_seen;
            }
        }
        self.delivery.recent_turn = !recent;
        // A local-only debt scan must yield before the aggregate service driver
        // chooses its next owner, even when no transport operation was needed.
        tokio::task::yield_now().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_mail_transport::DeliveryAdmissionAckParts;
    use grapt_types::remote_mail::*;

    fn due_ack() -> Result<DeliveryAdmissionAck, String> {
        due_ack_at(40)
    }

    fn due_ack_at(admission: u64) -> Result<DeliveryAdmissionAck, String> {
        let text = |error: IdentityError| error.to_string();
        Ok(DeliveryAdmissionAck::new(DeliveryAdmissionAckParts {
            ack_id: DeliveryAdmissionAckId::try_new("delivery:remote-mail:due").map_err(text)?,
            source_binding: SourceBindingId::try_new("binding").map_err(text)?,
            projects: ProjectPair::new(
                SourceProjectId::try_new("source").map_err(text)?,
                TargetProjectId::try_new("target").map_err(text)?,
            ),
            source_message: RemoteMailboxMessageId::try_new("due").map_err(text)?,
            source_cursor: ExclusiveSourceCursor::try_new("cursor-due").map_err(text)?,
            recipient: grapt_types::SeatRecipientId::try_new("coder").map_err(|e| e.to_string())?,
            local_message: grapt_types::MailboxMessageId::try_new("remote-mail:due")
                .map_err(|e| e.to_string())?,
            local_admission: MailboxAdmissionSeq::new(admission).ok_or("seq")?,
            canonical_message_digest: MailboxMessageDigest::try_new("digest").map_err(text)?,
            protocol: grapt_types::ProtocolVersion::new("remote-seat-mail-transport/v1")
                .ok_or("protocol")?,
        }))
    }

    #[test]
    fn an_observation_resumes_the_history_walk_above_its_high_water_mark() -> Result<(), String> {
        let walked = MailboxCursor::after(MailboxAdmissionSeq::new(40).ok_or("seq")?);
        let mut progress = DeliveryProgress {
            history_after: walked,
            history_done: true,
            ..DeliveryProgress::default()
        };
        progress.observe();
        assert_eq!(
            progress.history_after, walked,
            "old history is never walked again"
        );
        assert!(!progress.history_done, "rows above the mark are still read");
        Ok(())
    }

    #[test]
    fn an_observation_retries_due_repair_without_walking_history() -> Result<(), String> {
        let ack = due_ack()?;
        let walked = MailboxCursor::after(MailboxAdmissionSeq::new(100).ok_or("seq")?);
        let mut progress = DeliveryProgress {
            history_after: walked,
            history_done: true,
            ..DeliveryProgress::default()
        };
        progress
            .repair_due
            .insert(ack.local_admission(), ack.clone());
        progress.observe();
        assert_eq!(
            progress.repair_now.values().collect::<Vec<_>>(),
            [&ack],
            "the due repair is retried"
        );
        assert!(progress.repair_due.is_empty());
        assert_eq!(
            progress.history_after, walked,
            "history is not walked again"
        );
        Ok(())
    }

    #[test]
    fn a_retry_leaves_the_queue_only_by_its_own_identity() -> Result<(), String> {
        let first = due_ack()?;
        let second = due_ack_at(41)?;
        let mut progress = DeliveryProgress::default();
        for ack in [&first, &second] {
            progress
                .repair_now
                .insert(ack.local_admission(), ack.clone());
        }
        // A cancel between choosing and saving the retry must not lose it.
        let chosen = progress.next_retry(false).ok_or("a retry")?;
        assert_eq!(chosen.ack, first);
        assert_eq!(progress.repair_now.len(), 2, "still queued");
        assert!(
            progress.next_retry(true).is_none(),
            "only history turns retry"
        );
        // Saved: only that row leaves. Re-saving it after a lost response
        // removes nothing else.
        progress.save_pending(&chosen);
        progress.save_pending(&chosen);
        assert_eq!(progress.repair_now.values().collect::<Vec<_>>(), [&second]);
        Ok(())
    }

    #[test]
    fn an_observation_keeps_the_recent_mark_and_a_reset_clears_it() -> Result<(), String> {
        let seq = |value| MailboxAdmissionSeq::new(value).ok_or("seq");
        let mark = MailboxCursor::after(seq(40)?);
        let mut progress = DeliveryProgress {
            recent_before: Some(seq(45)?),
            recent_done: true,
            recent_floor: mark,
            recent_top: Some(seq(50)?),
            ..DeliveryProgress::default()
        };
        progress.observe();
        assert_eq!(
            progress.recent_floor, mark,
            "an advisory never re-reads below the mark"
        );
        assert_eq!((progress.recent_before, progress.recent_top), (None, None));
        assert!(
            !progress.recent_done,
            "the walk restarts from the newest row"
        );
        progress.forget_marks();
        assert_eq!(progress.recent_floor, MailboxCursor::START);
        // A witnessed delivery lowers a mark to just below its row.
        assert_eq!(below(seq(41)?), mark);
        assert_eq!(below(MailboxAdmissionSeq::FIRST), MailboxCursor::START);
        Ok(())
    }
}
