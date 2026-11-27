//! Prompt preparation borrows the retained owner; it never replaces a refused binding.
use super::*;

impl NativeMailPlan {
    /// Private fixture ownership uses the same runtime and explicit close seam.
    pub(in crate::dogfood) fn private(context: NativeMailContext) -> Self {
        Self {
            external_host: false,
            owner: Owner::new(context, Arc::new(Notify::new())),
            lease: None,
            planning: None,
            service_owned: false,
            degraded: None,
            cleanup_owed: false,
        }
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(in crate::dogfood) async fn refresh(&mut self, step: StepNumber) -> Result<(), E> {
        if self.external_host {
            return Ok(());
        }
        self.degraded = None;
        self.cleanup_owed = false;
        self.lease = None;
        self.planning = None;
        if self.owner.planning.fetch_add(1, Ordering::SeqCst) >= OWNER_CAP {
            self.owner.planning.fetch_sub(1, Ordering::SeqCst);
            return Err(E::Incomplete);
        }
        self.planning = Some(Planning(self.owner.clone()));
        self.owner.changed.notify_one();
        if !self.service_owned {
            self.owner.cancel_wait().map_err(|_| E::Transport)?;
        }
        self.lease = Some(self.owner.context.clone().lock_owned().await);
        let revision = self.owner.revision().map_err(|_| E::Binding)?;
        let context = self.lease.as_mut().ok_or(E::Incomplete)?;
        if self.owner.superseded.load(Ordering::SeqCst)
            || !context
                .inbox
                .lock()
                .map_err(|_| E::Binding)?
                .native_mail_wakes_available(&context.session)
                .map_err(|_| E::Binding)?
        {
            context.batch = None;
            context.prepared = true;
            self.owner.cache_attention(context);
            return Ok(());
        }
        if context.requires_rebind()
            || context
                .blocked_revision
                .as_ref()
                .is_some_and(|blocked| Arc::ptr_eq(blocked, &revision))
        {
            context.blocked_revision = Some(revision.clone());
            context.verified_revision = Some(revision);
            context.batch = None;
            context.prepared = true;
            context.attention = context.blocked_attention();
            self.owner.cache_attention(context);
            if context.credential.refusal() == Some(RefusalCause::Principal) {
                return Err(E::Principal);
            }
            return Ok(());
        }
        if context.close_pending
            || self
                .owner
                .recovery
                .lock()
                .map_err(|_| E::Transport)?
                .waiting()
        {
            context.batch = None;
            context.prepared = true;
            context.attention = if context.slow_stopped {
                MailAttentionInfo::Stopped
            } else if self
                .owner
                .recovery
                .lock()
                .map_err(|_| E::Transport)?
                .recovering()
            {
                MailAttentionInfo::Retrying
            } else {
                MailAttentionInfo::Unavailable
            };
            self.owner.cache_attention(context);
            return Ok(());
        }
        context.blocked_revision = None;
        if context.attention == MailAttentionInfo::RebindRequired {
            context.attention = MailAttentionInfo::Unavailable;
        }
        // Retained preparation reads a settled snapshot; only its service
        // coordinator owns the in-flight transport retry and its outcome.
        let retry = if self.service_owned {
            None
        } else {
            self.owner
                .recovery
                .lock()
                .map_err(|_| E::Transport)?
                .begin(&context.session)
        };
        let refreshed = if self.service_owned {
            context.prepare_retained(step)
        } else {
            context.refresh(step).await
        };
        if let Some(retry) = retry {
            retry.finish(match &refreshed {
                Ok(()) if matches!(context.attention, MailAttentionInfo::Ready { .. }) => "live",
                Ok(()) => "pending",
                Err(_) => "failed",
            });
        }
        match refreshed {
            Ok(()) => {}
            // Mail availability is independent of coding authority. A refused
            // transport supplies no batch and stays visibly unavailable; the
            // exact current binding is still required by take_batch.
            Err(E::Transport) => {
                context.batch = None;
                context.prepared = true;
                context.attention = context.blocked_attention();
                if let Some(action) = context.recovery_action {
                    let write_pending = context.runtime.as_ref().is_some_and(|runtime| {
                        runtime.delivery_work_pending() || runtime.delivery_debt_pending()
                    });
                    let mut recovery = self.owner.recovery.lock().map_err(|_| E::Transport)?;
                    context.close_pending = recovery.escalate(action, write_pending)
                        == super::recovery::Action::Reconnect;
                    recovery.defer(&context.session);
                    drop(recovery);
                    context.attention = MailAttentionInfo::Retrying;
                    context.verified_revision = None;
                    self.owner.cache_attention(context);
                    return Ok(());
                }
                if context.slow_retry {
                    // Same policy as the service path: stopped now, one slow retry.
                    let mut recovery = self.owner.recovery.lock().map_err(|_| E::Transport)?;
                    context.stop_for_slow_retry(&mut recovery);
                    drop(recovery);
                    context.verified_revision = None;
                    self.owner.cache_attention(context);
                    return Ok(());
                }
                context.blocked_revision = Some(revision.clone());
                context.close_pending = true;
            }
            Err(error) => {
                if matches!(error, E::Capability) {
                    context.require_rebind();
                    context.blocked_revision = Some(revision.clone());
                    context.verified_revision = Some(revision.clone());
                }
                self.owner.cache_attention(context);
                return Err(error);
            }
        }
        context.verified_revision = Some(revision);
        if matches!(context.attention, MailAttentionInfo::Ready { .. }) {
            context.slow_stopped = false;
            self.owner
                .recovery
                .lock()
                .map_err(|_| E::Transport)?
                .restored();
        }
        self.owner.cache_attention(context);
        Ok(())
    }
    pub(in crate::dogfood) fn decode_warning(&self) -> Result<Option<String>, E> {
        // A degraded step reports its outage from take_batch instead.
        if self.external_host || self.degraded.is_some() {
            return Ok(None);
        }
        self.lease.as_ref().ok_or(E::Incomplete)?.decode_warning()
    }
    /// Proceed without mail after any failure: nothing is taken, so the mail
    /// stays pending, and the plan tells the model why.
    pub(in crate::dogfood) fn degrade(&mut self, failure: E) {
        crate::diagnostics::report(format!(
            "stateengine-continuity stage=inbox state=degraded failure={failure:?}"
        ));
        self.degraded = Some(failure);
        self.cleanup_owed = true;
    }
    pub(in crate::dogfood) fn take_batch(&mut self) -> Result<Option<NativeMailBatch>, E> {
        if self.external_host {
            return Ok(None);
        }
        if let Some(failure) = self.degraded.take() {
            return Err(failure);
        }
        if self.owner.superseded.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let context = self.lease.as_mut().ok_or(E::Incomplete)?;
        if !context
            .inbox
            .lock()
            .map_err(|_| E::Binding)?
            .native_mail_wakes_available(&context.session)
            .map_err(|_| E::Binding)?
        {
            return Ok(None);
        }
        context.take_batch()
    }
    pub(in crate::dogfood) async fn finish(&mut self, cancelled: bool) -> Result<(), String> {
        if self.external_host {
            return Ok(());
        }
        // A degraded preparation failed part-way, exactly as a refused one did
        // before it stopped failing the turn; it is cleaned up the same way.
        let owed = std::mem::take(&mut self.cleanup_owed);
        let cancelled = cancelled || owed;
        // Before this plan acquires its lease, the retained service still owns
        // every transport operation. A timeout in an earlier preparation stage
        // must not close or wait for that unrelated transport.
        if self.service_owned && self.lease.is_none() {
            self.planning = None;
            return Ok(());
        }
        // The preparation future is already dropped. Private plans own their
        // context outright and must join cleanup even before acquiring a lease.
        let result = if cancelled {
            let wait_closed = self.owner.cancel_wait();
            let mut context = match self.lease.take() {
                Some(held) => held,
                None => self.owner.context.clone().lock_owned().await,
            };
            // A cancelled prompt cannot clear a stop that still needs attention.
            if context.attention != MailAttentionInfo::RebindRequired && !context.slow_stopped {
                context.attention = MailAttentionInfo::Unavailable;
                context.verified_revision = None;
            }
            let closing_revision = context
                .verified_revision
                .clone()
                .or_else(|| self.owner.revision().ok());
            let closed = context.finish().await;
            context.close_failed |= wait_closed.is_err();
            context.close_cleanup_failed |= wait_closed.is_err();
            if (closed.is_err() || wait_closed.is_err()) && context.blocked_revision.is_none() {
                context.blocked_revision = closing_revision;
            }
            wait_closed.and(closed)
        } else {
            Ok(())
        };
        self.lease = None;
        self.planning = None;
        match result {
            // A degraded step already told the model mail is unavailable. Its
            // failed close stays recorded on the context for the next step to
            // recover, and never refuses this attempt.
            Err(reason) if owed => {
                crate::diagnostics::report(format!(
                    "stateengine-continuity stage=cleanup state=degraded failure={reason}"
                ));
                Ok(())
            }
            other => other,
        }
    }
    pub(in crate::dogfood) async fn shutdown(&mut self) -> Result<(), String> {
        self.finish(!self.service_owned).await
    }
}
