//! Preview and confirm exactly the project declaration selected by the catalog.
use super::{
    ApplicationFuture, Edit, InvalidRequestErrorDto, ProjectCurrentLocationQueryDto,
    ServerApplication,
};
use crate::dogfood::{
    catalog_config::CatalogSources,
    connections::{project_approval::ProjectServer, McpSnapshot},
    session_config,
    support::invalid,
};
use grapt_api::u3a::{BoundedString, McpApprovalPreviewDto};
use grapt_types::Secret;

fn selected(
    sources: &CatalogSources,
    location: &str,
    name: &str,
) -> Result<Option<ProjectServer>, String> {
    Ok(McpSnapshot::read(sources, location)?
        .project_servers
        .remove(name))
}

pub(crate) fn enabled_edit(
    sources: &CatalogSources,
    location: &str,
    name: &str,
    enabled: bool,
) -> Result<Edit, String> {
    let Some(project) = selected(sources, location, name)? else {
        return Ok(Edit::SetEnabled(enabled));
    };
    if !project.approved {
        return Err(format!(
            "Project MCP server is not approved. Run /mcp approve {name} first."
        ));
    }
    Ok(Edit::ProjectApproval {
        fingerprint: project.fingerprint,
        enabled,
    })
}

pub(crate) fn approve_edit(
    sources: &CatalogSources,
    location: &str,
    name: &str,
    expected: &str,
) -> Result<Edit, String> {
    let project =
        selected(sources, location, name)?.ok_or("MCP project declaration is unavailable")?;
    if project.fingerprint != expected {
        return Err(
            "MCP declaration changed after its preview; inspect and approve it again".into(),
        );
    }
    Ok(Edit::ProjectApproval {
        fingerprint: project.fingerprint,
        enabled: true,
    })
}

pub(crate) fn preview(
    application: ServerApplication,
    name: String,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<McpApprovalPreviewDto, InvalidRequestErrorDto> {
    Box::pin(async move {
        let selection = session_config::catalog::select(&application, &query)
            .await
            .map_err(invalid)?;
        let project = selected(&selection.sources, &selection.location.directory, &name)
            .map_err(invalid)?
            .ok_or_else(|| invalid("MCP project declaration is unavailable"))?;
        selection.validate_current(&application).map_err(invalid)?;
        Ok(McpApprovalPreviewDto {
            fingerprint: BoundedString::try_new(project.fingerprint).map_err(invalid)?,
            source: BoundedString::try_new(project.source).map_err(invalid)?,
            directory: BoundedString::try_new(selection.location.directory).map_err(invalid)?,
            description: Secret::new(BoundedString::try_new(project.summary).map_err(invalid)?),
            approved: project.approved,
        })
    })
}
