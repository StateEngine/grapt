//! Permission polling reads one committed generation without mutation locks.
//! Capacity exhaustion disables acceleration; it never refuses durable work.

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

use grapt_event::EventKind;
use grapt_permission::{
    PermissionHttpError, PermissionHttpPage, PermissionHttpSnapshot, PermissionOwner,
};
use grapt_store::commit_projection::ProjectionConnection as Connection;
use grapt_store::commit_projection::{CommittedReadProjection, PreparedReadProjection};
use grapt_types::SessionId;

use super::ServerApplication;

const MAX_GENERATION_BYTES: usize = 1_048_576;
const MAX_SESSIONS: usize = 4096;
const MAX_PERMISSIONS: usize = 4096;
const MAX_READERS: usize = 32;

#[cfg(test)]
#[path = "permission_view_measure.rs"]
mod measure;
// Bound the existing lifecycle fold's working input, independently of retained
// generation data. Longer/corrupt histories use the unchanged durable path.
const MAX_LIFECYCLE_ROWS: i64 = 128;
const MAX_LIFECYCLE_BYTES: i64 = 32_768;

#[derive(Debug)]
struct Generation {
    sessions: Vec<SessionId>,
    permissions: PermissionHttpSnapshot,
    #[cfg(test)]
    allocation_bytes: usize,
}

#[derive(Debug)]
pub(super) enum PollError {
    MissingSession(SessionId),
    Permission(PermissionHttpError),
}

#[derive(Debug, Default)]
pub(super) struct PermissionView {
    published: Mutex<Option<Arc<Generation>>>,
    readers: AtomicUsize,
    fallback_logged: AtomicBool,
}

struct ReaderLease<'a>(&'a AtomicUsize);

impl Drop for ReaderLease<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl PermissionView {
    pub(super) fn attach(application_inbox: &Arc<Mutex<grapt_inbox::Inbox>>) -> Arc<Self> {
        let view = Arc::new(Self::default());
        let mut inbox = match application_inbox.lock() {
            Ok(inbox) => inbox,
            Err(poisoned) => poisoned.into_inner(),
        };
        inbox
            .store_mut()
            .install_read_projection(Arc::new(ViewProjection(Arc::clone(&view))));
        view
    }

    /// The lease and Arc never escape this synchronous operation, including
    /// errors. They drop before the HTTP handler can await or wait on a socket.
    pub(super) fn poll(
        &self,
        owner: &PermissionOwner<'_>,
        session: Option<&SessionId>,
    ) -> Option<Result<PermissionHttpPage, PollError>> {
        if self
            .readers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |readers| {
                (readers < MAX_READERS).then_some(readers + 1)
            })
            .is_err()
        {
            self.log_fallback();
            return None;
        }
        let _lease = ReaderLease(&self.readers);
        let generation = match self.published.lock() {
            Ok(published) => published.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        let Some(generation) = generation else {
            self.log_fallback();
            return None;
        };
        if let Some(session) = session.filter(|session| {
            generation
                .sessions
                .binary_search_by(|candidate| candidate.as_str().cmp(session.as_str()))
                .is_err()
        }) {
            return Some(Err(PollError::MissingSession(session.clone())));
        }
        Some(
            generation
                .permissions
                .list(owner, session)
                .map_err(PollError::Permission),
        )
    }

    fn log_fallback(&self) {
        if !self.fallback_logged.swap(true, Ordering::AcqRel) {
            crate::diagnostics::report(
                "permission polling accelerator unavailable; using durable reads".to_owned(),
            );
        }
    }

    #[cfg(test)]
    pub(super) fn allocation_bytes(&self) -> Option<usize> {
        match self.published.lock() {
            Ok(published) => published
                .as_ref()
                .map(|generation| generation.allocation_bytes),
            Err(poisoned) => poisoned
                .into_inner()
                .as_ref()
                .map(|generation| generation.allocation_bytes),
        }
    }
}

#[derive(Debug)]
struct ViewProjection(Arc<PermissionView>);

impl CommittedReadProjection for ViewProjection {
    fn affected(&self, kind: EventKind) -> bool {
        matches!(
            kind.name(),
            "session.created"
                | "session.deleted"
                | "session.renamed"
                | "session.agent.selected"
                | "session.model.selected"
                | "session.preset.selected"
                | "session.forked"
        ) || kind.name().starts_with("permission.")
    }

    fn prepare(&self, connection: &Connection) -> Box<dyn PreparedReadProjection> {
        // No preparation failure can change a mutation's durable outcome.
        let generation = build(connection).ok().flatten().map(Arc::new);
        Box::new(PreparedView {
            view: Arc::clone(&self.0),
            generation,
            #[cfg(feature = "loopback-provider-testkit")]
            fault: super::permission_view_fault::Fault::prepare(),
        })
    }
}

#[derive(Debug)]
struct PreparedView {
    view: Arc<PermissionView>,
    generation: Option<Arc<Generation>>,
    #[cfg(feature = "loopback-provider-testkit")]
    fault: Option<super::permission_view_fault::Fault>,
}

impl PreparedReadProjection for PreparedView {
    fn publish(self: Box<Self>) {
        #[cfg(feature = "loopback-provider-testkit")]
        if let Some(fault) = self.fault {
            fault.after_commit();
        }
        let mut published = match self.view.published.lock() {
            Ok(published) => published,
            Err(poisoned) => poisoned.into_inner(),
        };
        let previous = std::mem::replace(&mut *published, self.generation);
        drop(published);
        // Destruction can free thousands of strings. Keep it outside the lock.
        drop(previous);
    }
}

fn build(connection: &Connection) -> Result<Option<Generation>, String> {
    let mut statement = connection
        .prepare(
            "SELECT DISTINCT e.aggregate_id FROM event e
         WHERE e.aggregate_family = 'session' AND e.type = 'session.created.1'
         ORDER BY e.aggregate_id LIMIT 4097",
        )
        .map_err(|error| error.to_string())?;
    let mut cursor = statement.query([]).map_err(|error| error.to_string())?;
    let mut sessions = Vec::new();
    let mut seeds_read = 0;
    let mut strings_bytes = 0usize;
    let overhead = std::mem::size_of::<Generation>() + 2 * std::mem::size_of::<usize>();
    while let Some(row) = cursor.next().map_err(|error| error.to_string())? {
        if seeds_read == MAX_SESSIONS {
            return Ok(None);
        }
        seeds_read += 1;
        let raw = row
            .get_ref(0)
            .map_err(|error| error.to_string())?
            .as_str()
            .map_err(|error| error.to_string())?;
        if overhead
            + (sessions.len() + 1) * std::mem::size_of::<SessionId>()
            + strings_bytes
            + raw.len()
            > MAX_GENERATION_BYTES
        {
            return Ok(None);
        }
        let session: SessionId = raw
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        let (rows, bytes): (i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(length(CAST(data AS BLOB))), 0) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type IN ('session.created.1', 'session.renamed.1', 'session.agent.selected.1',
                          'session.model.selected.1', 'session.preset.selected.1',
                          'session.launched.1',
                          'session.deleted.1', 'session.deleted.2')",
                [session.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        if rows > MAX_LIFECYCLE_ROWS || bytes > MAX_LIFECYCLE_BYTES {
            return Ok(None);
        }
        // Preserve get_record's corruption/seed validation, retaining only ID.
        match grapt_session::SessionStore::new(connection).record(&session) {
            Ok(_) => {}
            Err(grapt_session::SessionStoreError::Unknown { .. }) => continue,
            Err(error) => return Err(error.to_string()),
        }
        // SessionId's parser owns raw.to_owned(), with this exact byte length.
        strings_bytes += session.as_str().len();
        sessions.reserve_exact(1);
        sessions.push(session);
    }
    let session_bytes =
        overhead + strings_bytes + sessions.capacity() * std::mem::size_of::<SessionId>();
    let Some(remaining) = MAX_GENERATION_BYTES.checked_sub(session_bytes) else {
        return Ok(None);
    };
    let Some(permissions) =
        PermissionHttpSnapshot::read_bounded(connection, MAX_PERMISSIONS, remaining)
            .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let allocation_bytes = session_bytes + permissions.allocation_bytes();
    if allocation_bytes > MAX_GENERATION_BYTES {
        return Ok(None);
    }
    Ok(Some(Generation {
        sessions,
        permissions,
        #[cfg(test)]
        allocation_bytes,
    }))
}

impl ServerApplication {
    pub(super) fn poll_committed_permissions(
        &self,
        owner: &PermissionOwner<'_>,
        session: Option<&SessionId>,
    ) -> Option<Result<PermissionHttpPage, PollError>> {
        self.permission_view.poll(owner, session)
    }
}
