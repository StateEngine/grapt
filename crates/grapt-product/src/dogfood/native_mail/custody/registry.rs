//! Bound active registrations and retain retired transports only through cleanup.
use super::*;

impl NativeMailOwners {
    pub(super) fn retire_owner(&self, owner: &Owner) {
        owner.superseded.store(true, Ordering::SeqCst);
        owner.target_registered.store(true, Ordering::SeqCst);
        if owner.cancel_wait().is_err() {
            self.retirement_cleanup_failed.store(true, Ordering::SeqCst);
        }
        if let Ok(mut revision) = owner.revision.lock() {
            *revision = Arc::new(());
        }
        if let Ok(mut context) = owner.context.try_lock() {
            context.batch = None;
            context.close_pending = context.owns_runtime();
        }
        self.changed.notify_one();
    }

    pub(in crate::dogfood) fn retire(&self, session: &SessionId) {
        let mut owners = match self.owners.lock() {
            Ok(owners) => owners,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(owner) = owners.get(session) {
            self.retire_owner(owner);
        }
        self.prune_retired(&mut owners);
    }

    pub(super) fn prune_retired(&self, owners: &mut BTreeMap<SessionId, Arc<Owner>>) {
        owners.retain(|_, owner| {
            if !owner.superseded.load(Ordering::SeqCst)
                || owner.planning.load(Ordering::SeqCst) != 0
            {
                return true;
            }
            let Ok(context) = owner.context.try_lock() else {
                return true;
            };
            if context.owns_runtime() {
                return true;
            }
            // Preserve the existing shutdown policy: a lost remote receipt is
            // historical; failed local cleanup must still refuse shutdown.
            if context.close_cleanup_failed {
                self.retirement_cleanup_failed.store(true, Ordering::SeqCst);
            }
            false
        });
    }

    pub(super) fn retain_context(
        &self,
        owners: &mut BTreeMap<SessionId, Arc<Owner>>,
        session: &SessionId,
        mut context: NativeMailContext,
        scope: Option<&RemoteMailboxScope>,
    ) -> Result<(Arc<Owner>, bool), String> {
        if let Some(owner) = owners.get(session) {
            if owner.superseded.load(Ordering::SeqCst) {
                self.admit_owner(owners, session, scope)?;
            }
            return Ok((owner.clone(), false));
        }
        // At most one further bounded set may be joining retired transports.
        // A takeover must not wait forever behind the owner it replaces.
        if owners.len() >= 2 * OWNER_CAP {
            return Err(
                "Native inbox retired connections are closing; retry after cleanup.".into(),
            );
        }
        self.admit_owner(owners, session, scope)?;
        context.service_root = Some(self.service_root.clone());
        context.service_owned = true;
        let owner = Owner::new(context, self.changed.clone());
        owners.insert(session.clone(), owner.clone());
        Ok((owner, true))
    }

    pub(super) fn admit_owner(
        &self,
        owners: &BTreeMap<SessionId, Arc<Owner>>,
        session: &SessionId,
        scope: Option<&RemoteMailboxScope>,
    ) -> Result<(), String> {
        let active = owners
            .values()
            .filter(|owner| !owner.superseded.load(Ordering::SeqCst))
            .count();
        if active < OWNER_CAP {
            return Ok(());
        }
        let authenticated = self
            .custody
            .lock()
            .map_err(|_| "native target capability unavailable")?
            .native_principals
            .contains_key(session);
        if let Some(scope) = scope.filter(|_| authenticated) {
            for owner in owners
                .values()
                .filter(|owner| !owner.superseded.load(Ordering::SeqCst))
            {
                if owner
                    .target_scope
                    .lock()
                    .map_err(|_| "native target scope unavailable")?
                    .as_ref()
                    == Some(scope)
                {
                    return Ok(());
                }
            }
        }
        Err("Native inbox connection capacity reached; close an active binding and retry.".into())
    }
}
