//! Process-local tool authority issued only from a committed permission reply.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use grapt_types::{CapabilityId, PermissionId, PermissionReply, SessionId};
use tokio::sync::oneshot;

pub const MAX_PENDING_TOOL_PERMISSIONS: usize = 64;

#[derive(Debug)]
struct ToolEffectLease(CapabilityId);

impl ToolEffectLease {
    pub fn consume(self) -> CapabilityId {
        self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolAuthorizationError {
    #[error("permission {0} has no pending tool effect")]
    Unknown(PermissionId),
    #[error("permission {0} was rejected")]
    Rejected(PermissionId),
    #[error("permission {0} was cancelled")]
    Cancelled(PermissionId),
    #[error("approval unavailable (headless) for permission {0}")]
    ApprovalUnavailable(PermissionId),
    #[error("tool permission authority closed before settlement")]
    Closed,
    #[error("pending tool permission bound {0} is exhausted")]
    PendingBound(usize),
    #[error("tool permission reservation identity is exhausted")]
    ReservationIdentityExhausted,
}

#[derive(Debug)]
pub struct PendingToolEffect {
    awaiting_decision: bool,
    receiver: oneshot::Receiver<Result<ToolEffectLease, ToolAuthorizationError>>,
}

impl PendingToolEffect {
    pub fn awaits_decision(&self) -> bool {
        self.awaiting_decision
    }

    /// Consume this one pending authority result and invoke `effect` exactly
    /// once only after a private live lease has been consumed.
    ///
    /// The closure receives the lower, typed capability coordinate needed by
    /// an effect adapter. Neither the lease nor any constructor for it crosses
    /// this crate boundary.
    pub async fn consume_and_run<F, T>(self, effect: F) -> Result<T, ToolAuthorizationError>
    where
        F: FnOnce(CapabilityId) -> T,
    {
        let lease = self
            .receiver
            .await
            .unwrap_or(Err(ToolAuthorizationError::Closed))?;
        Ok(effect(lease.consume()))
    }
}

#[derive(Debug)]
struct Pending {
    reservation: u64,
    permission: PermissionId,
    session: SessionId,
    capability: CapabilityId,
    sender: oneshot::Sender<Result<ToolEffectLease, ToolAuthorizationError>>,
}

/// One capacity slot held before a durable permission ask may commit.
///
/// Dropping an uncommitted reservation removes the process-local entry. After
/// the durable ask commits, `commit` transfers the receiver into the only
/// public consume-and-run continuation.
#[derive(Debug)]
pub(crate) struct ToolEffectReservation<'a> {
    authority: &'a ToolPermissionAuthority,
    reservation: u64,
    receiver: Option<oneshot::Receiver<Result<ToolEffectLease, ToolAuthorizationError>>>,
}

impl ToolEffectReservation<'_> {
    pub(crate) fn commit(mut self) -> Result<PendingToolEffect, ToolAuthorizationError> {
        let receiver = self.receiver.take().ok_or(ToolAuthorizationError::Closed)?;
        Ok(PendingToolEffect {
            receiver,
            awaiting_decision: true,
        })
    }
}

impl Drop for ToolEffectReservation<'_> {
    fn drop(&mut self) {
        if self.receiver.is_none() {
            return;
        }
        let mut pending = lock(&self.authority.pending);
        if let Some(position) = pending
            .iter()
            .position(|entry| entry.reservation == self.reservation)
        {
            pending.remove(position);
        }
    }
}

/// Opaque proof that `PermissionHttpState` committed the first live reply.
#[derive(Debug)]
pub struct CommittedPermissionReply {
    permission: PermissionId,
    session: SessionId,
    reply: PermissionReply,
}

/// Proof of the permission owner's durable cancellation, never caller-minted.
#[derive(Debug)]
pub struct CommittedPermissionCancellation {
    permission: PermissionId,
    session: SessionId,
    reason: crate::CancelReason,
}

impl CommittedPermissionCancellation {
    pub(crate) fn new(
        permission: PermissionId,
        session: SessionId,
        reason: crate::CancelReason,
    ) -> Self {
        Self {
            permission,
            session,
            reason,
        }
    }
}

impl CommittedPermissionReply {
    pub(super) fn new(
        permission: PermissionId,
        session: SessionId,
        reply: PermissionReply,
    ) -> Self {
        Self {
            permission,
            session,
            reply,
        }
    }
}

#[derive(Debug, Default)]
pub struct ToolPermissionAuthority {
    pending: Mutex<Vec<Pending>>,
    next_reservation: AtomicU64,
}

impl ToolPermissionAuthority {
    pub(crate) fn pending_in(&self, session: &SessionId) -> Vec<PermissionId> {
        lock(&self.pending)
            .iter()
            .filter(|pending| pending.session == *session)
            .map(|pending| pending.permission.clone())
            .collect()
    }

    pub fn cancel(&self, committed: CommittedPermissionCancellation) {
        let mut pending = lock(&self.pending);
        if let Some(index) = pending.iter().position(|entry| {
            entry.permission == committed.permission && entry.session == committed.session
        }) {
            let entry = pending.remove(index);
            let error = match committed.reason {
                crate::CancelReason::Interrupted => {
                    ToolAuthorizationError::Cancelled(committed.permission)
                }
                crate::CancelReason::ApprovalUnavailable => {
                    ToolAuthorizationError::ApprovalUnavailable(committed.permission)
                }
            };
            let _ = entry.sender.send(Err(error));
        }
    }

    pub(crate) fn begin(
        &self,
        effect: crate::service::PermissionCreateEffect,
        permission: PermissionId,
        session: SessionId,
        capability: CapabilityId,
    ) -> Result<PendingToolEffect, ToolAuthorizationError> {
        if effect == crate::service::PermissionCreateEffect::Allow {
            let (sender, receiver) = oneshot::channel();
            let _ = sender.send(Ok(ToolEffectLease(capability)));
            return Ok(PendingToolEffect {
                receiver,
                awaiting_decision: false,
            });
        }
        self.reserve(permission, session, capability)?.commit()
    }

    pub(crate) fn reserve(
        &self,
        permission: PermissionId,
        session: SessionId,
        capability: CapabilityId,
    ) -> Result<ToolEffectReservation<'_>, ToolAuthorizationError> {
        let mut pending = lock(&self.pending);
        if pending.len() >= MAX_PENDING_TOOL_PERMISSIONS {
            return Err(ToolAuthorizationError::PendingBound(
                MAX_PENDING_TOOL_PERMISSIONS,
            ));
        }
        let reservation = self
            .next_reservation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| ToolAuthorizationError::ReservationIdentityExhausted)?;
        let (sender, receiver) = oneshot::channel();
        pending.push(Pending {
            reservation,
            permission,
            session,
            capability,
            sender,
        });
        Ok(ToolEffectReservation {
            authority: self,
            reservation,
            receiver: Some(receiver),
        })
    }

    pub fn settle(
        &self,
        committed: CommittedPermissionReply,
    ) -> Result<(), ToolAuthorizationError> {
        let pending = {
            let mut held = lock(&self.pending);
            let position = held
                .iter()
                .position(|pending| {
                    pending.permission == committed.permission
                        && pending.session == committed.session
                })
                .ok_or_else(|| ToolAuthorizationError::Unknown(committed.permission.clone()))?;
            held.remove(position)
        };
        if committed.reply == PermissionReply::Reject {
            let id = committed.permission;
            let _ = pending
                .sender
                .send(Err(ToolAuthorizationError::Rejected(id)));
            return Ok(());
        }
        let _ = pending.sender.send(Ok(ToolEffectLease(pending.capability)));
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::{
        EffectClass, EgressAuthority, FilesystemAuthority, RealizedEnforcement, ResolvedCapability,
    };

    fn capability() -> CapabilityId {
        CapabilityId::derive(&ResolvedCapability::new(
            EffectClass::FilesystemRead,
            None,
            FilesystemAuthority::default(),
            EgressAuthority::default(),
            RealizedEnforcement::HarnessOnly,
        ))
    }

    fn id<T: std::str::FromStr>(raw: &str) -> T
    where
        T::Err: std::fmt::Debug,
    {
        raw.parse().unwrap_or_else(|_| panic!("static identity"))
    }

    #[tokio::test]
    async fn once_consumes_one_committed_reply_into_one_lease() -> Result<(), ToolAuthorizationError>
    {
        let authority = ToolPermissionAuthority::default();
        let permission: PermissionId = id("per_once");
        let session: SessionId = id("ses_once");
        let pending = authority.begin(
            crate::service::PermissionCreateEffect::Ask,
            permission.clone(),
            session.clone(),
            capability(),
        )?;
        authority.settle(CommittedPermissionReply::new(
            permission.clone(),
            session,
            PermissionReply::Once,
        ))?;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&calls);
        let observed = pending
            .consume_and_run(move |capability| {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                capability
            })
            .await?;
        assert_eq!(observed, capability());
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one committed permission reply ran its governed effect more than once"
        );
        assert!(authority
            .settle(CommittedPermissionReply::new(
                permission,
                id("ses_once"),
                PermissionReply::Once
            ))
            .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn preapproved_policy_effect_runs_once_without_a_public_lease(
    ) -> Result<(), ToolAuthorizationError> {
        let authority = ToolPermissionAuthority::default();
        let expected = capability();
        let pending = authority.begin(
            crate::service::PermissionCreateEffect::Allow,
            id("per_preapproved"),
            id("ses_preapproved"),
            expected.clone(),
        )?;
        let observed = pending.consume_and_run(|capability| capability).await?;
        assert_eq!(observed, expected);
        Ok(())
    }

    #[tokio::test]
    async fn reject_and_cross_session_reply_mint_no_lease() -> Result<(), ToolAuthorizationError> {
        let authority = ToolPermissionAuthority::default();
        let permission: PermissionId = id("per_reject");
        let session: SessionId = id("ses_reject");
        let pending = authority.begin(
            crate::service::PermissionCreateEffect::Ask,
            permission.clone(),
            session,
            capability(),
        )?;
        assert!(authority
            .settle(CommittedPermissionReply::new(
                permission.clone(),
                id("ses_other"),
                PermissionReply::Once
            ))
            .is_err());
        authority.settle(CommittedPermissionReply::new(
            permission,
            id("ses_reject"),
            PermissionReply::Reject,
        ))?;
        assert!(matches!(
            pending.consume_and_run(|_| ()).await,
            Err(ToolAuthorizationError::Rejected(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_permission_cannot_be_revived_by_a_late_allow(
    ) -> Result<(), ToolAuthorizationError> {
        let authority = ToolPermissionAuthority::default();
        let permission: PermissionId = id("per_cancelled");
        let session: SessionId = id("ses_cancelled");
        let pending = authority.begin(
            crate::service::PermissionCreateEffect::Ask,
            permission.clone(),
            session.clone(),
            capability(),
        )?;
        authority.cancel(CommittedPermissionCancellation::new(
            permission.clone(),
            id("ses_other"),
            crate::CancelReason::Interrupted,
        ));
        assert_eq!(
            lock(&authority.pending).len(),
            1,
            "foreign cancellation revoked this session"
        );
        authority.cancel(CommittedPermissionCancellation::new(
            permission.clone(),
            session.clone(),
            crate::CancelReason::Interrupted,
        ));
        assert!(authority
            .settle(CommittedPermissionReply::new(
                permission,
                session,
                PermissionReply::Once
            ))
            .is_err());
        let mut effects = 0;
        assert!(matches!(
            pending.consume_and_run(|_| effects += 1).await,
            Err(ToolAuthorizationError::Cancelled(_))
        ));
        assert_eq!(effects, 0);
        assert!(lock(&authority.pending).is_empty());
        Ok(())
    }
}
