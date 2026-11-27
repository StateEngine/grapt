//! The typed application ports the API and product call.
//!
//! Law: the disposition row *Session aggregate/API* — "Split pure aggregate/
//! application API/transport. Keep admission separate from execution. Thin
//! server handlers" — and the protocol row I1: a client "consumes the same
//! DTO/protocol surface even when embedded". This module is the layer BELOW
//! that surface: typed application calls with no transport in them.
//!
//! # What this is, and what it deliberately is not
//!
//! It is a composition, not a new owner. Every call here delegates to the seam
//! that already owns the capability — [`grapt_session::create_session`],
//! [`grapt_session::SessionStore`], [`grapt_session::SessionListStore`],
//! [`grapt_session::MessageHistory`], [`grapt_inbox::Inbox`] — and adds
//! exactly one thing: a single place where an API handler can reach all of
//! them without knowing which crate owns which.
//!
//! It contains **no HTTP, no SSE, no DTOs and no routing**. Those belong to
//! `grapt-api`, and a port that returned a wire shape would make this the
//! protocol's source of truth. It also does not open a store, choose a
//! directory, resolve config or select a catalog: a caller hands it an already
//! open [`Inbox`], because store-open policy and product-root policy are other
//! owners' law.
//!
//! # Why interrupt is NOT here
//!
//! [`SessionCoordinator::interrupt`](crate::SessionCoordinator::interrupt)
//! already exposes a typed [`InterruptOutcome`](crate::InterruptOutcome) that
//! distinguishes a real interrupt from an idle no-op. Wrapping it would be a
//! second route to one typed outcome, and the two routes would be free to
//! disagree about what "interrupted" means. A caller that needs to interrupt
//! holds the coordinator, which is where the live drain actually is; these
//! ports hold durable state only.
//!
//! # Why the inbox is the shared handle
//!
//! [`Inbox`] owns either the writer or a pooled read-only snapshot. Mutation
//! ports retain the writer's shared lock; a finite read projection supplies a
//! private inbox whose queries all observe the same WAL transaction.

use std::sync::{Arc, Mutex, MutexGuard};

use grapt_inbox::{AdmitOutcome, AdmitRequest, Inbox, InboxError};
use grapt_session::{
    create_session, AssistantMessage, HistoryReadError, SessionCreateError, SessionCreated,
    SessionHistoryStore, SessionListCriteria, SessionListError, SessionListPage,
    SessionListProjector, SessionListStore, SessionRecord, SessionSeed, SessionStore,
    SessionStoreError,
};
use grapt_types::{DeliveryMode, DeterministicAuthorities, EventId, MessageId, SessionId};

#[derive(Debug, thiserror::Error)]
pub enum SessionPortError {
    #[error("create refused: {source}")]
    Create {
        #[source]
        source: SessionCreateError,
    },
    #[error("read refused: {source}")]
    Read {
        #[source]
        source: SessionStoreError,
    },
    #[error("list refused: {source}")]
    List {
        #[source]
        source: SessionListError,
    },
    #[error("history refused: {source}")]
    History {
        #[source]
        source: HistoryReadError,
    },
    #[error("admission refused: {source}")]
    Admission {
        #[source]
        source: InboxError,
    },
    #[error("the identity authority produced an unusable {what} token")]
    Identity { what: &'static str },
}

/// The application ports over one open [`Inbox`].
#[derive(Debug)]
pub struct SessionPorts<'a> {
    inbox: Arc<Mutex<Inbox>>,
    authorities: &'a DeterministicAuthorities<'a>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PromptAdmission {
    pub message: MessageId,
    pub created_ms: i64,
    pub outcome: AdmitOutcome,
}

impl<'a> SessionPorts<'a> {
    /// There is no zero-argument constructor: the authorities are required,
    /// which is ownership probe #11 expressed in a signature.
    pub fn new(inbox: Arc<Mutex<Inbox>>, authorities: &'a DeterministicAuthorities<'a>) -> Self {
        Self { inbox, authorities }
    }

    fn lock(&self) -> MutexGuard<'_, Inbox> {
        // A poisoned lock means a sibling panicked. The store's own
        // transactions rolled back, so the DATA is not in doubt, and refusing
        // every later call would turn one panic into a dead process.
        match self.inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Creates one Session, durably, and projects it into the session list in
    /// the SAME transaction.
    ///
    /// The list projector is attached here rather than left to the caller
    /// because these ports also serve [`Self::list`]: a create that did not
    /// project would make this surface self-inconsistent — the Session would
    /// exist for `get` and not for `list` — and a caller has no way to know
    /// that from the signature.
    pub fn create(&self, seed: &SessionSeed) -> Result<SessionCreated, SessionPortError> {
        let mut guard = self.lock();
        let mut projector = SessionListProjector;
        create_session(
            guard.store_mut(),
            self.authorities,
            seed,
            &mut [&mut projector],
        )
        .map_err(|source| SessionPortError::Create { source })
    }

    /// One Session's durable record.
    pub fn get(&self, session: &SessionId) -> Result<SessionRecord, SessionPortError> {
        let guard = self.lock();
        SessionStore::over(guard.store())
            .record(session)
            .map_err(|source| SessionPortError::Read { source })
    }

    /// One bounded page of the session list.
    pub fn list(
        &self,
        criteria: &SessionListCriteria,
    ) -> Result<SessionListPage, SessionPortError> {
        let mut guard = self.lock();
        SessionListStore::over(guard.store_mut())
            .list(criteria)
            .map_err(|source| SessionPortError::List { source })
    }

    /// One Session's assistant message history, folded from durable truth.
    ///
    /// Folding on every call is deliberate — see
    /// [`grapt_session::history`] — and it is what makes the restart equality
    /// law testable rather than merely asserted.
    pub fn history(&self, session: &SessionId) -> Result<Vec<AssistantMessage>, SessionPortError> {
        let guard = self.lock();
        let history = SessionHistoryStore::over(guard.store())
            .fold(session)
            .map_err(|source| SessionPortError::History { source })?;
        history
            .messages()
            .map_err(|source| SessionPortError::History {
                source: source.into(),
            })
    }

    /// Admits one user input durably.
    ///
    /// Admission is SEPARATE from execution, which is the disposition row's
    /// own words. This call makes the input durable and reports whether the
    /// caller may advise a wake; it starts no drain, opens no provider stream,
    /// and boots no Location.
    pub fn prompt(
        &self,
        session: &SessionId,
        text: &str,
        delivery: DeliveryMode,
        resume: bool,
    ) -> Result<AdmitOutcome, SessionPortError> {
        let input: MessageId = format!("msg_{}", self.authorities.mint().as_str())
            .parse()
            .map_err(|_| SessionPortError::Identity { what: "message" })?;
        self.prompt_with_id(session, input, text, delivery, resume)
            .map(|admission| admission.outcome)
    }

    /// Admit a prompt under the client's stable message identity.
    ///
    /// This is the HTTP retry seam: the same identity plus the same Session,
    /// text and delivery reconciles, including after promotion. A caller can
    /// therefore lose the response or sever its event stream without buying a
    /// second provider call.
    pub fn prompt_with_id(
        &self,
        session: &SessionId,
        input: MessageId,
        text: &str,
        delivery: DeliveryMode,
        resume: bool,
    ) -> Result<PromptAdmission, SessionPortError> {
        self.prompt_with_images(session, input, text, delivery, resume, &[])
    }

    /// User-selected pixels are already captured; stable retries bind their exact references.
    #[allow(
        clippy::too_many_arguments,
        reason = "typed prompt admission coordinates plus captured images"
    )]
    pub fn prompt_with_images(
        &self,
        session: &SessionId,
        input: MessageId,
        text: &str,
        delivery: DeliveryMode,
        resume: bool,
        images: &[grapt_types::ArtifactRef],
    ) -> Result<PromptAdmission, SessionPortError> {
        self.prompt_with_origin(
            session,
            input,
            text,
            delivery,
            resume,
            images,
            grapt_inbox::UserInputOrigin::Human,
        )
    }

    /// Preserve prepared-seat provenance through the same admission/retry seam.
    #[allow(
        clippy::too_many_arguments,
        reason = "typed prompt admission coordinates and presentation provenance"
    )]
    pub fn prompt_with_origin(
        &self,
        session: &SessionId,
        input: MessageId,
        text: &str,
        delivery: DeliveryMode,
        resume: bool,
        images: &[grapt_types::ArtifactRef],
        origin: grapt_inbox::UserInputOrigin,
    ) -> Result<PromptAdmission, SessionPortError> {
        self.prompt_with_options(
            session,
            input,
            text,
            delivery,
            resume,
            grapt_inbox::UserInputOptions {
                images,
                origin,
                max_steps: None,
            },
        )
    }

    pub fn prompt_with_options(
        &self,
        session: &SessionId,
        input: MessageId,
        text: &str,
        delivery: DeliveryMode,
        resume: bool,
        options: grapt_inbox::UserInputOptions<'_>,
    ) -> Result<PromptAdmission, SessionPortError> {
        let event: EventId = format!("evt_{}", self.authorities.mint().as_str())
            .parse()
            .map_err(|_| SessionPortError::Identity { what: "event" })?;
        let created_ms = self.authorities.now().as_millis();
        let mut guard = self.lock();
        let outcome = guard
            .admit_with_options(
                AdmitRequest {
                    event_id: &event,
                    session,
                    input: &input,
                    text,
                    delivery,
                    resume,
                    created_ms,
                },
                options,
            )
            .map_err(|source| SessionPortError::Admission { source })?;
        Ok(PromptAdmission {
            message: input,
            created_ms,
            outcome,
        })
    }
}

#[cfg(test)]
mod tests;
