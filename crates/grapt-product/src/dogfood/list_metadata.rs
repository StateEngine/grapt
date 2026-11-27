//! Isolate metadata hydration failures without changing the indexed page.
use super::support::decode;
use grapt_api::u3a::{SessionInfoDto, SessionInfoDtoMetadataStatus as Status};
use grapt_runtime::{SessionPortError, SessionPorts};
use grapt_session::{SessionListItem, SessionStoreError};
use serde_json::json;

pub(super) fn row(
    ports: &SessionPorts<'_>,
    item: &SessionListItem,
) -> Result<SessionInfoDto, String> {
    match hydrate(ports, item) {
        Ok(row) => Ok(row),
        Err(status) => fallback(item, status),
    }
}

fn hydrate(ports: &SessionPorts<'_>, item: &SessionListItem) -> Result<SessionInfoDto, Status> {
    let record = ports
        .get(item.key.session_id())
        .map_err(|error| match error {
            SessionPortError::Read {
                source: SessionStoreError::Store { .. },
            } => Status::Unavailable,
            _ => Status::Quarantined,
        })?;
    let value = super::session_info::metadata(
        &record,
        item.key.updated_ms().value().max(record.created_ms),
    )
    .map_err(|_| Status::Quarantined)?;
    decode(value).map_err(|_| Status::Quarantined)
}

fn fallback(item: &SessionListItem, status: Status) -> Result<SessionInfoDto, String> {
    // The index knows placement and update time, but cannot attest creation,
    // selected configuration, totals or cost when the seed is unreadable.
    let mut value = json!({
        "id": item.key.session_id().as_str(),
        "projectID": item.project_id.as_str(),
        "title": item.title,
        "time": { "updated": item.key.updated_ms().value() as f64 },
        "location": { "directory": item.directory },
        "metadataStatus": status,
    });
    if let Some(workspace) = &item.workspace_id {
        value["location"]["workspaceID"] = json!(workspace.as_str());
    }
    if let Some(parent) = &item.parent_id {
        value["parentID"] = json!(parent.as_str());
    }
    decode(value)
}
