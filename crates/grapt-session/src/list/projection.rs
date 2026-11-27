use super::request::NonNegativeMs;
use grapt_store::session_list::SESSION_LIST_PROJECTION;
use grapt_store::{DecodedEvent, ProjectionId, Projector};
use grapt_types::{LocationRef, ProjectId, SessionId, WorkspaceId};
use rusqlite::OptionalExtension;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct SessionListProjector;

impl SessionListProjector {
    fn project_event(tx: &rusqlite::Transaction<'_>, event: &DecodedEvent) -> Result<(), String> {
        match (event.kind_name(), event.kind_version().value()) {
            ("session.created", 1) => insert_created(tx, event),
            ("session.moved", 1) => move_session(tx, event),
            ("session.renamed", 1) => {
                let title = string(&event.data, "title")?;
                update_time_and_title(tx, event, Some(title))
            }
            ("session.agent.selected", 1)
            | ("session.preset.selected", 1)
            | ("session.model.selected", 1)
            | ("session.revert.staged", 1)
            | ("session.revert.cleared", 1)
            | ("session.revert.committed", 1) => update_time_and_title(tx, event, None),
            ("session.forked", 2) => insert_fork(tx, event),
            ("session.deleted", 1 | 2) => delete_session(tx, event),
            ("session.usage.recorded", 1) => Ok(()),
            _ => Ok(()),
        }
    }
}

impl Projector for SessionListProjector {
    fn projection(&self) -> ProjectionId {
        SESSION_LIST_PROJECTION
    }

    fn project(
        &mut self,
        tx: &rusqlite::Transaction<'_>,
        event: &DecodedEvent,
    ) -> Result<(), grapt_store::ProjectorError> {
        Ok(Self::project_event(tx, event)?)
    }
}

fn insert_created(tx: &rusqlite::Transaction<'_>, event: &DecodedEvent) -> Result<(), String> {
    let info = event
        .data
        .get("info")
        .and_then(Value::as_object)
        .ok_or_else(|| "session.created info is missing or not an object".to_owned())?;
    let session = session(&event.data)?;
    if session != event.aggregate_id {
        return Err("session.created payload identity differs from aggregate".into());
    }
    let project = object_string(info, "projectID")?;
    ProjectId::new(project.clone())
        .ok_or_else(|| "session.created projectID is empty".to_owned())?;
    let workspace = object_optional_string(info, "workspaceID")?;
    let workspace_id = validate_workspace(workspace.as_deref(), "session.created workspaceID")?;
    let parent = object_optional_string(info, "parentID")?;
    if let Some(parent) = &parent {
        parent
            .parse::<SessionId>()
            .map_err(|error| format!("session.created parentID: {error}"))?;
    }
    let directory = object_string(info, "directory")?;
    validate_location(&directory, workspace_id.as_ref())?;
    let title = object_string(info, "title")?;
    let updated = info
        .get("time")
        .and_then(|value| value.get("updated"))
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            "session.created info.time.updated is missing or not an integer".to_owned()
        })?;
    nonnegative(updated, "session.created info.time.updated")?;
    tx.execute(
        "INSERT INTO session_list_projection
            (session_id, project_id, workspace_id, parent_id, directory, title, updated_ms,
             provenance_kind, provenance_digest, cutover_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'rust-event', ?8, -1)",
        rusqlite::params![
            session.as_str(),
            project,
            workspace,
            parent,
            directory,
            title,
            updated,
            event.canonical_hash
        ],
    )
    .map_err(|error| format!("project session.created: {error}"))?;
    Ok(())
}

fn move_session(tx: &rusqlite::Transaction<'_>, event: &DecodedEvent) -> Result<(), String> {
    nonnegative(event.created_ms, "session.moved created")?;
    let session = session(&event.data)?;
    let location = event
        .data
        .get("location")
        .and_then(Value::as_object)
        .ok_or_else(|| "session.moved location is missing or not an object".to_owned())?;
    let directory = object_string(location, "directory")?;
    let workspace = object_optional_string(location, "workspaceID")?;
    let workspace_id = validate_workspace(workspace.as_deref(), "session.moved workspaceID")?;
    validate_location(&directory, workspace_id.as_ref())?;
    let project = optional_string(&event.data, "projectID")?;
    if let Some(project) = &project {
        ProjectId::new(project.clone())
            .ok_or_else(|| "session.moved projectID is empty".to_owned())?;
    }
    let changed = if let Some(project) = project {
        tx.execute(
            "UPDATE session_list_projection
             SET directory = ?2, workspace_id = ?3, project_id = ?4,
                 updated_ms = ?5, provenance_kind = 'rust-event',
                 provenance_digest = ?6, cutover_seq = -1
             WHERE session_id = ?1",
            rusqlite::params![
                session.as_str(),
                directory,
                workspace,
                project,
                event.created_ms,
                event.canonical_hash
            ],
        )
    } else {
        tx.execute(
            "UPDATE session_list_projection
             SET directory = ?2, workspace_id = ?3, updated_ms = ?4,
                 provenance_kind = 'rust-event', provenance_digest = ?5, cutover_seq = -1
             WHERE session_id = ?1",
            rusqlite::params![
                session.as_str(),
                directory,
                workspace,
                event.created_ms,
                event.canonical_hash
            ],
        )
    }
    .map_err(|error| format!("project session.moved: {error}"))?;
    exactly_one(changed, "session.moved", &session)
}

fn update_time_and_title(
    tx: &rusqlite::Transaction<'_>,
    event: &DecodedEvent,
    title: Option<String>,
) -> Result<(), String> {
    nonnegative(event.created_ms, event.kind_name())?;
    let session = session(&event.data)?;
    let changed = if let Some(title) = title {
        tx.execute(
            "UPDATE session_list_projection
             SET title = ?2, updated_ms = ?3, provenance_kind = 'rust-event',
                 provenance_digest = ?4, cutover_seq = -1
             WHERE session_id = ?1",
            rusqlite::params![
                session.as_str(),
                title,
                event.created_ms,
                event.canonical_hash
            ],
        )
    } else {
        tx.execute(
            "UPDATE session_list_projection
             SET updated_ms = ?2, provenance_kind = 'rust-event',
                 provenance_digest = ?3, cutover_seq = -1
             WHERE session_id = ?1",
            rusqlite::params![session.as_str(), event.created_ms, event.canonical_hash],
        )
    }
    .map_err(|error| format!("project {}: {error}", event.kind_name()))?;
    exactly_one(changed, event.kind_name(), &session)
}

fn insert_fork(tx: &rusqlite::Transaction<'_>, event: &DecodedEvent) -> Result<(), String> {
    nonnegative(event.created_ms, "session.forked created")?;
    let session = session(&event.data)?;
    let parent_raw = string(&event.data, "parentID")?;
    let parent = parent_raw
        .parse::<SessionId>()
        .map_err(|error| format!("session.forked parentID: {error}"))?;
    let inherited: Option<(String, Option<String>, String, String)> = tx
        .query_row(
            "SELECT project_id, workspace_id, directory, title
             FROM session_list_projection WHERE session_id = ?1",
            [parent.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| format!("read session.forked parent: {error}"))?;
    let (project, workspace, directory, title) =
        inherited.ok_or_else(|| format!("session.forked parent {parent} is absent"))?;
    tx.execute(
        "INSERT INTO session_list_projection
            (session_id, project_id, workspace_id, parent_id, directory, title, updated_ms,
             provenance_kind, provenance_digest, cutover_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'rust-event', ?8, -1)",
        rusqlite::params![
            session.as_str(),
            project,
            workspace,
            parent.as_str(),
            directory,
            fork_title(&title),
            event.created_ms,
            event.canonical_hash
        ],
    )
    .map_err(|error| format!("project session.forked: {error}"))?;
    Ok(())
}

fn delete_session(tx: &rusqlite::Transaction<'_>, event: &DecodedEvent) -> Result<(), String> {
    let session = if event.kind_version().value() == 1 {
        crate::deletion::decode_v1(&event.data)?
    } else {
        session(&event.data)?
    };
    if session != event.aggregate_id {
        return Err("session.deleted payload identity differs from aggregate".into());
    }
    let changed = tx
        .execute(
            "DELETE FROM session_list_projection WHERE session_id = ?1",
            [session.as_str()],
        )
        .map_err(|error| format!("project session.deleted: {error}"))?;
    exactly_one(changed, "session.deleted", &session)
}

fn exactly_one(changed: usize, kind: &str, session: &SessionId) -> Result<(), String> {
    if changed == 1 {
        Ok(())
    } else {
        Err(format!(
            "{kind} affected {changed} session-list rows for {session}; expected exactly one"
        ))
    }
}

fn fork_title(title: &str) -> String {
    let suffix = title.strip_suffix(')');
    if let Some((base, number)) = suffix.and_then(|value| value.rsplit_once(" (fork #")) {
        if let Ok(number) = number.parse::<u64>() {
            return format!("{base} (fork #{})", number.saturating_add(1));
        }
    }
    format!("{title} (fork #1)")
}

fn session(data: &Value) -> Result<SessionId, String> {
    string(data, "sessionID")?
        .parse()
        .map_err(|error| format!("sessionID: {error}"))
}

fn string(data: &Value, field: &'static str) -> Result<String, String> {
    data.get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{field} is missing or not a string"))
}

fn optional_string(data: &Value, field: &'static str) -> Result<Option<String>, String> {
    match data.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("{field} is present but not a string")),
    }
}

fn object_string(
    data: &serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<String, String> {
    data.get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{field} is missing or not a string"))
}

fn object_optional_string(
    data: &serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, String> {
    match data.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("{field} is present but not a string")),
    }
}

fn nonnegative(value: i64, field: &str) -> Result<NonNegativeMs, String> {
    NonNegativeMs::new(value).ok_or_else(|| format!("{field} is negative"))
}

fn validate_workspace(raw: Option<&str>, field: &str) -> Result<Option<WorkspaceId>, String> {
    raw.map(|value| WorkspaceId::new(value.to_owned()).ok_or_else(|| format!("{field} is empty")))
        .transpose()
}

pub(crate) fn validate_location(
    directory: &str,
    workspace: Option<&WorkspaceId>,
) -> Result<(), String> {
    location_of(directory, workspace).map(|_| ())
}

/// The ONE canonicalization of a durable `directory` string.
///
/// `validate_location` used to hold this body and discard the result. The
/// create path needs the `LocationRef` itself, and a second derivation there —
/// even one copied line for line — would be a second normalizer for the same
/// bytes: the exact defect the ratified `PathIdentity` row forbids, and one
/// that would drift the first time either copy changed. So the value-returning
/// form is the owner and the validating form is its `map(|_| ())`.
///
/// Platform is inferred from the string rather than injected because that is
/// what the durable row offers: a projector folding a log written on another
/// machine has no ambient platform to consult, and a POSIX-absolute directory
/// is unambiguous.
pub(crate) fn location_of(
    directory: &str,
    workspace: Option<&WorkspaceId>,
) -> Result<LocationRef, String> {
    let platform = if directory.starts_with('/') {
        grapt_types::Platform::Posix
    } else {
        grapt_types::Platform::Windows
    };
    LocationRef::on(platform, directory, workspace.cloned()).map_err(|error| error.to_string())
}
