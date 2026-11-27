//! The bounded connection registry owns one authenticated target per seat scope.
use super::*;

impl NativeMailOwners {
    pub(super) fn select_target(
        &self,
        owners: &BTreeMap<SessionId, Arc<Owner>>,
        session: &SessionId,
        owner: &Arc<Owner>,
        binding: (RemoteMailboxScope, String, Arc<Secret<String>>),
    ) -> Result<(), String> {
        let (scope, raw, capability) = binding;
        let fingerprint = MailBindingExpectation::new(
            InstructionKey::new("stateengine-identity").ok_or("native identity key")?,
            raw,
        )
        .map_err(|_| "native target binding is unavailable")?
        .fingerprint();
        let mut inbox = self
            .inbox
            .lock()
            .map_err(|_| "native target inbox unavailable")?;
        let mut custody = self
            .custody
            .lock()
            .map_err(|_| "native target capability unavailable")?;
        if !custody.native_principals.contains_key(session) {
            return Ok(());
        }
        if custody
            .values
            .get(session)
            .is_none_or(|current| !Arc::ptr_eq(current, &capability))
        {
            return Err("native target capability changed".into());
        }
        // Obtain every fallible bookkeeping guard before the durable commit.
        let mut current_scope = owner
            .target_scope
            .lock()
            .map_err(|_| "native target scope unavailable")?;
        let mut superseded = Vec::new();
        for (old_session, old) in owners.iter() {
            if old_session != session
                && !old.superseded.load(Ordering::SeqCst)
                && old
                    .target_scope
                    .lock()
                    .map_err(|_| "native target scope unavailable")?
                    .as_ref()
                    == Some(&scope)
            {
                superseded.push(old_session.clone());
            }
        }
        let event: EventId = format!("evt_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|_| "native target event identity")?;
        let created_ms = self.clock.now().as_millis();
        inbox
            .select_native_mail_target(session, &superseded, &fingerprint, &event, created_ms)
            .map_err(|_| "native target transition refused")?;
        *current_scope = Some(scope);
        owner.superseded.store(false, Ordering::SeqCst);
        owner.target_registered.store(true, Ordering::SeqCst);
        for old_session in superseded {
            custody.remove(&old_session);
            if let Some(old) = owners.get(&old_session) {
                // Revocation is already committed. Cleanup cannot undo the
                // new target or prevent its registry metadata being recorded.
                self.retire_owner(old);
            }
        }
        Ok(())
    }
}

impl Owner {
    /// Retirement does not depend on a usable old revision or retry timer.
    pub(super) async fn retire_if_superseded(&self) -> bool {
        if !self.superseded.load(Ordering::SeqCst) {
            return false;
        }
        let mut context = self.context.clone().lock_owned().await;
        if context.owns_runtime() && !context.close_pending {
            context.close_pending = true;
            self.changed.notify_one();
        }
        true
    }
}
