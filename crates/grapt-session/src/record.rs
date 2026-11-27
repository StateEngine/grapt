//! `get`: one Session's durable record.
//!
//! Law: the disposition row *Session aggregate/API* — "Split pure aggregate/
//! application API/transport… Thin server handlers" — and the store's three
//! answers, never conflated: a read that FAILED, a Session that does not
//! EXIST, and a seed that exists but cannot be UNDERSTOOD.
//!
//! # Why this reads the seed and not the list projection
//!
//! The session-list projection holds the same coordinates, and reading them
//! from there would be one query instead of two. It would also make `get`
//! answer from a REBUILDABLE cache: a projection that had not caught up would
//! report a freshly created Session as unknown, and a projection rebuilt from
//! a different range would report a different record than the log holds. The
//! seed is the durable truth; the list is a keyset index over it. `get` reads
//! truth.
//!
//! Consequently `get` and the list can disagree while a projection lags, and
//! that is correct — the projection "may lag but never lead". A caller that
//! needs the indexed view asks the list; a caller that needs the fact asks
//! here.

use crate::store::{SessionStore, SessionStoreError};
use grapt_types::{LocationRef, ProjectId, SessionId, SessionLaunch, WorkspaceId};
use serde_json::Value;

/// One Session, as its durable seed and lifecycle updates describe it.
///
/// Every optional field is optional in the DURABLE record, not merely unread:
/// a Session with no agent selected is a real state, and defaulting one here
/// would report a choice nobody made.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub session: SessionId,
    pub project: ProjectId,
    pub placement: LocationRef,
    pub workspace: Option<WorkspaceId>,
    pub parent: Option<SessionId>,
    pub title: String,
    pub slug: Option<String>,
    pub agent: Option<String>,
    pub model: Option<Value>,
    pub preset: Option<String>,
    /// How this Session's launch was recorded, when it was recorded at all.
    ///
    /// `None` is an ABSENT fact, not "windowed": an old client that stated no
    /// mode, or a Session seeded before this fact existed. The stop rule keeps
    /// absence distinct from an interactive launch, so this is not a `bool`.
    pub launch: Option<SessionLaunch>,
    /// From `info.time.created` — the seed's own stamp, not the event row's.
    pub created_ms: i64,
}

impl<'a> SessionStore<'a> {
    /// One Session's durable record, folded from its own aggregate.
    ///
    /// Returns [`SessionStoreError::Unknown`] for a Session with no seed —
    /// distinct from a read failure and from a corrupt seed, exactly as
    /// [`SessionStore::placement`] already distinguishes them.
    pub fn record(&self, session: &SessionId) -> Result<SessionRecord, SessionStoreError> {
        if self.deleted(session)? {
            return Err(SessionStoreError::Unknown {
                session: session.as_str().to_owned(),
            });
        }
        let corrupt = |reason: String| SessionStoreError::Corrupt {
            session: session.as_str().to_owned(),
            reason,
        };

        let mut projection = crate::projection::SessionProjection::default();
        let mut seeded = false;
        for (kind, version, data) in self.lifecycle_rows(session)? {
            match crate::event::SessionEvent::decode(&kind, version, &data) {
                Ok(Some(event)) => {
                    if matches!(event, crate::event::SessionEvent::Created { .. }) {
                        // Earliest seed wins. A second `session.created` for one
                        // aggregate is a fact about a broken log, and letting it
                        // re-seed would silently re-place a live Session.
                        if seeded {
                            continue;
                        }
                        seeded = true;
                    }
                    projection.apply(event);
                }
                Ok(None) => {}
                Err(error) => return Err(corrupt(format!("{kind}.{version}: {error}"))),
            }
        }
        if !seeded {
            return Err(SessionStoreError::Unknown {
                session: session.as_str().to_owned(),
            });
        }
        let row = projection
            .rows
            .get(session)
            .ok_or_else(|| corrupt("the seed folded to no row".to_owned()))?;

        let project = row
            .project_id
            .clone()
            .and_then(ProjectId::new)
            .ok_or_else(|| corrupt("seed projectID is missing or empty".to_owned()))?;
        let title = row
            .title
            .clone()
            .ok_or_else(|| corrupt("seed title is missing".to_owned()))?;
        let created_ms = row
            .time_created_ms
            .ok_or_else(|| corrupt("seed time.created is missing".to_owned()))?;
        // Placement comes from the store's existing canonicalizing read rather
        // than from a second parse of the same bytes here.
        let placement = self.placement(session)?;
        let workspace = placement.workspace().cloned();
        let parent = self.parent(session)?;

        Ok(SessionRecord {
            session: session.clone(),
            project,
            placement,
            workspace,
            parent,
            title,
            slug: row.slug.clone(),
            agent: row.agent.clone(),
            model: row.model.clone(),
            preset: row.preset.clone(),
            launch: row.launch,
            created_ms,
        })
    }
}
