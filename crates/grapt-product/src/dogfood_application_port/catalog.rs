use std::path::Path;
use std::sync::Arc;

use grapt_api::u3a::{
    AgentListResponseDto, AgentLocationQueryDto, ApplicationFuture, CatalogApplicationError,
    CommandInspectionResponseDto, CommandListResponseDto, FormListResponseDto,
    HookImportRequestDto, HookImportResponseDto, InvalidRequestErrorDto, LocationInfoDto,
    ModelDefaultLocationQueryDto, ModelDefaultResponseDto, ModelListResponseDto,
    ModelLocationQueryDto, PluginListResponseDto, ProjectCurrentDto,
    ProjectCurrentLocationQueryDto, ProjectListResponseDto, ProviderListResponseDto,
    ProviderLocationQueryDto, QuestionListResponseDto, ReferenceListResponseDto, ShellInfoDto,
    ShellListResponseDto, SkillListResponseDto, WorkspaceDiffResponseDto,
    WorkspaceWorktreeRequestDto, WorkspaceWorktreeResponseDto,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::dogfood::support::{decode, invalid, ready};
use crate::dogfood::{
    catalogs, form_catalog, hook_import, locations, plugins, projects, selection, workspace_diff,
    workspace_worktree, ServerApplication,
};

async fn resolve_transformed_catalog<Q: Serialize>(
    application: ServerApplication,
    query: Q,
) -> Result<(Value, Value), CatalogApplicationError> {
    let plugins = application.plugins_runtime();
    let draft = plugins::catalog_draft(&application)
        .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
    let query = serde_json::to_value(query)
        .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
    let query =
        decode(query).map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
    let location = locations::resolve_for(&application, &query)
        .await
        .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
    let transformed = plugins
        .transform_current("catalog", draft)
        .await
        .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))?;
    Ok((location.wire(), transformed))
}

pub(super) fn shell_list(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<ShellListResponseDto, InvalidRequestErrorDto> {
    let application = application.clone();
    Box::pin(async move {
        let location = locations::resolve(&application.catalog_snapshot(), &query)
            .await
            .map_err(invalid)?;
        // The route stays for the desktop renderer's route contract, which
        // lists GET /api/shell. Nothing populates a durable shell inventory,
        // so the answer is an empty scoped list and no store is read.
        let data: Vec<ShellInfoDto> = Vec::new();
        decode(json!({"location": location.wire(), "data": data})).map_err(invalid)
    })
}

pub(super) fn plugin_list(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<PluginListResponseDto, InvalidRequestErrorDto> {
    plugins::list(application, query)
}

pub(super) fn workspace_diff(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<WorkspaceDiffResponseDto, InvalidRequestErrorDto> {
    workspace_diff::response(Arc::clone(&application.catalog_snapshot()), query)
}

pub(super) fn workspace_worktree(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
    request: WorkspaceWorktreeRequestDto,
) -> ApplicationFuture<WorkspaceWorktreeResponseDto, InvalidRequestErrorDto> {
    workspace_worktree::response(application.clone(), query, request)
}

pub(super) fn hook_import(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
    request: HookImportRequestDto,
) -> ApplicationFuture<HookImportResponseDto, InvalidRequestErrorDto> {
    hook_import::request(application.clone(), query, request)
}

pub(super) fn question_request_list(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<QuestionListResponseDto, InvalidRequestErrorDto> {
    let catalog = Arc::clone(&application.catalog_snapshot());
    Box::pin(async move {
        let location = locations::resolve(&catalog, &query)
            .await
            .map_err(invalid)?;
        // Rust emits forms, not legacy question events. Do not turn forms
        // into questions or hide an invalid/unreadable location as empty.
        decode(json!({"location": location.wire(), "data": []})).map_err(invalid)
    })
}

pub(super) fn form_request_list(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<FormListResponseDto, InvalidRequestErrorDto> {
    form_catalog::list(application.clone(), query)
}

pub(super) fn command_inspect(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<CommandInspectionResponseDto, InvalidRequestErrorDto> {
    catalogs::command_response(application.clone(), query, true)
}

pub(super) fn command_list(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<CommandListResponseDto, InvalidRequestErrorDto> {
    catalogs::command_response(application.clone(), query, false)
}

pub(super) fn skill_list(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<SkillListResponseDto, InvalidRequestErrorDto> {
    catalogs::transformed_response(
        Arc::clone(&application.catalog_snapshot()),
        application.plugins_runtime(),
        query,
        catalogs::Kind::Skills,
    )
}

pub(super) fn reference_list(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<ReferenceListResponseDto, InvalidRequestErrorDto> {
    catalogs::transformed_response(
        Arc::clone(&application.catalog_snapshot()),
        application.plugins_runtime(),
        query,
        catalogs::Kind::References,
    )
}

pub(super) fn location_get(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<LocationInfoDto, InvalidRequestErrorDto> {
    let catalog = Arc::clone(&application.catalog_snapshot());
    Box::pin(async move {
        let resolved = locations::resolve(&catalog, &query)
            .await
            .map_err(invalid)?;
        decode(resolved.wire()).map_err(invalid)
    })
}

pub(super) fn project_list(
    application: &ServerApplication,
) -> ApplicationFuture<ProjectListResponseDto, InvalidRequestErrorDto> {
    ready(projects::list(application).map_err(invalid))
}

pub(super) fn project_current(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<ProjectCurrentDto, InvalidRequestErrorDto> {
    let application = application.clone();
    Box::pin(async move {
        let resolved = locations::resolve_for(&application, &query)
            .await
            .map_err(invalid)?;
        decode(json!({"id": resolved.project.as_str(), "directory": resolved.project_directory}))
            .map_err(invalid)
    })
}

pub(super) fn provider_list(
    application: &ServerApplication,
    query: ProviderLocationQueryDto,
) -> ApplicationFuture<ProviderListResponseDto, CatalogApplicationError> {
    let application = application.clone();
    Box::pin(async move {
        let (location, transformed) = resolve_transformed_catalog(application, query).await?;
        let data = transformed["providers"]
            .as_array()
            .ok_or_else(|| {
                CatalogApplicationError::InvalidRequest(invalid(
                    "plugin catalog transform removed the provider list",
                ))
            })?
            .iter()
            .map(|record| record["provider"].clone())
            .collect::<Vec<_>>();
        decode(json!({"location": location, "data": data}))
            .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))
    })
}

pub(super) fn model_list(
    application: &ServerApplication,
    query: ModelLocationQueryDto,
) -> ApplicationFuture<ModelListResponseDto, CatalogApplicationError> {
    let application = application.clone();
    Box::pin(async move {
        let (location, transformed) = resolve_transformed_catalog(application, query).await?;
        let data = transformed["providers"]
            .as_array()
            .ok_or_else(|| {
                CatalogApplicationError::InvalidRequest(invalid(
                    "plugin catalog transform removed the provider list",
                ))
            })?
            .iter()
            .flat_map(|record| record["models"].as_array().into_iter().flatten().cloned())
            .collect::<Vec<_>>();
        decode(json!({"location": location, "data": data}))
            .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))
    })
}

pub(super) fn model_default(
    application: &ServerApplication,
    query: ModelDefaultLocationQueryDto,
) -> ApplicationFuture<ModelDefaultResponseDto, CatalogApplicationError> {
    let application = application.clone();
    Box::pin(async move {
        let (location, transformed) = resolve_transformed_catalog(application, query).await?;
        let selected = transformed.get("default").cloned().unwrap_or(Value::Null);
        let provider = selected.get("providerID").and_then(Value::as_str);
        let model = selected.get("modelID").and_then(Value::as_str);
        let data = transformed["providers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|record| record["provider"]["id"].as_str() == provider)
            .flat_map(|record| record["models"].as_array().into_iter().flatten())
            .find(|item| item["id"].as_str() == model)
            .cloned()
            .unwrap_or(Value::Null);
        decode(json!({"location": location, "data": data}))
            .map_err(|error| CatalogApplicationError::InvalidRequest(invalid(error)))
    })
}

pub(super) fn agent_list(
    application: &ServerApplication,
    query: AgentLocationQueryDto,
) -> ApplicationFuture<AgentListResponseDto, InvalidRequestErrorDto> {
    let plugins = application.plugins_runtime();
    let application = application.clone();
    Box::pin(async move {
        let query = serde_json::to_value(query).map_err(invalid)?;
        let query = decode(query).map_err(invalid)?;
        let location = locations::resolve_for(&application, &query)
            .await
            .map_err(invalid)?;
        let agents = selection::agent_inventory(
            &application.catalog_snapshot(),
            plugins.service_root(),
            Path::new(&location.directory),
        )
        .map_err(invalid)?;
        let transformed = plugins
            .transform_current("agent", json!({"items": agents, "default": null}))
            .await
            .map_err(invalid)?;
        let data = transformed
            .get("items")
            .cloned()
            .ok_or_else(|| invalid("plugin agent transform removed the item list"))?;
        decode(json!({"location": location.wire(), "data": data})).map_err(invalid)
    })
}
