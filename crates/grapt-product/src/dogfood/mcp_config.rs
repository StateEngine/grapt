//! Persistent user configuration, separate from temporary ACP registrations.
mod approval;
pub(crate) mod edit;
pub(crate) mod file;
pub(crate) use approval::preview;

use crate::dogfood::{
    catalog_config::CatalogEnvironment, session_config, support::invalid, ServerApplication,
};
use grapt_api::u3a::{
    ApplicationFuture, InvalidRequestErrorDto, McpConfigurationRequestDto, NoContent,
    ProjectCurrentLocationQueryDto,
};
use std::path::Path;

pub(crate) enum Edit {
    Add(serde_json::Value),
    Remove,
    SetEnabled(bool),
    ProjectApproval { fingerprint: String, enabled: bool },
    RevokeProjectApproval,
}

pub(crate) fn request(
    application: ServerApplication,
    name: String,
    query: ProjectCurrentLocationQueryDto,
    request: McpConfigurationRequestDto,
) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
    Box::pin(async move {
        let selection = session_config::catalog::select(&application, &query)
            .await
            .map_err(invalid)?;
        let environment =
            CatalogEnvironment::read(Path::new(&application.catalog_snapshot().directory))
                .map_err(invalid)?;
        let edit = match request {
            McpConfigurationRequestDto::Add { config } => {
                Edit::Add(serde_json::to_value(config).map_err(invalid)?)
            }
            McpConfigurationRequestDto::Remove {} => Edit::Remove,
            McpConfigurationRequestDto::SetEnabled { enabled } => approval::enabled_edit(
                &selection.sources,
                &selection.location.directory,
                &name,
                enabled,
            )
            .map_err(invalid)?,
            McpConfigurationRequestDto::ApproveProject { fingerprint } => approval::approve_edit(
                &selection.sources,
                &selection.location.directory,
                &name,
                fingerprint.as_str(),
            )
            .map_err(invalid)?,
            McpConfigurationRequestDto::RevokeProjectApproval {} => Edit::RevokeProjectApproval,
        };
        selection.validate_current(&application).map_err(invalid)?;
        // User server edits apply to every workspace. Project approval edits
        // apply to every private Session scope of this exact workspace.
        let workspace = matches!(
            edit,
            Edit::RevokeProjectApproval | Edit::ProjectApproval { .. }
        )
        .then_some(selection.location.directory.as_str());
        let roots_edit = application
            .mcp
            .edit_roots(workspace, &name)
            .map_err(invalid)?;
        tokio::task::spawn_blocking(move || {
            let _roots_edit = roots_edit;
            file::update(environment, &selection.location.directory, &name, &edit)
        })
        .await
        .map_err(|_| invalid("MCP configuration writer could not finish"))?
        .map_err(invalid)?;
        Ok(NoContent)
    })
}

// Validation uses the catalog's existing declaration fold, without creating a
// transport or treating these not-yet-published bytes as trusted configuration.
fn validate(
    bytes: &[u8],
    path: &Path,
    environment: CatalogEnvironment,
    location: &str,
) -> Result<(), String> {
    use crate::dogfood::catalog_config::{bounds, CatalogDocument, CatalogSources};
    let value = grapt_config::decode(bytes, bounds()?)
        .map_err(|error| error.to_string())?
        .into_value();
    let sources = CatalogSources {
        directory: Path::new(location).to_path_buf(),
        documents: vec![CatalogDocument {
            directory: environment.config.clone(),
            source: path.to_string_lossy().into_owned(),
            origin: grapt_plugin::PluginOrigin::Unknown,
            value,
        }],
        document_paths: vec![path.to_path_buf()],
        command_sources: Vec::new(),
        skill_directories: Vec::new(),
        environment,
    };
    crate::dogfood::connections::McpSnapshot::read(&sources, location).map(|_| ())
}
