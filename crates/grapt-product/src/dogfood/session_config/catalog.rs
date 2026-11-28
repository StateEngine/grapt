//! One Session selection for MCP inspection, resources and operator actions.
use super::{registry_key, revision, sources, CatalogEnvironment, CatalogSources};
use crate::dogfood::{context, locations, ServerApplication};
use grapt_api::u3a::ProjectCurrentLocationQueryDto;
use grapt_types::SessionId;
use std::path::Path;

pub(in crate::dogfood) struct Selection {
    pub location: locations::ResolvedLocation,
    pub sources: CatalogSources,
    pub registry_key: String,
    pub roots_revision: crate::dogfood::mcp::roots::Revision,
    pub session: Option<SessionId>,
    revision: Option<u64>,
}

impl Selection {
    /// A response that awaited transport startup must not report an old binding.
    pub fn validate_current(&self, application: &ServerApplication) -> Result<(), String> {
        if let Some(session) = &self.session {
            context::session_record(application, session)
                .map_err(|_| "MCP catalog Session ended")?;
            if revision(&application.lock_inbox(), session)? != self.revision {
                return Err("Session config changed while reading MCP state; retry".into());
            }
        }
        Ok(())
    }
}

pub(in crate::dogfood) async fn select(
    application: &ServerApplication,
    query: &ProjectCurrentLocationQueryDto,
) -> Result<Selection, String> {
    let roots_revision = application.mcp.roots_revision()?;
    let provider_catalog = application.catalog_snapshot();
    let service = Path::new(&provider_catalog.directory);
    let Some(session) = query.session().as_present().map(|id| id.as_inner()) else {
        let location = locations::resolve(&application.catalog_snapshot(), query).await?;
        return Ok(Selection {
            roots_revision,
            sources: CatalogSources::read(
                Path::new(&location.directory),
                CatalogEnvironment::read(service)?,
            )?,
            registry_key: location.directory.clone(),
            location,
            session: None,
            revision: None,
        });
    };
    let record = context::session_record(application, session)
        .map_err(|_| "MCP catalog Session is unavailable")?;
    let directory = record.placement.directory();
    if query.location().as_value().is_some_and(|location| {
        location.workspace().as_value().is_some()
            || location
                .directory()
                .as_value()
                .is_some_and(|path| path.as_str() != directory)
    }) {
        return Err("MCP catalog location differs from its Session".into());
    }
    let expected = revision(&application.lock_inbox(), session)?;
    let location = locations::resolve_directory(directory).await?;
    let selection = Selection {
        roots_revision,
        sources: sources(&application.inbox, session, service, Path::new(directory))?,
        registry_key: registry_key(&application.inbox, session, directory)?,
        location,
        session: Some(session.clone()),
        revision: expected,
    };
    selection.validate_current(application)?;
    Ok(selection)
}
