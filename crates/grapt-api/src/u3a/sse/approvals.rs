//! An approver is an authenticated live subscription, never a durable grant.
use super::*;
use grapt_types::{LocationRef, PermissionId, SessionId};

pub(super) const APPROVER_HEADER: &str = "x-grapt-approver";
/// The pre-rename spelling, still accepted from external approver clients.
pub(super) const LEGACY_APPROVER_HEADER: &str = "x-secode-approver"; // grapt-rename: keep

#[derive(Debug, Clone)]
pub(super) struct Approver {
    principal: SealedPrincipal,
    session: Option<SessionId>,
}

impl Approver {
    pub(super) fn from_request(request: &Request<Body>) -> Result<Option<Self>, ()> {
        let headers = request.headers();
        // Both spellings may be sent only when they agree.
        if headers.contains_key(APPROVER_HEADER)
            && headers.contains_key(LEGACY_APPROVER_HEADER)
            && headers.get(APPROVER_HEADER) != headers.get(LEGACY_APPROVER_HEADER)
        {
            return Err(());
        }
        let name = if request.headers().contains_key(APPROVER_HEADER) {
            APPROVER_HEADER
        } else {
            LEGACY_APPROVER_HEADER
        };
        let mut values = request.headers().get_all(name).iter();
        let Some(value) = values.next() else {
            return Ok(None);
        };
        if values.next().is_some() {
            return Err(());
        }
        let value = value.to_str().map_err(|_| ())?;
        let session = if value == "*" {
            None
        } else {
            Some(value.parse().map_err(|_| ())?)
        };
        let principal = request
            .extensions()
            .get::<SealedPrincipal>()
            .cloned()
            .ok_or(())?;
        Ok(Some(Self { principal, session }))
    }

    fn matches(&self, principal: &SealedPrincipal, session: &SessionId) -> bool {
        self.principal.principal() == principal.principal()
            && self.session.as_ref().is_none_or(|id| id == session)
    }
}

/// One observation of continuous approver presence, latched on the first gap.
#[derive(Debug)]
pub(super) struct ApprovalState {
    principal: SealedPrincipal,
    session: SessionId,
    permission: PermissionId,
    location: LocationRef,
    available: AtomicBool,
}

impl ApprovalState {
    fn matches(&self, subscribers: &[SubscriberSender]) -> bool {
        subscribers.iter().any(|subscriber| {
            subscriber
                .approver
                .as_ref()
                .is_some_and(|approver| approver.matches(&self.principal, &self.session))
                && !subscriber.sink.is_closed()
                && !subscriber.sink.is_invalidated()
                && interested(&subscriber.interest, Some(&self.location))
        })
    }
}

/// Subscriber membership and pending presence observations share one lock.
/// The observations issue no authority and own no permission decisions.
#[derive(Debug)]
pub(super) struct SubscriberRegistry {
    senders: Vec<SubscriberSender>,
    observations: Vec<std::sync::Weak<ApprovalState>>,
    /// The hub's stream positions, advanced only under this lock.
    pub(super) positions: grapt_stream::positions::StreamPositions,
    /// Every positioned frame offered, for resumes; under the same lock.
    pub(super) replay: grapt_stream::replay::ReplayRing,
}

impl SubscriberRegistry {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            senders: Vec::with_capacity(capacity),
            observations: Vec::new(),
            positions: grapt_stream::positions::StreamPositions::new(),
            replay: grapt_stream::replay::ReplayRing::default(),
        }
    }

    pub(super) fn refresh(&mut self) {
        let senders = &self.senders;
        self.observations.retain(|weak| {
            let Some(state) = weak.upgrade() else {
                return false;
            };
            if !state.matches(senders) {
                state.available.store(false, Ordering::Release);
            }
            true
        });
    }
}

impl std::ops::Deref for SubscriberRegistry {
    type Target = Vec<SubscriberSender>;
    fn deref(&self) -> &Self::Target {
        &self.senders
    }
}
impl std::ops::DerefMut for SubscriberRegistry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.senders
    }
}

#[derive(Debug)]
pub(super) struct ApprovalDrop {
    pub(super) subscribers: Arc<Mutex<SubscriberRegistry>>,
    pub(super) changes: Arc<tokio::sync::watch::Sender<()>>,
}

/// Watches the same bounded subscriber owner that delivers permission notices.
#[derive(Debug, Clone)]
pub struct ApprovalWatch {
    hub: Arc<DogfoodEventHub>,
    state: Arc<ApprovalState>,
    changed: tokio::sync::watch::Receiver<()>,
}

impl ApprovalWatch {
    pub fn available(&self) -> Result<bool, SseError> {
        let _registry = self
            .hub
            .subscribers
            .lock()
            .map_err(|_| SseError::RegistryUnavailable)?;
        Ok(self.state.available.load(Ordering::Acquire))
    }

    /// A fast reconnect cannot erase a last-approver disconnect before the
    /// permission owner gets polled. A new call takes a fresh observation.
    pub async fn unavailable(&mut self) -> Result<(), SseError> {
        loop {
            if !self.available()? {
                return Ok(());
            }
            self.changed
                .changed()
                .await
                .map_err(|_| SseError::RegistryUnavailable)?;
        }
    }
}

impl DogfoodEventHub {
    /// A global form needs a live, explicitly authenticated location-wide UI.
    /// This is availability, not an answer grant; global-form routes still
    /// authenticate and validate their real location at settlement.
    pub fn has_global_form_recipient(&self, location: &LocationRef) -> Result<bool, SseError> {
        let registry = self
            .subscribers
            .lock()
            .map_err(|_| SseError::RegistryUnavailable)?;
        Ok(registry.iter().any(|subscriber| {
            subscriber
                .approver
                .as_ref()
                .is_some_and(|approver| approver.session.is_none())
                && !subscriber.sink.is_closed()
                && !subscriber.sink.is_invalidated()
                && interested(&subscriber.interest, Some(location))
        }))
    }

    pub fn watch_approvals(
        self: &Arc<Self>,
        principal: &SealedPrincipal,
        session: &SessionId,
        permission: &PermissionId,
        location: LocationRef,
    ) -> Result<ApprovalWatch, SseError> {
        let mut registry = self
            .subscribers
            .lock()
            .map_err(|_| SseError::RegistryUnavailable)?;
        registry.refresh();
        if registry.observations.len() >= MAX_SSE_SUBSCRIBERS {
            return Err(SseError::ApprovalWatchBound);
        }
        let state = Arc::new(ApprovalState {
            principal: principal.clone(),
            session: session.clone(),
            permission: permission.clone(),
            location,
            available: AtomicBool::new(true),
        });
        state
            .available
            .store(state.matches(&registry), Ordering::Release);
        registry.observations.push(Arc::downgrade(&state));
        Ok(ApprovalWatch {
            hub: Arc::clone(self),
            state,
            changed: self.approval_changes.subscribe(),
        })
    }

    /// The caller holds the permission owner while this lock fences the
    /// synchronous durable decision against last-approver disconnect. None
    /// denotes an HTTP-only request with no pending tool observation.
    pub fn with_permission_approval<R>(
        &self,
        principal: &SealedPrincipal,
        session: &SessionId,
        permission: &PermissionId,
        decide: impl FnOnce(Option<bool>) -> R,
    ) -> Result<R, SseError> {
        let mut registry = self
            .subscribers
            .lock()
            .map_err(|_| SseError::RegistryUnavailable)?;
        registry.refresh();
        let mut available = None;
        for state in registry
            .observations
            .iter()
            .filter_map(std::sync::Weak::upgrade)
        {
            let owner = state.principal.principal();
            let caller = principal.principal();
            if state.permission == *permission
                && state.session == *session
                && owner.receipt().subject() == caller.receipt().subject()
                && owner.client() == caller.client()
            {
                let present = state.available.load(Ordering::Acquire);
                available = Some(available.unwrap_or(true) && present);
            }
        }
        Ok(decide(available))
    }

    pub(super) fn approvals_changed(&self) {
        self.approval_changes.send_replace(());
    }
}

#[cfg(test)]
mod tests;
