//! Durable session create: the one route that seeds a Session.
//!
//! Law: `SECODE_RUST_FOUNDATIONAL_TYPES_AND_LIFECYCLES_2026-08-07.md`
//! §"Session lifecycle" (`prompt admission writes a durable pending record`
//! presupposes a Session that durably exists); the R1 determinism row
//! (`ClockAuthority`/`IdAuthority` are injected at durable and lifecycle
//! boundaries, ambient calls are adapter-only); and the store's
//! commit-before-notify law — the seed event, every projector row and every
//! projection watermark commit in ONE transaction, and the advisory notice
//! cannot leave it before that commit.
//!
//! # Why the seed is validated before the transaction opens
//!
//! [`SessionListProjector`](crate::list::SessionListProjector) decodes the very
//! `info` object this module writes, and a projector failure inside
//! `append_within` aborts the transaction. A seed that cannot project is
//! therefore already a refusal — the only question is whether it is a TYPED
//! refusal naming the field, or a `WriteError` naming a projector string. This
//! module makes it the former by validating through the SAME rules the
//! projector applies, reached through
//! `crate::list::validate_location` rather than restated here: two
//! normalizers for one directory is the defect the ratified `PathIdentity` row
//! exists to forbid, and a create path that agreed with the projector only by
//! coincidence would drift on the first change to either.
//!
//! # Identity selection never adopts an existing Session
//!
//! Ordinary creation mints through the injected [`grapt_types::IdAuthority`].
//! A launcher may supply a bounded typed identity, but the store must claim it
//! while it has no writer or history. That claim cannot transfer ownership or
//! reconcile an interrupted create. Both choices use the same seed event and
//! projection transaction. An unusable minted token is a typed refusal.
//!
//! # Why an existing seed refuses instead of reconciling
//!
//! The store's append path reconciles an EXACT retry of one event id as
//! `Idempotent`. That is a different question from this one. Here the identity
//! is required to be fresh, so a Session that already carries a seed means a
//! repeated identity, not a retry. Reporting
//! it as success would hand the caller somebody else's Session.
//! `SessionStore::seeded_within` answers from the same read the public
//! `exists` uses, so the refusal cannot disagree with the read view.

use crate::store::SessionStore;
use grapt_store::{AggregateId, AppendInput, Appended, EventStore, Projector, WriteError};
use grapt_types::{
    DeterministicAuthorities, LocationRef, ProjectId, SessionId, SessionLaunch, WorkspaceId,
};
use serde_json::{json, Map, Value};

/// The durable kind this module writes. Name and version stay separate
/// coordinates because [`AppendInput`] composes the stored
/// `session.created.1` identity from the pair.
const SESSION_CREATED: &str = "session.created";

/// The durable kind this module writes when a launcher states its launch mode.
///
/// Separate from the seed because the seed object is frozen: the launch fact
/// rides its own row rather than adding a member to `info`, so a reader that
/// knows only the seed still understands the Session, and an older build can
/// skip this row without losing the Session it belongs to.
const SESSION_LAUNCHED: &str = "session.launched";

/// The longest title a seed may carry, in UTF-8 bytes.
///
/// Bounded because the title enters durable canonical bytes AND the bounded
/// session-list page, and "resource claims include bytes, counts, and working
/// memory". The value is the list page's own per-row text bound, so a Session
/// cannot be created whose title the list read model would have to truncate.
pub const MAX_TITLE_BYTES: usize = crate::list::MAX_PAGE_TEXT_BYTES;

/// The longest slug a seed may carry, in UTF-8 bytes.
pub const MAX_SLUG_BYTES: usize = 128;

const MAX_REQUESTED_ID_BYTES: usize = 128;

/// The longest agent identity a seed may carry, in UTF-8 bytes.
///
/// Agent enters the same durable `info` object and the same bounded list page
/// as title and slug, so an unbounded agent is an unbounded durable write
/// through a field that merely happened to be checked for emptiness. The slug's
/// bound is reused rather than a second number invented: both are short
/// identity-shaped strings and two different ceilings would be two things to
/// keep in step for no stated reason.
pub const MAX_AGENT_BYTES: usize = MAX_SLUG_BYTES;

/// The longest model selection a seed may carry, as SERIALIZED UTF-8 bytes.
///
/// Measured on the canonical encoding rather than on any one member, because
/// the object is what enters durable bytes. Generous next to the three short
/// identifiers it legally holds, and still a bound.
pub const MAX_MODEL_BYTES: usize = 4096;

/// The deepest a model selection may nest.
///
/// The legal shape is FLAT — `{id, providerID, variant}` — so one is the only
/// depth a valid selection needs. The bound exists because a byte bound alone
/// does not stop a small deeply-nested object from costing unbounded stack in
/// every consumer that walks it, and `info` clones this value into a durable
/// event where every later reader inherits the cost.
pub const MAX_MODEL_DEPTH: usize = 1;

#[derive(Debug, thiserror::Error)]
pub enum SessionCreateError {
    /// The injected identity authority produced a token that cannot form a
    /// `ses_*` identity. Typed rather than repaired: an authority that mints
    /// unusable tokens is broken, and inventing a substitute identity here
    /// would hide it.
    #[error("the identity authority produced an unusable session token: {source}")]
    Identity {
        #[source]
        source: grapt_types::IdError,
    },
    /// A seed field is absent, empty, oversized, or cannot be canonicalized.
    #[error("session seed field {field} is invalid: {reason}")]
    Seed { field: &'static str, reason: String },
    /// The minted identity already carries a seed. See the module docs.
    #[error("session {session} already has a durable seed")]
    AlreadyCreated { session: String },
    /// The read that answers [`Self::AlreadyCreated`] failed. A failed read is
    /// never reported as "no seed": that would create a second Session over
    /// the top of a live one because a disk hiccuped.
    #[error("could not read the existing seed for {session}: {source}")]
    Read {
        session: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("session create could not be made durable: {source}")]
    Store {
        #[source]
        source: WriteError,
    },
    /// The append reconciled as idempotent, which for a freshly minted
    /// identity means the event id repeated. Distinguished from
    /// [`Self::AlreadyCreated`] because it names a different broken authority:
    /// the EVENT id authority rather than the session id it produced.
    #[error("session create reconciled as a retry of event {event}: identity repeated")]
    RepeatedEventId { event: String },
}

impl From<WriteError> for SessionCreateError {
    fn from(source: WriteError) -> Self {
        SessionCreateError::Store { source }
    }
}

/// A validated Session seed.
///
/// Fields are private and [`SessionSeed::new`] is the only constructor, so a
/// caller cannot assemble one that the projector would reject. Optional
/// coordinates are added by the `with_*` builders, each of which validates its
/// own field.
#[derive(Debug, Clone)]
pub struct SessionSeed {
    requested_id: Option<SessionId>,
    project: ProjectId,
    directory: String,
    workspace: Option<WorkspaceId>,
    parent: Option<SessionId>,
    title: String,
    slug: Option<String>,
    agent: Option<String>,
    model: Option<Value>,
    /// How this Session was launched. `None` means the launching client did not
    /// state a mode (an old client), so no `session.launched` fact is written
    /// and the reader falls back to absence.
    launch: Option<SessionLaunch>,
}

impl SessionSeed {
    /// The required coordinates. A Session without a project, a placement and
    /// a title is not a Session the list read model can represent, so all
    /// three are required here rather than defaulted.
    pub fn new(
        project: ProjectId,
        directory: impl Into<String>,
        title: impl Into<String>,
    ) -> Result<Self, SessionCreateError> {
        let directory = directory.into();
        let title = title.into();
        bounded("title", &title, MAX_TITLE_BYTES)?;
        // Placement is canonicalized through the projector's own rule. A
        // directory that will not form a `LocationRef` is refused BEFORE the
        // transaction, with the field named.
        crate::list::validate_location(&directory, None).map_err(|reason| {
            SessionCreateError::Seed {
                field: "directory",
                reason,
            }
        })?;
        Ok(Self {
            requested_id: None,
            project,
            directory,
            workspace: None,
            parent: None,
            title,
            slug: None,
            agent: None,
            model: None,
            launch: None,
        })
    }

    /// Use a launcher's fresh identity. The create owner claims it exactly once;
    /// an existing or interrupted aggregate is refused, never adopted.
    pub fn with_requested_id(mut self, session: SessionId) -> Result<Self, SessionCreateError> {
        bounded("id", session.as_str(), MAX_REQUESTED_ID_BYTES)?;
        self.requested_id = Some(session);
        Ok(self)
    }

    /// Places the Session inside a workspace.
    ///
    /// Re-validates the directory WITH the workspace: `LocationRef::on` takes
    /// both, so a directory that canonicalizes alone may still be refused
    /// inside a workspace. Validating only at [`Self::new`] would let that
    /// refusal arrive as a projector string instead of a typed field.
    pub fn with_workspace(mut self, workspace: WorkspaceId) -> Result<Self, SessionCreateError> {
        crate::list::validate_location(&self.directory, Some(&workspace)).map_err(|reason| {
            SessionCreateError::Seed {
                field: "workspaceID",
                reason,
            }
        })?;
        self.workspace = Some(workspace);
        Ok(self)
    }

    /// Records the parent this Session was created from.
    ///
    /// This is lineage ATTRIBUTION on an ordinary create, not a fork: a fork
    /// inherits capped parent history through `session.forked.2` and is not
    /// this module's business.
    pub fn with_parent(mut self, parent: SessionId) -> Self {
        self.parent = Some(parent);
        self
    }

    pub fn with_slug(mut self, slug: impl Into<String>) -> Result<Self, SessionCreateError> {
        let slug = slug.into();
        bounded("slug", &slug, MAX_SLUG_BYTES)?;
        self.slug = Some(slug);
        Ok(self)
    }

    /// The agent this Session runs as.
    ///
    /// Bounded like every other durable text coordinate. An emptiness check
    /// alone made this the one seed string with no ceiling, which is not a
    /// smaller version of the same rule — it is the absence of the rule.
    pub fn with_agent(mut self, agent: impl Into<String>) -> Result<Self, SessionCreateError> {
        let agent = agent.into();
        bounded("agent", &agent, MAX_AGENT_BYTES)?;
        self.agent = Some(agent);
        Ok(self)
    }

    /// The selected model, as the oracle stores it: an object carrying
    /// `{id, providerID, variant}`.
    ///
    /// Refused unless it IS an object. A bare string here would decode into
    /// [`crate::projection::SessionRow::model`] and reach a consumer expecting
    /// the object shape, which is the "JSON identity accident" the type
    /// constitution names as deliberate non-parity.
    ///
    /// "An object" was the WHOLE check before this revision, so any arbitrarily
    /// large or deeply nested object passed and `Self::info` then cloned it
    /// into the durable event. The shape is validated member by member here —
    /// against the frozen oracle's own three coordinates, `variant` optional —
    /// and the size and depth bounds are applied before the clone rather than
    /// discovered by a consumer that has already paid for it.
    pub fn with_model(mut self, model: Value) -> Result<Self, SessionCreateError> {
        validate_model(&model)?;
        self.model = Some(model);
        Ok(self)
    }

    /// Records how this Session was launched.
    ///
    /// A launcher that states a mode gets a durable `session.launched` fact at
    /// admission. A launcher that states nothing leaves it absent, so the
    /// reader can tell "never recorded" from "recorded as interactive".
    pub fn with_launch(mut self, launch: SessionLaunch) -> Self {
        self.launch = Some(launch);
        self
    }

    pub fn project(&self) -> &ProjectId {
        &self.project
    }

    pub fn directory(&self) -> &str {
        &self.directory
    }

    pub fn workspace(&self) -> Option<&WorkspaceId> {
        self.workspace.as_ref()
    }

    /// The placement this seed canonicalizes to.
    ///
    /// Rebuilt from the validated directory rather than stored, so there is one
    /// canonicalization and no second copy that could disagree with the bytes
    /// actually written.
    pub fn placement(&self) -> Result<LocationRef, SessionCreateError> {
        crate::list::location_of(&self.directory, self.workspace.as_ref()).map_err(|reason| {
            SessionCreateError::Seed {
                field: "directory",
                reason,
            }
        })
    }

    /// The `info` object exactly as it enters canonical bytes.
    ///
    /// `created` and `updated` are the SAME reading. A Session that reported
    /// two different times for the instant it was created would make
    /// `time_created` and the list's `updated_ms` disagree at seq zero, and
    /// the restart/relist equality witness would be asserting nothing.
    fn info(&self, session: &SessionId, now_ms: i64) -> Value {
        let mut info = Map::new();
        info.insert("id".to_owned(), json!(session.as_str()));
        info.insert("projectID".to_owned(), json!(self.project.as_str()));
        info.insert("directory".to_owned(), json!(self.directory));
        info.insert("title".to_owned(), json!(self.title));
        if let Some(workspace) = &self.workspace {
            info.insert("workspaceID".to_owned(), json!(workspace.as_str()));
        }
        if let Some(parent) = &self.parent {
            info.insert("parentID".to_owned(), json!(parent.as_str()));
        }
        if let Some(slug) = &self.slug {
            info.insert("slug".to_owned(), json!(slug));
        }
        if let Some(agent) = &self.agent {
            info.insert("agent".to_owned(), json!(agent));
        }
        if let Some(model) = &self.model {
            info.insert("model".to_owned(), model.clone());
        }
        info.insert(
            "time".to_owned(),
            json!({"created": now_ms, "updated": now_ms}),
        );
        Value::Object(info)
    }
}

/// One member of the model selection: a non-empty string and nothing else.
fn model_identifier(member: &str, value: &Value) -> Result<(), SessionCreateError> {
    match value.as_str() {
        Some(text) if !text.is_empty() => Ok(()),
        Some(_) => Err(SessionCreateError::Seed {
            field: "model",
            reason: format!("has an empty {member}"),
        }),
        None => Err(SessionCreateError::Seed {
            field: "model",
            reason: format!("has a non-string {member}"),
        }),
    }
}

/// How deeply a JSON value nests, counting the outermost container as one.
///
/// Iterative rather than recursive: a depth check that blew the stack on the
/// input it exists to refuse would be the defect wearing the guard's clothes.
fn json_depth(value: &Value) -> usize {
    let mut deepest = 0;
    let mut frontier = vec![(value, 1_usize)];
    while let Some((value, depth)) = frontier.pop() {
        match value {
            Value::Object(members) => {
                deepest = deepest.max(depth);
                for held in members.values() {
                    frontier.push((held, depth.saturating_add(1)));
                }
            }
            Value::Array(items) => {
                deepest = deepest.max(depth);
                for held in items {
                    frontier.push((held, depth.saturating_add(1)));
                }
            }
            // A scalar occupies no container of its own; it is AT the depth of
            // the container holding it, which the parent already recorded.
            _ => {}
        }
    }
    deepest
}

pub(crate) fn validate_model(model: &Value) -> Result<(), SessionCreateError> {
    let Some(members) = model.as_object() else {
        return Err(SessionCreateError::Seed {
            field: "model",
            reason: "is not an object".to_owned(),
        });
    };
    // RESOURCE bounds first, SHAPE second, and the order is load-bearing.
    // Strict member typing admits only strings, so a model that reached the
    // member check could never be more than one level deep: putting the
    // depth bound after it would make the depth bound unreachable from any
    // real input, and an unreachable guard is an unwitnessed one. Refusing
    // on size and depth BEFORE walking the members also means the walk is
    // itself bounded.
    let encoded = serde_json::to_string(model).map_err(|source| SessionCreateError::Seed {
        field: "model",
        reason: format!("cannot be encoded: {source}"),
    })?;
    if encoded.len() > MAX_MODEL_BYTES {
        return Err(SessionCreateError::Seed {
            field: "model",
            reason: format!("is {} bytes; the bound is {MAX_MODEL_BYTES}", encoded.len()),
        });
    }
    let depth = json_depth(model);
    if depth > MAX_MODEL_DEPTH {
        return Err(SessionCreateError::Seed {
            field: "model",
            reason: format!("nests {depth} deep; the bound is {MAX_MODEL_DEPTH}"),
        });
    }
    for required in ["id", "providerID"] {
        let value = members
            .get(required)
            .ok_or_else(|| SessionCreateError::Seed {
                field: "model",
                reason: format!("is missing {required}"),
            })?;
        model_identifier(required, value)?;
    }
    if let Some(variant) = members.get("variant") {
        model_identifier("variant", variant)?;
    }
    // A closed member set, like every other payload this lane writes: an
    // unnamed member would reach the projection and the list page as a
    // coordinate nothing validated.
    if let Some(unknown) = members
        .keys()
        .find(|key| !matches!(key.as_str(), "id" | "providerID" | "variant"))
    {
        return Err(SessionCreateError::Seed {
            field: "model",
            reason: format!("carries the unnamed member {unknown}"),
        });
    }
    Ok(())
}

pub(crate) fn bounded(
    field: &'static str,
    value: &str,
    max: usize,
) -> Result<(), SessionCreateError> {
    if value.is_empty() {
        return Err(SessionCreateError::Seed {
            field,
            reason: "is empty".to_owned(),
        });
    }
    if value.len() > max {
        return Err(SessionCreateError::Seed {
            field,
            reason: format!("is {} bytes; the bound is {max}", value.len()),
        });
    }
    Ok(())
}

/// What one successful create committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCreated {
    pub session: SessionId,
    /// The seed's own sequence on the Session aggregate. Zero for every
    /// Session this module creates — carried rather than asserted so a witness
    /// can read it instead of trusting the claim.
    pub seq: u64,
    /// The single clock reading the seed carries, in milliseconds.
    pub created_ms: i64,
}

/// Creates one Session, durably.
///
/// The store is taken by `&mut` because election takes `&mut self` and cannot
/// run while a transaction from the same store is alive: the writer is elected
/// FIRST, its token snapshotted, and only then is the transaction opened.
///
/// `projectors` is the caller's list. It is not defaulted to the session-list
/// projector: which projections a store maintains is the store owner's
/// decision, and silently attaching one here would write a durable row a
/// caller never asked for.
pub fn create_session(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    seed: &SessionSeed,
    projectors: &mut [&mut dyn Projector],
) -> Result<SessionCreated, SessionCreateError> {
    let session: SessionId = match &seed.requested_id {
        Some(requested) => requested.clone(),
        None => format!("{}_{}", SessionId::PREFIX, authorities.mint().as_str())
            .parse()
            .map_err(|source| SessionCreateError::Identity { source })?,
    };
    let event: grapt_types::EventId = format!(
        "{}_{}",
        grapt_types::EventId::PREFIX,
        authorities.mint().as_str()
    )
    .parse()
    .map_err(|source| SessionCreateError::Identity { source })?;
    let now_ms = authorities.now().as_millis();
    let data = json!({"sessionID": session.as_str(), "info": seed.info(&session, now_ms)});

    let aggregate = AggregateId::Session(session.clone());
    if seed.requested_id.is_some() {
        store.claim_new_writer(&aggregate)?;
    } else {
        store.elect_writer(&aggregate)?;
    }
    let writer = store.fencing_token(&aggregate)?;

    let tx = store
        .governed_transaction()
        .map_err(|source| SessionCreateError::Store {
            source: WriteError::from(source),
        })?;
    // The existence check runs INSIDE the transaction, which begins IMMEDIATE:
    // a check taken before the write lock could pass and then lose the race to
    // another writer's seed.
    if SessionStore::seeded_within(&tx, &session).map_err(|source| SessionCreateError::Read {
        session: session.as_str().to_owned(),
        source,
    })? {
        return Err(SessionCreateError::AlreadyCreated {
            session: session.as_str().to_owned(),
        });
    }
    let appended = EventStore::append_within(
        &tx,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: SESSION_CREATED,
            version: created_version(),
            created_ms: now_ms,
            data: &data,
        },
        projectors,
    )?;
    let seq = match appended {
        Appended::Committed { seq } => seq,
        // Unreachable through a working authority: the identity above was
        // minted for this call. Represented rather than ignored, because
        // reporting a repeated event id as a successful create is exactly the
        // silent adoption this module refuses.
        Appended::Idempotent { .. } => {
            return Err(SessionCreateError::RepeatedEventId {
                event: event.as_str().to_owned(),
            })
        }
    };
    // The launch mode rides the SAME transaction as the seed. A Session that
    // exists without the launch fact its launcher stated would be stopped by a
    // closing window the instant it started, so the two commit together or
    // neither does.
    if let Some(launch) = seed.launch {
        append_launch(
            &tx,
            authorities,
            &session,
            launch,
            writer,
            projectors,
            now_ms,
        )?;
    }
    tx.commit()
        .map_err(|source| SessionCreateError::Store {
            source: WriteError::from(source),
        })?
        // Create has no live subscriber of its own: the advisory notice belongs
        // to whoever owns the feed, and this seam has none to hand it to.
        .discard();
    Ok(SessionCreated {
        session,
        seq,
        created_ms: now_ms,
    })
}

/// Append one `session.launched` fact inside the caller's open transaction.
///
/// It reuses the writer the caller already elected and fenced, so the launch
/// row cannot belong to a different epoch than the seed beside it: a create
/// that failed its fence must fail before either row lands, not between them.
fn append_launch(
    tx: &grapt_store::GovernedTransaction<'_>,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    launch: SessionLaunch,
    writer: grapt_store::FencedWriter,
    projectors: &mut [&mut dyn Projector],
    created_ms: i64,
) -> Result<(), SessionCreateError> {
    let event: grapt_types::EventId = format!(
        "{}_{}",
        grapt_types::EventId::PREFIX,
        authorities.mint().as_str()
    )
    .parse()
    .map_err(|source| SessionCreateError::Identity { source })?;
    let aggregate = AggregateId::Session(session.clone());
    let data = json!({"sessionID": session.as_str(), "launch": launch.as_str()});
    let appended = EventStore::append_within(
        tx,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: SESSION_LAUNCHED,
            version: created_version(),
            created_ms,
            data: &data,
        },
        projectors,
    )?;
    match appended {
        Appended::Committed { .. } => Ok(()),
        // The identity above was minted for this call, so a repeat means the
        // event-id authority handed out one twice. Reporting that is the only
        // honest answer: reconciling would silently drop the launch fact.
        Appended::Idempotent { .. } => Err(SessionCreateError::RepeatedEventId {
            event: event.as_str().to_owned(),
        }),
    }
}

fn created_version() -> grapt_types::EventVersion {
    match grapt_types::EventVersion::new(1) {
        Some(version) => version,
        None => unreachable!("1 is a non-zero version"),
    }
}

#[cfg(test)]
mod tests;
