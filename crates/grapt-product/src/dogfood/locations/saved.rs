//! A saved Session supplies its binding; Git is optional repository metadata.
use std::path::Path;
use std::sync::Arc;

use grapt_api::u3a::ProjectCurrentLocationQueryDto;
use grapt_runtime::SessionPorts;
use grapt_types::DeterministicAuthorities;

use super::{ResolvedLocation, MAX_PATH_BYTES};
use crate::dogfood::{projects, ServerApplication};

pub(crate) async fn resolve_for(
    application: &ServerApplication,
    query: &ProjectCurrentLocationQueryDto,
) -> Result<ResolvedLocation, String> {
    let Some(session) = query.session().as_present() else {
        return super::resolve(&application.catalog_snapshot(), query).await;
    };
    let requested = query
        .location()
        .as_value()
        .ok_or("Session location is absent")?;
    if requested.workspace().as_value().is_some() {
        return Err("Session workspace context is not supported".into());
    }
    let directory = requested
        .directory()
        .as_value()
        .ok_or("Session directory is absent")?;
    if directory.as_str().len() > MAX_PATH_BYTES {
        return Err("Session directory exceeds its byte bound".into());
    }
    grapt_types::LocationRef::new(directory.as_str(), None).map_err(|error| error.to_string())?;
    // This is the same canonical record as SessionGet, never a client-supplied
    // project ID or a Session inferred from whichever cwd happens to match.
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let ports = SessionPorts::new(Arc::clone(&application.inbox), &authorities);
    let record = ports
        .get(session.as_inner())
        .map_err(|error| error.to_string())?;
    let saved = Path::new(record.placement.directory());
    let boundary =
        projects::project_directory(saved).ok_or("Saved Session directory is unreadable")?;
    let actual = std::fs::canonicalize(directory.as_str())
        .map_err(|error| format!("Session directory is unreadable: {error}"))?;
    let saved_path = std::fs::canonicalize(saved)
        .map_err(|error| format!("Saved Session directory is unreadable: {error}"))?;
    let root = std::fs::canonicalize(&boundary)
        .map_err(|error| format!("Session repository boundary is unreadable: {error}"))?;
    if actual != saved_path && actual != root {
        return Err("Requested directory differs from the saved Session checkout".into());
    }
    let project_directory = boundary
        .to_str()
        .ok_or("Session repository boundary is not UTF-8")?;
    if project_directory.len() > MAX_PATH_BYTES
        || record.placement.directory().len() > MAX_PATH_BYTES
    {
        return Err("Saved Session location exceeds its byte bound".into());
    }
    Ok(ResolvedLocation {
        directory: record.placement.directory().to_owned(),
        project: record.project,
        project_directory: project_directory.to_owned(),
    })
}

#[cfg(test)]
mod tests;
